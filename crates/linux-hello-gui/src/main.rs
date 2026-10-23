mod system;
mod worker;

use std::path::PathBuf;
use std::time::{Duration, Instant};

use eframe::egui::{
    self, Align2, Color32, ColorImage, CornerRadius, FontId, Pos2, Rect, RichText, Sense, Stroke,
    StrokeKind, TextureHandle, TextureOptions, Vec2,
};
use linux_hello_camera::{discover_cameras, CameraInfo};
use linux_hello_core::{
    verify_face, Face, FaceEngine, Storage, UserProfile, DEFAULT_COSINE_THRESHOLD,
};

use system::{
    average_embeddings, control_systemd_service_privileged, friendly_pose_name,
    get_current_username, query_daemon_status, query_pam_status,
    remove_profile_or_label_privileged, resolve_models_dir, resolve_storage_dir,
    run_daemon_auth_test, save_profile_privileged, set_pam_mode_privileged, validate_label,
    DaemonStatus, PamStatus,
};
use worker::{CameraWorker, FramePacket, WorkerEvent};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ActiveTab {
    Enroll,
    Profiles,
    LiveTest,
    SystemPam,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EnrollMode {
    GuidedFivePose,
    CustomSinglePose,
}

struct PoseStep {
    id: &'static str,
    title: &'static str,
    prompt: &'static str,
}

const FIVE_POSES: [PoseStep; 5] = [
    PoseStep {
        id: "center",
        title: "Center",
        prompt: "Look straight ahead into the IR camera",
    },
    PoseStep {
        id: "look_left",
        title: "Turn Left",
        prompt: "Turn your head slightly to your LEFT (15-20°)",
    },
    PoseStep {
        id: "look_right",
        title: "Turn Right",
        prompt: "Turn your head slightly to your RIGHT (15-20°)",
    },
    PoseStep {
        id: "look_up",
        title: "Tilt Up",
        prompt: "Tilt your head slightly UP (10-15°)",
    },
    PoseStep {
        id: "look_down",
        title: "Tilt Down",
        prompt: "Tilt your head slightly DOWN (10-15°)",
    },
];

#[derive(Debug, Clone)]
struct StatusBanner {
    message: String,
    is_error: bool,
    created_at: Instant,
}

struct LinuxHelloGuiApp {
    active_tab: ActiveTab,
    username: String,
    models_dir: PathBuf,
    storage_dir: PathBuf,
    socket_path: PathBuf,

    // Discovered cameras & selected camera
    cameras: Vec<CameraInfo>,
    selected_camera: Option<PathBuf>,

    // Loaded user profile
    profile: Option<UserProfile>,
    confirm_delete_profile: bool,

    // Camera worker & texture
    worker: Option<CameraWorker>,
    camera_texture: Option<TextureHandle>,
    latest_frame: Option<FramePacket>,
    mirror_preview: bool,
    show_landmarks_overlay: bool,

    // Enrollment state
    enroll_mode: EnrollMode,
    enroll_replace_existing: bool,
    custom_pose_label: String,
    is_enrolling: bool,
    current_pose_idx: usize,
    pose_completed: [bool; 5],
    stable_hold_frames: usize,
    pose_missed_frames: usize,
    collected_embeddings: Vec<Vec<f32>>,
    pose_yaws: Vec<f32>,
    pose_pitches: Vec<f32>,
    center_baseline: Option<(f32, f32)>,
    working_profile: Option<UserProfile>,
    enroll_guidance: String,
    step_cooldown_until: Option<Instant>,

    // Live Test / Debugger state
    test_threshold: f32,
    test_frames_scanned: usize,
    test_faces_detected: usize,
    test_matches_count: usize,
    test_peak_score: f32,
    daemon_test_result: Option<(bool, f32, String)>,

    // System & PAM state
    daemon_status: DaemonStatus,
    pam_status: PamStatus,
    last_system_refresh: Instant,

    // Optional 5-pose calibration snapshot mode (--capture-poses <dir>)
    capture_poses_dir: Option<PathBuf>,
    capture_pose_idx: usize,
    last_capture_time: Instant,

    // Notification banner
    banner: Option<StatusBanner>,
}

impl LinuxHelloGuiApp {
    fn new(cc: &eframe::CreationContext<'_>, capture_poses_dir: Option<PathBuf>) -> Self {
        configure_visuals(&cc.egui_ctx);

        let username = get_current_username();
        let models_dir = resolve_models_dir();
        let storage_dir = resolve_storage_dir();
        let socket_path = PathBuf::from("/run/linux-hello.sock");

        let cameras: Vec<CameraInfo> = discover_cameras().into_iter().filter(|c| c.is_ir).collect();
        let selected_camera = cameras.first().map(|c| c.path.clone());

        let mut app = Self {
            active_tab: ActiveTab::Enroll,
            username,
            models_dir,
            storage_dir,
            socket_path,
            cameras,
            selected_camera,
            profile: None,
            confirm_delete_profile: false,
            worker: None,
            camera_texture: None,
            latest_frame: None,
            mirror_preview: true,
            show_landmarks_overlay: true,
            enroll_mode: EnrollMode::GuidedFivePose,
            enroll_replace_existing: false,
            custom_pose_label: "glasses".to_string(),
            is_enrolling: false,
            current_pose_idx: 0,
            pose_completed: [false; 5],
            stable_hold_frames: 0,
            pose_missed_frames: 0,
            collected_embeddings: Vec::new(),
            pose_yaws: Vec::new(),
            pose_pitches: Vec::new(),
            center_baseline: None,
            working_profile: None,
            enroll_guidance: "Click 'Start Enrollment' to begin camera capture.".to_string(),
            step_cooldown_until: None,
            test_threshold: DEFAULT_COSINE_THRESHOLD,
            test_frames_scanned: 0,
            test_faces_detected: 0,
            test_matches_count: 0,
            test_peak_score: 0.0,
            daemon_test_result: None,
            daemon_status: DaemonStatus::default(),
            pam_status: PamStatus::default(),
            last_system_refresh: Instant::now() - Duration::from_secs(10),
            capture_poses_dir,
            capture_pose_idx: 0,
            last_capture_time: Instant::now() - Duration::from_secs(5),
            banner: None,
        };

        app.reload_profile();
        app.refresh_system_status();
        if app.capture_poses_dir.is_some() {
            app.start_camera(&cc.egui_ctx);
        }
        app
    }

    fn set_banner(&mut self, msg: impl Into<String>, is_error: bool) {
        self.banner = Some(StatusBanner {
            message: msg.into(),
            is_error,
            created_at: Instant::now(),
        });
    }

    fn reload_profile(&mut self) {
        if let Ok(storage) = Storage::new(&self.storage_dir) {
            match storage.load_profile(&self.username) {
                Ok(p) => self.profile = p,
                Err(_) => self.profile = None,
            }
        }
    }

    fn refresh_system_status(&mut self) {
        self.daemon_status = query_daemon_status(&self.socket_path);
        self.pam_status = query_pam_status();
        self.last_system_refresh = Instant::now();
    }

    fn refresh_cameras(&mut self) {
        self.cameras = discover_cameras().into_iter().filter(|c| c.is_ir).collect();
        if self.selected_camera.is_none()
            || !self
                .cameras
                .iter()
                .any(|c| Some(&c.path) == self.selected_camera.as_ref())
        {
            self.selected_camera = self.cameras.first().map(|c| c.path.clone());
        }
    }

    fn start_camera(&mut self, ctx: &egui::Context) -> bool {
        self.stop_camera();
        let Some(ref dev) = self.selected_camera else {
            self.set_banner(
                "No IR camera device selected. Check the System & PAM tab.",
                true,
            );
            return false;
        };
        self.latest_frame = None;
        self.worker = Some(CameraWorker::start(
            dev.clone(),
            self.models_dir.clone(),
            ctx.clone(),
        ));
        true
    }

    fn stop_camera(&mut self) {
        if let Some(mut w) = self.worker.take() {
            w.stop();
        }
        self.is_enrolling = false;
    }

    fn start_enrollment(&mut self, ctx: &egui::Context) {
        if self.enroll_mode == EnrollMode::CustomSinglePose {
            if let Err(e) = validate_label(&self.custom_pose_label) {
                self.set_banner(e, true);
                return;
            }
        }

        if !self.start_camera(ctx) {
            return;
        }

        let base_profile = if self.enroll_replace_existing {
            UserProfile::new(self.username.clone())
        } else {
            let mut p = self
                .profile
                .clone()
                .unwrap_or_else(|| UserProfile::new(self.username.clone()));
            if self.enroll_mode == EnrollMode::GuidedFivePose
                && p.labels.iter().any(|l| l == "default")
            {
                p.enrolled_embeddings.clear();
                p.labels.clear();
            }
            p
        };

        self.working_profile = Some(base_profile);
        self.is_enrolling = true;
        self.current_pose_idx = 0;
        self.pose_completed = [false; 5];
        self.stable_hold_frames = 0;
        self.pose_missed_frames = 0;
        self.collected_embeddings.clear();
        self.pose_yaws.clear();
        self.pose_pitches.clear();
        self.center_baseline = None;
        self.step_cooldown_until = Some(Instant::now() + Duration::from_millis(1500));

        if self.enroll_mode == EnrollMode::GuidedFivePose {
            self.enroll_guidance = format!(
                "Get ready... Step 1/5 ({}): {}",
                FIVE_POSES[0].title, FIVE_POSES[0].prompt
            );
        } else {
            self.enroll_guidance = format!(
                "Capturing custom pose '{}': Look straight at the camera and hold steady.",
                self.custom_pose_label.trim()
            );
        }
    }

    fn process_enrollment_frame(&mut self, frame: &FramePacket) {
        if !self.is_enrolling {
            return;
        }

        if let Some(until) = self.step_cooldown_until {
            if Instant::now() < until {
                return;
            }
            self.step_cooldown_until = None;
        }

        let valid_face = frame.faces.iter().find(|f| f.is_valid_human_face());

        let Some(face) = valid_face else {
            self.pose_missed_frames += 1;
            if self.stable_hold_frames == 0 || self.pose_missed_frames >= 3 {
                self.stable_hold_frames = 0;
                self.collected_embeddings.clear();
                self.pose_yaws.clear();
                self.pose_pitches.clear();
                if frame.faces.iter().any(|f| !f.eyes_visible) {
                    self.enroll_guidance =
                        "Eyes appear covered or occluded — keep eyes open and visible.".to_string();
                } else {
                    self.enroll_guidance =
                        "Waiting for face... Position your face inside the circular frame."
                            .to_string();
                }
            }
            return;
        };

        match self.enroll_mode {
            EnrollMode::CustomSinglePose => {
                self.pose_missed_frames = 0;
                self.stable_hold_frames += 1;
                self.collected_embeddings.push(face.embedding.clone());

                if self.stable_hold_frames >= 3 {
                    let avg = average_embeddings(&self.collected_embeddings);
                    let lbl = self.custom_pose_label.trim().to_string();
                    if let Some(ref mut wp) = self.working_profile {
                        wp.upsert_embedding(avg, lbl.clone());
                    }
                    self.finish_and_save_enrollment();
                } else {
                    self.enroll_guidance = format!(
                        "Face locked! Hold steady ({}/3 frames)...",
                        self.stable_hold_frames
                    );
                }
            }
            EnrollMode::GuidedFivePose => {
                if self.current_pose_idx >= FIVE_POSES.len() {
                    self.finish_and_save_enrollment();
                    return;
                }
                let step = &FIVE_POSES[self.current_pose_idx];
                let (matched, guidance) =
                    face.matches_target_pose_with_baseline(step.id, self.center_baseline);
                let (yaw, pitch) = face.estimated_pose_ratios();

                if matched {
                    self.pose_missed_frames = 0;
                    self.stable_hold_frames += 1;
                    self.collected_embeddings.push(face.embedding.clone());
                    self.pose_yaws.push(yaw);
                    self.pose_pitches.push(pitch);

                    if self.stable_hold_frames >= 3 {
                        let avg = average_embeddings(&self.collected_embeddings);
                        if let Some(ref mut wp) = self.working_profile {
                            wp.upsert_embedding(avg, step.id.to_string());
                        }
                        if step.id == "center" && !self.pose_yaws.is_empty() {
                            let cy =
                                self.pose_yaws.iter().sum::<f32>() / (self.pose_yaws.len() as f32);
                            let cp = self.pose_pitches.iter().sum::<f32>()
                                / (self.pose_pitches.len() as f32);
                            self.center_baseline = Some((cy, cp));
                        }

                        self.pose_completed[self.current_pose_idx] = true;
                        self.current_pose_idx += 1;
                        self.stable_hold_frames = 0;
                        self.collected_embeddings.clear();
                        self.pose_yaws.clear();
                        self.pose_pitches.clear();

                        if self.current_pose_idx >= FIVE_POSES.len() {
                            self.finish_and_save_enrollment();
                        } else {
                            let next = &FIVE_POSES[self.current_pose_idx];
                            self.enroll_guidance = format!(
                                "✓ {} captured! Get ready for Step {}/5 ({}): {}",
                                step.title,
                                self.current_pose_idx + 1,
                                next.title,
                                next.prompt
                            );
                            self.step_cooldown_until =
                                Some(Instant::now() + Duration::from_millis(1600));
                        }
                    } else {
                        self.enroll_guidance = format!(
                            "✓ {} pose aligned! Hold still ({}/3)...",
                            step.title, self.stable_hold_frames
                        );
                    }
                } else {
                    self.pose_missed_frames += 1;
                    if self.stable_hold_frames == 0 || self.pose_missed_frames >= 2 {
                        self.stable_hold_frames = 0;
                        self.collected_embeddings.clear();
                        self.pose_yaws.clear();
                        self.pose_pitches.clear();
                        self.enroll_guidance = format!(
                            "Step {}/5 ({}): {}",
                            self.current_pose_idx + 1,
                            step.title,
                            guidance
                        );
                    }
                }
            }
        }
    }

    fn finish_and_save_enrollment(&mut self) {
        self.stop_camera();
        if let Some(wp) = self.working_profile.take() {
            match save_profile_privileged(&self.storage_dir, &wp) {
                Ok(msg) => {
                    self.enroll_guidance =
                        "Enrollment complete! Biometric profile saved.".to_string();
                    self.set_banner(msg, false);
                    self.reload_profile();
                    self.refresh_system_status();
                }
                Err(err) => {
                    self.enroll_guidance = format!("Enrollment captured, but save failed: {}", err);
                    self.set_banner(err, true);
                }
            }
        }
    }

    fn update_camera_texture(&mut self, ctx: &egui::Context, packet: &FramePacket) {
        let w = packet.width as usize;
        let h = packet.height as usize;
        if packet.gray_data.len() < w * h {
            return;
        }

        let mut pixels = Vec::with_capacity(w * h);
        for y in 0..h {
            let row_start = y * w;
            for x in 0..w {
                let src_x = if self.mirror_preview { w - 1 - x } else { x };
                let lum = packet.gray_data[row_start + src_x];
                pixels.push(Color32::from_gray(lum));
            }
        }

        let color_image = ColorImage {
            size: [w, h],
            pixels,
        };

        if let Some(ref mut tex) = self.camera_texture {
            tex.set(color_image, TextureOptions::LINEAR);
        } else {
            self.camera_texture =
                Some(ctx.load_texture("ir_camera_feed", color_image, TextureOptions::LINEAR));
        }
    }

    fn poll_worker(&mut self, ctx: &egui::Context) {
        let events = if let Some(ref w) = self.worker {
            w.poll_events()
        } else {
            return;
        };

        for ev in events {
            match ev {
                WorkerEvent::Started => {}
                WorkerEvent::Frame(packet) => {
                    self.update_camera_texture(ctx, &packet);
                    if self.active_tab == ActiveTab::Enroll && self.is_enrolling {
                        self.process_enrollment_frame(&packet);
                    } else if self.active_tab == ActiveTab::Enroll {
                        if let Some(face) = packet.faces.first() {
                            let (yaw, pitch) = face.estimated_pose_ratios();
                            let (_, pose_name) = face.classify_pose(self.center_baseline);
                            self.enroll_guidance = format!(
                                "Camera preview — Detected direction: {} (yaw: {:.2}, pitch: {:.2})",
                                pose_name, yaw, pitch
                            );
                        } else {
                            self.enroll_guidance =
                                "Camera preview — Position your face in the frame.".to_string();
                        }
                    } else if self.active_tab == ActiveTab::LiveTest {
                        self.test_frames_scanned += 1;
                        if let Some(face) = packet.faces.first() {
                            self.test_faces_detected += 1;
                            if face.is_valid_human_face() {
                                if let Some(ref prof) = self.profile {
                                    let res = verify_face(face, prof, self.test_threshold);
                                    if res.best_score > self.test_peak_score {
                                        self.test_peak_score = res.best_score;
                                    }
                                    if res.matched {
                                        self.test_matches_count += 1;
                                    }
                                }
                            }
                        }
                    }
                    self.latest_frame = Some(packet);
                }
                WorkerEvent::Error(e) => {
                    self.set_banner(e, true);
                    self.stop_camera();
                }
                WorkerEvent::Stopped => {}
            }
        }
    }

    fn draw_camera_viewfinder(&self, ui: &mut egui::Ui, desired_size: Vec2, show_pose_ring: bool) {
        let (rect, _) = ui.allocate_exact_size(desired_size, Sense::hover());
        let painter = ui.painter_at(rect);

        // Dark background
        painter.rect_filled(rect, CornerRadius::same(12), Color32::from_rgb(16, 18, 24));

        if let Some(ref tex) = self.camera_texture {
            painter.image(
                tex.id(),
                rect,
                Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)),
                Color32::WHITE,
            );
        } else {
            painter.text(
                rect.center(),
                Align2::CENTER_CENTER,
                "IR Camera Idle",
                FontId::proportional(18.0),
                Color32::from_gray(140),
            );
        }

        let live_detected_pose: Option<(&'static str, &'static str)> = self
            .latest_frame
            .as_ref()
            .and_then(|p| p.faces.first())
            .map(|f| f.classify_pose(self.center_baseline));

        // Draw face bounding box & 5 landmarks if a frame is active
        if self.show_landmarks_overlay {
            if let Some(ref packet) = self.latest_frame {
                let scale_x = rect.width() / (packet.width.max(1) as f32);
                let scale_y = rect.height() / (packet.height.max(1) as f32);

                for face in &packet.faces {
                    let (bx, by, bw, bh) = face.bbox;
                    let mapped_x = if self.mirror_preview {
                        (packet.width as f32) - bx - bw
                    } else {
                        bx
                    };
                    let face_rect = Rect::from_min_size(
                        Pos2::new(rect.left() + mapped_x * scale_x, rect.top() + by * scale_y),
                        Vec2::new(bw * scale_x, bh * scale_y),
                    );

                    let box_color = if !face.eyes_visible {
                        Color32::from_rgb(239, 68, 68) // Red: eyes occluded
                    } else if face.is_valid_human_face() {
                        Color32::from_rgb(34, 197, 94) // Green: valid human face
                    } else {
                        Color32::from_rgb(245, 158, 11) // Amber
                    };

                    painter.rect_stroke(
                        face_rect,
                        CornerRadius::same(6),
                        Stroke::new(2.0_f32, box_color),
                        StrokeKind::Outside,
                    );

                    // 5 Landmarks: R-Eye, L-Eye, Nose, R-Mouth, L-Mouth
                    for (idx, &(lx, ly)) in face.landmarks.iter().enumerate() {
                        let mx = if self.mirror_preview {
                            (packet.width as f32) - lx
                        } else {
                            lx
                        };
                        let pt = Pos2::new(rect.left() + mx * scale_x, rect.top() + ly * scale_y);
                        let lm_color = match idx {
                            0 | 1 => Color32::from_rgb(56, 189, 248), // Eyes: Cyan
                            2 => Color32::from_rgb(250, 204, 21),     // Nose: Yellow
                            _ => Color32::from_rgb(167, 139, 250),    // Mouth: Purple
                        };
                        painter.circle_filled(pt, 3.5, lm_color);
                    }
                }
            }
        }

        // Draw live detected direction badge at top of viewfinder when a face is present
        if let Some((_, pose_name)) = live_detected_pose {
            let pill_rect = Rect::from_center_size(
                Pos2::new(rect.center().x, rect.top() + 20.0),
                Vec2::new(210.0, 26.0),
            );
            painter.rect_filled(
                pill_rect,
                CornerRadius::same(13),
                Color32::from_rgba_unmultiplied(15, 23, 42, 210),
            );
            painter.text(
                pill_rect.center(),
                Align2::CENTER_CENTER,
                format!("Direction: {}", pose_name),
                FontId::proportional(13.5),
                Color32::from_rgb(56, 189, 248),
            );
        }

        // Draw Windows Hello-style circular pose guide ring during enrollment or preview
        if show_pose_ring {
            let center = rect.center();
            let radius = rect.height().min(rect.width()) * 0.36;

            let color_done = Color32::from_rgb(34, 197, 94);
            let color_active = Color32::from_rgb(56, 189, 248);
            let color_pending = Color32::from_rgba_unmultiplied(148, 163, 184, 90);

            // Base subtle guide circle
            painter.circle_stroke(center, radius, Stroke::new(2.0_f32, color_pending));

            let live_pose_idx = live_detected_pose.and_then(|(id, _)| {
                FIVE_POSES.iter().position(|p| p.id == id)
            });

            // 5 Pose Directional Indicators around the ring:
            // 0: Center (inner ring)
            // 1: Turn Left (left arc)
            // 2: Turn Right (right arc)
            // 3: Tilt Up (top arc)
            // 4: Tilt Down (bottom arc)
            let pose_color = |idx: usize| -> Color32 {
                if self.pose_completed[idx] {
                    color_done
                } else if self.is_enrolling
                    && self.enroll_mode == EnrollMode::GuidedFivePose
                    && self.current_pose_idx == idx
                {
                    if self.stable_hold_frames > 0 {
                        color_done
                    } else {
                        color_active
                    }
                } else if self.capture_poses_dir.is_some() && self.capture_pose_idx == idx {
                    color_active
                } else if !self.is_enrolling
                    && self.capture_poses_dir.is_none()
                    && live_pose_idx == Some(idx)
                {
                    color_active
                } else {
                    color_pending
                }
            };

            // Inner ring for Center pose (idx 0)
            painter.circle_stroke(center, radius * 0.86, Stroke::new(3.0_f32, pose_color(0)));

            // Directional arc segments: (pose_idx, start_deg, end_deg)
            // Note: When mirror_preview is true, user's Turn Left moves face toward left of screen (180 deg)
            let (left_idx, right_idx) = if self.mirror_preview {
                (1usize, 2usize)
            } else {
                (2usize, 1usize)
            };
            let arcs = [
                (3usize, -130.0f32, -50.0f32),  // Top: Tilt Up
                (4usize, 50.0f32, 130.0f32),    // Bottom: Tilt Down
                (left_idx, 140.0f32, 220.0f32), // Left side of viewfinder
                (right_idx, -40.0f32, 40.0f32), // Right side of viewfinder
            ];

            for (idx, start_deg, end_deg) in arcs {
                let col = pose_color(idx);
                let steps = 24;
                let mut pts = Vec::with_capacity(steps + 1);
                for s in 0..=steps {
                    let t = s as f32 / steps as f32;
                    let deg = start_deg + (end_deg - start_deg) * t;
                    let rad = deg.to_radians();
                    pts.push(Pos2::new(
                        center.x + radius * rad.cos(),
                        center.y + radius * rad.sin(),
                    ));
                }
                painter.add(egui::Shape::line(pts, Stroke::new(5.5_f32, col)));
            }

            // 3-step hold dots at bottom of viewfinder
            if self.is_enrolling {
                let dot_y = rect.bottom() - 22.0;
                for i in 0..3 {
                    let dot_x = center.x + (i as f32 - 1.0) * 18.0;
                    let filled = i < self.stable_hold_frames;
                    let dot_col = if filled {
                        Color32::from_rgb(34, 197, 94)
                    } else {
                        Color32::from_gray(90)
                    };
                    painter.circle_filled(Pos2::new(dot_x, dot_y), 5.5, dot_col);
                }
            }
        }

        // Border stroke
        painter.rect_stroke(
            rect,
            CornerRadius::same(12),
            Stroke::new(1.5_f32, Color32::from_rgb(51, 65, 85)),
            StrokeKind::Inside,
        );
    }

    fn render_enroll_tab(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        ui.columns(2, |cols| {
            // Left Column: Viewfinder & Pose Ring
            cols[0].vertical_centered(|ui| {
                let avail_w = ui.available_width().min(500.0);
                let vf_size = Vec2::new(avail_w, avail_w * (400.0 / 640.0));
                self.draw_camera_viewfinder(ui, vf_size, true);

                ui.add_space(8.0);
                // Live Guidance Banner
                egui::Frame::group(ui.style())
                    .fill(Color32::from_rgb(30, 41, 59))
                    .corner_radius(CornerRadius::same(8))
                    .inner_margin(10.0)
                    .show(ui, |ui| {
                        ui.label(
                            RichText::new(&self.enroll_guidance)
                                .size(14.5)
                                .strong()
                                .color(Color32::from_rgb(226, 232, 240)),
                        );
                    });

                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    ui.checkbox(&mut self.mirror_preview, "Mirror camera preview");
                    ui.checkbox(&mut self.show_landmarks_overlay, "Show landmarks & bbox");
                });
            });

            // Right Column: Controls & 5-Pose Progress Checklist
            cols[1].vertical(|ui| {
                ui.heading("Biometric Face Enrollment");
                ui.add_space(4.0);
                ui.label(
                    RichText::new(format!("Target User: {}", self.username))
                        .color(Color32::from_gray(180)),
                );
                ui.add_space(10.0);

                ui.horizontal(|ui| {
                    ui.selectable_value(
                        &mut self.enroll_mode,
                        EnrollMode::GuidedFivePose,
                        "Guided 5-Pose (Recommended)",
                    );
                    ui.selectable_value(
                        &mut self.enroll_mode,
                        EnrollMode::CustomSinglePose,
                        "Single Custom Pose",
                    );
                });

                ui.add_space(8.0);

                if self.enroll_mode == EnrollMode::GuidedFivePose {
                    ui.checkbox(
                        &mut self.enroll_replace_existing,
                        "Reset existing profile before enrolling",
                    );
                    ui.add_space(8.0);

                    egui::Frame::group(ui.style())
                        .corner_radius(CornerRadius::same(8))
                        .inner_margin(12.0)
                        .show(ui, |ui| {
                            ui.label(RichText::new("5-Angle Verification Checklist").strong());
                            ui.add_space(6.0);
                            for (idx, step) in FIVE_POSES.iter().enumerate() {
                                let is_done = self.pose_completed[idx];
                                let is_current = self.is_enrolling && self.current_pose_idx == idx;
                                let (icon, col) = if is_done {
                                    ("✓", Color32::from_rgb(34, 197, 94))
                                } else if is_current {
                                    ("▶", Color32::from_rgb(56, 189, 248))
                                } else {
                                    ("○", Color32::from_gray(140))
                                };
                                ui.horizontal(|ui| {
                                    ui.label(RichText::new(icon).color(col).strong());
                                    ui.label(RichText::new(step.title).color(col).strong());
                                    ui.label(
                                        RichText::new(format!("— {}", step.prompt))
                                            .size(12.5)
                                            .color(Color32::from_gray(165)),
                                    );
                                });
                                ui.add_space(3.0);
                            }
                        });
                } else {
                    egui::Frame::group(ui.style())
                        .corner_radius(CornerRadius::same(8))
                        .inner_margin(12.0)
                        .show(ui, |ui| {
                            ui.label(RichText::new("Custom Pose Label").strong());
                            ui.label(
                                RichText::new(
                                    "Add an extra appearance template (e.g. 'glasses', 'hat').",
                                )
                                .size(12.5)
                                .color(Color32::from_gray(165)),
                            );
                            ui.add_space(6.0);
                            ui.text_edit_singleline(&mut self.custom_pose_label);
                        });
                }

                ui.add_space(14.0);

                ui.horizontal(|ui| {
                    if !self.is_enrolling {
                        if ui
                            .add_sized(
                                [180.0, 36.0],
                                egui::Button::new(
                                    RichText::new("▶ Start Enrollment").size(15.0).strong(),
                                )
                                .fill(Color32::from_rgb(37, 99, 235)),
                            )
                            .clicked()
                        {
                            self.start_enrollment(ctx);
                        }

                        if self.worker.is_none() {
                            if ui
                                .add_sized([140.0, 36.0], egui::Button::new("Preview Camera"))
                                .clicked()
                            {
                                self.start_camera(ctx);
                            }
                        } else if ui
                            .add_sized([140.0, 36.0], egui::Button::new("Stop Preview"))
                            .clicked()
                        {
                            self.stop_camera();
                        }
                    } else if ui
                        .add_sized(
                            [180.0, 36.0],
                            egui::Button::new(
                                RichText::new("■ Cancel Enrollment").size(15.0).strong(),
                            )
                            .fill(Color32::from_rgb(185, 28, 28)),
                        )
                        .clicked()
                    {
                        self.stop_camera();
                        self.enroll_guidance = "Enrollment cancelled.".to_string();
                    }
                });

                // Live Pose Telemetry Box
                if let Some(ref pkt) = self.latest_frame {
                    ui.add_space(12.0);
                    egui::Frame::group(ui.style())
                        .corner_radius(CornerRadius::same(8))
                        .inner_margin(10.0)
                        .show(ui, |ui| {
                            ui.label(RichText::new("Live Sensor Telemetry").strong());
                            if let Some(face) = pkt.faces.first() {
                                let (yaw, pitch) = face.estimated_pose_ratios();
                                let (_, pose_name) = face.classify_pose(self.center_baseline);
                                ui.colored_label(
                                    Color32::from_rgb(56, 189, 248),
                                    RichText::new(format!("Detected Direction: {}", pose_name))
                                        .strong(),
                                );
                                ui.label(format!(
                                    "Confidence: {:.2} | Yaw: {:.2} | Pitch: {:.2}",
                                    face.score, yaw, pitch
                                ));
                                ui.label(format!(
                                    "Eyes Unoccluded: {} | Hold Progress: {}/3 frames",
                                    if face.eyes_visible { "YES" } else { "NO" },
                                    self.stable_hold_frames
                                ));
                            } else {
                                ui.label(format!(
                                    "No face detected | Brightness: {:.1} | {:.1} FPS",
                                    pkt.mean_brightness, pkt.fps
                                ));
                            }
                        });
                }
            });
        });
    }

    fn render_profiles_tab(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.heading(format!("Enrolled Biometric Profile: '{}'", self.username));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.button("↻ Refresh Profile").clicked() {
                    self.reload_profile();
                }
            });
        });
        ui.label(
            RichText::new(format!(
                "Storage Path: {:?}",
                self.storage_dir.join(format!("{}.json", self.username))
            ))
            .color(Color32::from_gray(160)),
        );
        ui.add_space(12.0);

        let Some(profile) = self.profile.clone() else {
            egui::Frame::group(ui.style())
                .corner_radius(CornerRadius::same(10))
                .inner_margin(20.0)
                .show(ui, |ui| {
                    ui.label(
                        RichText::new("No biometric profile is currently enrolled for this user.")
                            .size(15.0),
                    );
                    ui.add_space(8.0);
                    if ui
                        .button(RichText::new("Go to Enroll Face →").strong())
                        .clicked()
                    {
                        self.active_tab = ActiveTab::Enroll;
                    }
                });
            return;
        };

        // 5-Angle Coverage Summary
        egui::Frame::group(ui.style())
            .corner_radius(CornerRadius::same(10))
            .inner_margin(14.0)
            .show(ui, |ui| {
                ui.label(
                    RichText::new(format!(
                        "5-Angle Coverage ({} total template{} enrolled)",
                        profile.enrolled_embeddings.len(),
                        if profile.enrolled_embeddings.len() == 1 {
                            ""
                        } else {
                            "s"
                        }
                    ))
                    .size(15.0)
                    .strong(),
                );
                ui.add_space(8.0);
                ui.horizontal_wrapped(|ui| {
                    for step in &FIVE_POSES {
                        let present = profile.labels.iter().any(|l| l == step.id);
                        let bg = if present {
                            Color32::from_rgb(20, 83, 45)
                        } else {
                            Color32::from_rgb(51, 65, 85)
                        };
                        let fg = if present {
                            Color32::from_rgb(134, 239, 172)
                        } else {
                            Color32::from_gray(160)
                        };
                        egui::Frame::NONE
                            .fill(bg)
                            .corner_radius(CornerRadius::same(6))
                            .inner_margin(egui::Margin::symmetric(10, 6))
                            .show(ui, |ui| {
                                ui.label(
                                    RichText::new(format!(
                                        "{} {}",
                                        if present { "✓" } else { "○" },
                                        step.title
                                    ))
                                    .color(fg)
                                    .strong(),
                                );
                            });
                    }
                });
            });

        ui.add_space(14.0);
        ui.label(RichText::new("Enrolled Templates").size(15.0).strong());
        ui.add_space(6.0);

        let mut label_to_remove: Option<String> = None;
        egui::ScrollArea::vertical()
            .max_height(260.0)
            .show(ui, |ui| {
                for (idx, lbl) in profile.labels.iter().enumerate() {
                    let dim = profile
                        .enrolled_embeddings
                        .get(idx)
                        .map(|v| v.len())
                        .unwrap_or(0);
                    egui::Frame::group(ui.style())
                        .corner_radius(CornerRadius::same(8))
                        .inner_margin(10.0)
                        .show(ui, |ui| {
                            ui.horizontal(|ui| {
                                ui.label(
                                    RichText::new(format!("#{}", idx + 1))
                                        .color(Color32::from_gray(140)),
                                );
                                ui.label(RichText::new(friendly_pose_name(lbl)).strong().size(14.0));
                                ui.label(
                                    RichText::new(format!("[id: '{}', {}-D SFace vector]", lbl, dim))
                                        .size(12.0)
                                        .color(Color32::from_gray(150)),
                                );
                                ui.with_layout(
                                    egui::Layout::right_to_left(egui::Align::Center),
                                    |ui| {
                                        if ui.button("Remove Pose").clicked() {
                                            label_to_remove = Some(lbl.clone());
                                        }
                                    },
                                );
                            });
                        });
                    ui.add_space(4.0);
                }
            });

        if let Some(lbl) = label_to_remove {
            match remove_profile_or_label_privileged(
                &self.storage_dir,
                &self.username,
                Some(&lbl),
            ) {
                Ok(msg) => {
                    self.set_banner(msg, false);
                    self.reload_profile();
                    self.refresh_system_status();
                }
                Err(err) => self.set_banner(err, true),
            }
        }

        ui.add_space(16.0);
        ui.separator();
        ui.add_space(8.0);

        // Delete Entire Profile with in-app confirmation
        if !self.confirm_delete_profile {
            if ui
                .add(
                    egui::Button::new(
                        RichText::new("🗑 Delete Entire Biometric Profile")
                            .color(Color32::from_rgb(248, 113, 113)),
                    )
                    .fill(Color32::from_rgb(69, 10, 10)),
                )
                .clicked()
            {
                self.confirm_delete_profile = true;
            }
        } else {
            egui::Frame::group(ui.style())
                .fill(Color32::from_rgb(69, 10, 10))
                .corner_radius(CornerRadius::same(8))
                .inner_margin(10.0)
                .show(ui, |ui| {
                    ui.horizontal(|ui| {
                        ui.label(
                            RichText::new(format!(
                                "Are you sure you want to delete all enrolled face models for '{}'?",
                                self.username
                            ))
                            .strong()
                            .color(Color32::from_rgb(254, 202, 202)),
                        );
                        if ui
                            .add(
                                egui::Button::new(RichText::new("Yes, Delete All").strong())
                                    .fill(Color32::from_rgb(220, 38, 38)),
                            )
                            .clicked()
                        {
                            self.confirm_delete_profile = false;
                            match remove_profile_or_label_privileged(
                                &self.storage_dir,
                                &self.username,
                                None,
                            ) {
                                Ok(msg) => {
                                    self.set_banner(msg, false);
                                    self.reload_profile();
                                    self.refresh_system_status();
                                }
                                Err(err) => self.set_banner(err, true),
                            }
                        }
                        if ui.button("Cancel").clicked() {
                            self.confirm_delete_profile = false;
                        }
                    });
                });
        }
    }

    fn render_live_test_tab(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        ui.columns(2, |cols| {
            // Left Column: Live Camera Stream
            cols[0].vertical_centered(|ui| {
                let avail_w = ui.available_width().min(500.0);
                let vf_size = Vec2::new(avail_w, avail_w * (400.0 / 640.0));
                self.draw_camera_viewfinder(ui, vf_size, false);

                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    if self.worker.is_none() {
                        if ui
                            .add_sized(
                                [170.0, 34.0],
                                egui::Button::new(
                                    RichText::new("▶ Start Live Stream").strong(),
                                )
                                .fill(Color32::from_rgb(37, 99, 235)),
                            )
                            .clicked()
                        {
                            self.test_frames_scanned = 0;
                            self.test_faces_detected = 0;
                            self.test_matches_count = 0;
                            self.test_peak_score = 0.0;
                            self.start_camera(ctx);
                        }
                    } else if ui
                        .add_sized(
                            [170.0, 34.0],
                            egui::Button::new(RichText::new("■ Stop Live Stream").strong())
                                .fill(Color32::from_rgb(185, 28, 28)),
                        )
                        .clicked()
                    {
                        self.stop_camera();
                    }

                    if ui
                        .add_sized(
                            [180.0, 34.0],
                            egui::Button::new("⚡ Test via Daemon Socket"),
                        )
                        .clicked()
                    {
                        // Release camera first so linux-hellod can open /dev/videoX
                        self.stop_camera();
                        match run_daemon_auth_test(&self.socket_path, &self.username) {
                            Ok(res) => {
                                self.daemon_test_result = Some(res.clone());
                                self.set_banner(res.2, !res.0);
                            }
                            Err(err) => {
                                self.set_banner(err, true);
                            }
                        }
                    }
                });

                ui.add_space(4.0);
                ui.horizontal(|ui| {
                    ui.checkbox(&mut self.mirror_preview, "Mirror preview");
                    ui.checkbox(&mut self.show_landmarks_overlay, "Show landmarks & bbox");
                });
            });

            // Right Column: Similarity Gauge, Per-Pose Breakdown & Diagnostics
            cols[1].vertical(|ui| {
                ui.heading("Live Recognition & Anti-Spoofing Debugger");
                ui.add_space(8.0);

                ui.horizontal(|ui| {
                    ui.label("Cosine Similarity Threshold:");
                    ui.add(
                        egui::Slider::new(&mut self.test_threshold, 0.45..=0.85)
                            .step_by(0.01)
                            .fixed_decimals(2),
                    );
                    if ui.small_button("Reset (0.65)").clicked() {
                        self.test_threshold = DEFAULT_COSINE_THRESHOLD;
                    }
                });

                ui.add_space(8.0);

                let pkt_opt = self.latest_frame.clone();
                let face_opt: Option<Face> =
                    pkt_opt.as_ref().and_then(|p| p.faces.first().cloned());

                egui::Frame::group(ui.style())
                    .corner_radius(CornerRadius::same(8))
                    .inner_margin(12.0)
                    .show(ui, |ui| {
                        if let Some(ref face) = face_opt {
                            let (yaw, pitch) = face.estimated_pose_ratios();
                            let (_, live_pose_name) = face.classify_pose(self.center_baseline);
                            let valid_geom = face.is_valid_human_face();

                            if !face.eyes_visible {
                                ui.colored_label(
                                    Color32::from_rgb(248, 113, 113),
                                    RichText::new("⛔ REJECTED: Eyes Covered or Occluded")
                                        .size(15.0)
                                        .strong(),
                                );
                            } else if let Some(ref prof) = self.profile {
                                let match_res = verify_face(face, prof, self.test_threshold);
                                let score = match_res.best_score.max(0.0);
                                let verdict_col = if match_res.matched && valid_geom {
                                    Color32::from_rgb(34, 197, 94)
                                } else {
                                    Color32::from_rgb(248, 113, 113)
                                };
                                let verdict_text = if match_res.matched && valid_geom {
                                    format!(
                                        "✓ MATCH SUCCESS ({:.3} >= {:.2}) — Direction: {}",
                                        score,
                                        self.test_threshold,
                                        live_pose_name
                                    )
                                } else {
                                    format!(
                                        "✗ NO MATCH ({:.3} < {:.2}) — Direction: {}",
                                        score, self.test_threshold, live_pose_name
                                    )
                                };
                                ui.colored_label(
                                    verdict_col,
                                    RichText::new(verdict_text).size(15.0).strong(),
                                );
                                ui.add_space(6.0);
                                ui.add(
                                    egui::ProgressBar::new(score.clamp(0.0, 1.0))
                                        .text(format!(
                                            "Best Similarity: {:.3} (Template: {})",
                                            score,
                                            friendly_pose_name(
                                                match_res.matched_label.as_deref().unwrap_or("?")
                                            )
                                        ))
                                        .fill(verdict_col),
                                );

                                ui.add_space(10.0);
                                ui.label(RichText::new("Per-Pose Template Similarity:").strong());
                                for (idx, emb) in prof.enrolled_embeddings.iter().enumerate() {
                                    let sim =
                                        FaceEngine::cosine_similarity(&face.embedding, emb).max(0.0);
                                    let lbl =
                                        prof.labels.get(idx).map(|s| s.as_str()).unwrap_or("?");
                                    let bar_col = if sim >= self.test_threshold {
                                        Color32::from_rgb(34, 197, 94)
                                    } else {
                                        Color32::from_rgb(59, 130, 246)
                                    };
                                    ui.horizontal(|ui| {
                                        ui.set_min_width(110.0);
                                        ui.label(friendly_pose_name(lbl));
                                        ui.add(
                                            egui::ProgressBar::new(sim.clamp(0.0, 1.0))
                                                .text(format!("{:.3}", sim))
                                                .fill(bar_col),
                                        );
                                    });
                                }
                            } else {
                                ui.label(
                                    RichText::new(format!(
                                        "Face detected — Direction: {} (No enrolled profile to compare against).",
                                        live_pose_name
                                    ))
                                    .color(Color32::from_rgb(250, 204, 21)),
                                );
                            }

                            ui.add_space(10.0);
                            ui.separator();
                            ui.label(RichText::new("Anatomical & Pose Telemetry").strong());
                            ui.colored_label(
                                Color32::from_rgb(56, 189, 248),
                                RichText::new(format!(
                                    "Detected Head Direction: {} (yaw = {:.2}, pitch = {:.2})",
                                    live_pose_name, yaw, pitch
                                ))
                                .strong(),
                            );
                            ui.label(format!(
                                "Detector Score: {:.2} | BBox: [{:.0}, {:.0}, {:.0}x{:.0}]",
                                face.score, face.bbox.0, face.bbox.1, face.bbox.2, face.bbox.3
                            ));
                            ui.label(format!(
                                "Eye Contrast: R(std={:.1}, rng={:.0}) | L(std={:.1}, rng={:.0})",
                                face.eye_contrast.0,
                                face.eye_range.0,
                                face.eye_contrast.1,
                                face.eye_range.1
                            ));
                        } else {
                            ui.label("Waiting for live face in camera stream...");
                        }

                        if let Some(ref pkt) = pkt_opt {
                            ui.add_space(6.0);
                            ui.label(
                                RichText::new(format!(
                                    "Stream: {:.1} FPS | Inference: {:.1} ms | Mean Brightness: {:.1}",
                                    pkt.fps, pkt.inference_ms, pkt.mean_brightness
                                ))
                                .size(12.5)
                                .color(Color32::from_gray(160)),
                            );
                        }
                    });

                ui.add_space(8.0);
                egui::Frame::group(ui.style())
                    .corner_radius(CornerRadius::same(8))
                    .inner_margin(10.0)
                    .show(ui, |ui| {
                        ui.label(RichText::new("Session Statistics").strong());
                        let match_rate = if self.test_faces_detected > 0 {
                            (self.test_matches_count as f32 / self.test_faces_detected as f32)
                                * 100.0
                        } else {
                            0.0
                        };
                        ui.label(format!(
                            "Frames: {} | Faces: {} | Positive Matches: {} ({:.1}%) | Peak Score: {:.3}",
                            self.test_frames_scanned,
                            self.test_faces_detected,
                            self.test_matches_count,
                            match_rate,
                            self.test_peak_score
                        ));
                    });
            });
        });
    }

    fn render_system_pam_tab(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.heading("System Integration, PAM & Hardware Settings");
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.button("↻ Refresh Status").clicked() {
                    self.refresh_system_status();
                    self.refresh_cameras();
                }
            });
        });
        ui.add_space(10.0);

        ui.columns(2, |cols| {
            // Left Column: PAM Authentication Modes
            cols[0].vertical(|ui| {
                // Card 1: Sudo-Only PAM
                egui::Frame::group(ui.style())
                    .corner_radius(CornerRadius::same(10))
                    .inner_margin(14.0)
                    .show(ui, |ui| {
                        let enabled = self.pam_status.is_sudo_only_enabled();
                        ui.horizontal(|ui| {
                            ui.label(
                                RichText::new("Sudo-Only PAM Authentication")
                                    .size(15.5)
                                    .strong(),
                            );
                            let (badge_txt, badge_col) = if enabled {
                                ("● ENABLED", Color32::from_rgb(34, 197, 94))
                            } else {
                                ("○ DISABLED", Color32::from_gray(150))
                            };
                            ui.colored_label(badge_col, RichText::new(badge_txt).strong());
                        });
                        ui.add_space(4.0);
                        ui.label(
                            RichText::new(
                                "Uses a dedicated /etc/pam.d/sudo-hello service and /etc/sudoers.d/ drop-in so only 'sudo' uses face auth, leaving login/lock screens and /etc/pam.d/common-auth untouched.",
                            )
                            .size(12.5)
                            .color(Color32::from_gray(170)),
                        );
                        ui.add_space(10.0);
                        ui.horizontal(|ui| {
                            if !enabled {
                                if ui
                                    .add(
                                        egui::Button::new(
                                            RichText::new("Enable Sudo-Only PAM (Polkit)").strong(),
                                        )
                                        .fill(Color32::from_rgb(37, 99, 235)),
                                    )
                                    .clicked()
                                {
                                    match set_pam_mode_privileged(true, true) {
                                        Ok(m) => self.set_banner(m, false),
                                        Err(e) => self.set_banner(e, true),
                                    }
                                    self.refresh_system_status();
                                }
                            } else if ui
                                .button("Disable Sudo-Only PAM (Polkit)")
                                .clicked()
                            {
                                match set_pam_mode_privileged(false, true) {
                                    Ok(m) => self.set_banner(m, false),
                                    Err(e) => self.set_banner(e, true),
                                }
                                self.refresh_system_status();
                            }
                        });
                    });

                ui.add_space(12.0);

                // Card 2: System-Wide PAM
                egui::Frame::group(ui.style())
                    .corner_radius(CornerRadius::same(10))
                    .inner_margin(14.0)
                    .show(ui, |ui| {
                        let enabled = self.pam_status.is_system_wide_enabled();
                        ui.horizontal(|ui| {
                            ui.label(
                                RichText::new("System-Wide PAM (pam-auth-update)")
                                    .size(15.5)
                                    .strong(),
                            );
                            let (badge_txt, badge_col) = if enabled {
                                ("● ENABLED", Color32::from_rgb(34, 197, 94))
                            } else {
                                ("○ DISABLED", Color32::from_gray(150))
                            };
                            ui.colored_label(badge_col, RichText::new(badge_txt).strong());
                        });
                        ui.add_space(4.0);
                        ui.label(
                            RichText::new(
                                "Registers /usr/share/pam-configs/linux-hello in /etc/pam.d/common-auth for sudo, Polkit GUI prompts, lock screens, and display managers.",
                            )
                            .size(12.5)
                            .color(Color32::from_gray(170)),
                        );
                        ui.add_space(10.0);
                        ui.horizontal(|ui| {
                            if !enabled {
                                if ui
                                    .button("Enable System-Wide PAM (Polkit)")
                                    .clicked()
                                {
                                    match set_pam_mode_privileged(true, false) {
                                        Ok(m) => self.set_banner(m, false),
                                        Err(e) => self.set_banner(e, true),
                                    }
                                    self.refresh_system_status();
                                }
                            } else if ui
                                .button("Disable System-Wide PAM (Polkit)")
                                .clicked()
                            {
                                match set_pam_mode_privileged(false, false) {
                                    Ok(m) => self.set_banner(m, false),
                                    Err(e) => self.set_banner(e, true),
                                }
                                self.refresh_system_status();
                            }
                        });
                    });
            });

            // Right Column: Daemon Service & Camera Hardware
            cols[1].vertical(|ui| {
                // Daemon Status Card
                egui::Frame::group(ui.style())
                    .corner_radius(CornerRadius::same(10))
                    .inner_margin(14.0)
                    .show(ui, |ui| {
                        ui.horizontal(|ui| {
                            ui.label(
                                RichText::new("Background Daemon (linux-hellod)")
                                    .size(15.5)
                                    .strong(),
                            );
                            let (txt, col) = if self.daemon_status.daemon_reachable {
                                ("● ONLINE", Color32::from_rgb(34, 197, 94))
                            } else {
                                ("○ OFFLINE", Color32::from_rgb(245, 158, 11))
                            };
                            ui.colored_label(col, RichText::new(txt).strong());
                        });
                        ui.add_space(6.0);
                        ui.label(format!(
                            "Unix Socket: {:?} ({})",
                            self.socket_path,
                            if self.daemon_status.socket_exists {
                                "present"
                            } else {
                                "not found"
                            }
                        ));
                        ui.label(format!(
                            "Systemd Service (linux-hello.service): {}",
                            if self.daemon_status.systemd_active {
                                "active (running)"
                            } else {
                                "inactive"
                            }
                        ));
                        if self.daemon_status.daemon_reachable {
                            ui.label(format!(
                                "Daemon IR Camera: {} ({})",
                                self.daemon_status.camera_path,
                                if self.daemon_status.camera_found {
                                    "connected"
                                } else {
                                    "missing"
                                }
                            ));
                        }
                        ui.add_space(10.0);
                        ui.horizontal_wrapped(|ui| {
                            if ui.button("▶ Start / Enable Service").clicked() {
                                match control_systemd_service_privileged("enable") {
                                    Ok(m) => self.set_banner(m, false),
                                    Err(e) => self.set_banner(e, true),
                                }
                                self.refresh_system_status();
                            }
                            if ui.button("↻ Restart Service").clicked() {
                                match control_systemd_service_privileged("restart") {
                                    Ok(m) => self.set_banner(m, false),
                                    Err(e) => self.set_banner(e, true),
                                }
                                self.refresh_system_status();
                            }
                            if ui.button("■ Stop Service").clicked() {
                                match control_systemd_service_privileged("stop") {
                                    Ok(m) => self.set_banner(m, false),
                                    Err(e) => self.set_banner(e, true),
                                }
                                self.refresh_system_status();
                            }
                        });
                    });

                ui.add_space(12.0);

                // Camera Hardware Card
                egui::Frame::group(ui.style())
                    .corner_radius(CornerRadius::same(10))
                    .inner_margin(14.0)
                    .show(ui, |ui| {
                        ui.label(
                            RichText::new("Detected IR Camera Devices")
                                .size(15.5)
                                .strong(),
                        );
                        ui.add_space(6.0);
                        if self.cameras.is_empty() {
                            ui.label("No Infrared (IR) camera devices detected.");
                        } else {
                            for cam in &self.cameras.clone() {
                                let is_selected =
                                    self.selected_camera.as_ref() == Some(&cam.path);
                                ui.horizontal(|ui| {
                                    if ui
                                        .radio(
                                            is_selected,
                                            RichText::new(cam.path.to_string_lossy()).strong(),
                                        )
                                        .clicked()
                                    {
                                        self.selected_camera = Some(cam.path.clone());
                                    }
                                    ui.colored_label(
                                        Color32::from_rgb(34, 197, 94),
                                        RichText::new("[IR CAMERA]").strong(),
                                    );
                                    ui.label(
                                        RichText::new(format!(
                                            "{} ({})",
                                            cam.card,
                                            cam.formats.join(", ")
                                        ))
                                        .size(12.5)
                                        .color(Color32::from_gray(165)),
                                    );
                                });
                            }
                        }
                    });
            });
        });
    }

    fn render_capture_poses_mode(&mut self, ui: &mut egui::Ui, ctx: &egui::Context, out_dir: PathBuf) {
        let idx = self.capture_pose_idx.min(FIVE_POSES.len() - 1);
        let step = &FIVE_POSES[idx];

        let key_triggered = self.last_capture_time.elapsed() > Duration::from_millis(450)
            && ctx.input(|i| i.key_pressed(egui::Key::Space) || i.key_pressed(egui::Key::Enter));
        let mut trigger_capture = key_triggered;

        ui.columns(2, |cols| {
            cols[0].vertical_centered(|ui| {
                let avail_w = ui.available_width().min(500.0);
                let vf_size = Vec2::new(avail_w, avail_w * (400.0 / 640.0));
                self.draw_camera_viewfinder(ui, vf_size, true);

                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    ui.checkbox(&mut self.mirror_preview, "Mirror camera preview");
                    ui.checkbox(&mut self.show_landmarks_overlay, "Show landmarks & bbox");
                });
            });

            cols[1].vertical(|ui| {
                ui.heading(
                    RichText::new(format!(
                        "📸 Calibration Photo {}/5: {}",
                        idx + 1,
                        step.title.to_uppercase()
                    ))
                    .size(22.0)
                    .color(Color32::from_rgb(56, 189, 248)),
                );
                ui.add_space(8.0);

                egui::Frame::group(ui.style())
                    .fill(Color32::from_rgb(30, 41, 59))
                    .corner_radius(CornerRadius::same(10))
                    .inner_margin(14.0)
                    .show(ui, |ui| {
                        ui.label(
                            RichText::new(step.prompt)
                                .size(17.0)
                                .strong()
                                .color(Color32::WHITE),
                        );
                        ui.add_space(6.0);
                        ui.label(
                            RichText::new(
                                "Position your head in this direction and press SPACE (or click the button below) to capture the frame.",
                            )
                            .size(13.5)
                            .color(Color32::from_gray(190)),
                        );
                    });

                ui.add_space(12.0);
                egui::Frame::group(ui.style())
                    .corner_radius(CornerRadius::same(8))
                    .inner_margin(12.0)
                    .show(ui, |ui| {
                        for (i, p) in FIVE_POSES.iter().enumerate() {
                            let done = i < idx;
                            let current = i == idx;
                            let (icon, col) = if done {
                                ("✓", Color32::from_rgb(34, 197, 94))
                            } else if current {
                                ("▶", Color32::from_rgb(56, 189, 248))
                            } else {
                                ("○", Color32::from_gray(140))
                            };
                            ui.horizontal(|ui| {
                                ui.label(RichText::new(icon).color(col).strong());
                                ui.label(RichText::new(format!("{}. {}", i + 1, p.title)).color(col).strong());
                                ui.label(
                                    RichText::new(format!("— {}", p.prompt))
                                        .size(12.5)
                                        .color(Color32::from_gray(165)),
                                );
                            });
                        }
                    });

                ui.add_space(16.0);
                if ui
                    .add_sized(
                        [340.0, 46.0],
                        egui::Button::new(
                            RichText::new(format!(
                                "📸 Take Picture {}/5 ({}) [SPACE]",
                                idx + 1,
                                step.title
                            ))
                            .size(16.0)
                            .strong(),
                        )
                        .fill(Color32::from_rgb(37, 99, 235)),
                    )
                    .clicked()
                {
                    trigger_capture = true;
                }

                if let Some(ref pkt) = self.latest_frame {
                    ui.add_space(12.0);
                    if let Some(face) = pkt.faces.first() {
                        let (yaw, pitch) = face.estimated_pose_ratios();
                        ui.label(format!(
                            "Live Face: score={:.2} | yaw={:.3} | pitch={:.3} | eyes_visible={}",
                            face.score, yaw, pitch, face.eyes_visible
                        ));
                    } else {
                        ui.label(
                            RichText::new("⚠ No face currently detected in frame — adjust position before capturing")
                                .color(Color32::from_rgb(250, 204, 21)),
                        );
                    }
                }
            });
        });

        if trigger_capture {
            if let Some(ref pkt) = self.latest_frame.clone() {
                self.last_capture_time = Instant::now();
                let _ = std::fs::create_dir_all(&out_dir);
                let filename = format!("pose_{}_{}.png", idx + 1, step.id);
                let file_path = out_dir.join(&filename);
                let _ = image::save_buffer(
                    &file_path,
                    &pkt.gray_data,
                    pkt.width,
                    pkt.height,
                    image::ExtendedColorType::L8,
                );

                if let Some(face) = pkt.faces.first() {
                    let (yaw, pitch) = face.estimated_pose_ratios();
                    println!(
                        "CALIBRATION_POSE step={} id={} file={:?} score={:.3} bbox={:?} landmarks={:?} yaw={:.4} pitch={:.4} eye_contrast={:?} eye_range={:?} eyes_visible={}",
                        idx + 1,
                        step.id,
                        file_path,
                        face.score,
                        face.bbox,
                        face.landmarks,
                        yaw,
                        pitch,
                        face.eye_contrast,
                        face.eye_range,
                        face.eyes_visible
                    );
                } else {
                    println!(
                        "CALIBRATION_POSE step={} id={} file={:?} NO_FACE_DETECTED brightness={:.1}",
                        idx + 1,
                        step.id,
                        file_path,
                        pkt.mean_brightness
                    );
                }

                self.pose_completed[idx] = true;
                self.capture_pose_idx += 1;
                if self.capture_pose_idx >= FIVE_POSES.len() {
                    self.stop_camera();
                    std::process::exit(0);
                }
            }
        }
    }
}

impl eframe::App for LinuxHelloGuiApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.poll_worker(ctx);

        if let Some(out_dir) = self.capture_poses_dir.clone() {
            egui::CentralPanel::default()
                .frame(
                    egui::Frame::central_panel(&ctx.style())
                        .inner_margin(egui::Margin::symmetric(18, 16)),
                )
                .show(ctx, |ui| {
                    self.render_capture_poses_mode(ui, ctx, out_dir);
                });
            return;
        }

        if self.last_system_refresh.elapsed() > Duration::from_secs(6) {
            self.refresh_system_status();
        }

        // Top Navigation & Status Header
        egui::TopBottomPanel::top("top_header")
            .frame(
                egui::Frame::side_top_panel(&ctx.style())
                    .inner_margin(egui::Margin::symmetric(16, 10)),
            )
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    ui.label(
                        RichText::new("Linux-Hello")
                            .size(20.0)
                            .strong()
                            .color(Color32::from_rgb(56, 189, 248)),
                    );
                    ui.label(
                        RichText::new("Biometric Facial Authentication")
                            .size(13.5)
                            .color(Color32::from_gray(165)),
                    );

                    ui.add_space(16.0);

                    let prev_tab = self.active_tab;
                    ui.selectable_value(
                        &mut self.active_tab,
                        ActiveTab::Enroll,
                        "1. Enroll Face",
                    );
                    ui.selectable_value(
                        &mut self.active_tab,
                        ActiveTab::Profiles,
                        "2. Profile & Models",
                    );
                    ui.selectable_value(
                        &mut self.active_tab,
                        ActiveTab::LiveTest,
                        "3. Live Test & Debug",
                    );
                    ui.selectable_value(
                        &mut self.active_tab,
                        ActiveTab::SystemPam,
                        "4. System & PAM",
                    );

                    if self.active_tab != prev_tab {
                        // Release camera when switching tabs so /dev/videoX is never held unnecessarily
                        self.stop_camera();
                        self.reload_profile();
                        self.refresh_system_status();
                    }

                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.label(
                            RichText::new(format!("User: {}", self.username))
                                .size(13.0)
                                .color(Color32::from_gray(190)),
                        );
                    });
                });

                // Optional status toast banner
                if let Some(ref banner) = self.banner.clone() {
                    if banner.created_at.elapsed() < Duration::from_secs(8) {
                        ui.add_space(6.0);
                        let bg = if banner.is_error {
                            Color32::from_rgb(69, 10, 10)
                        } else {
                            Color32::from_rgb(20, 83, 45)
                        };
                        let fg = if banner.is_error {
                            Color32::from_rgb(254, 202, 202)
                        } else {
                            Color32::from_rgb(187, 247, 208)
                        };
                        egui::Frame::NONE
                            .fill(bg)
                            .corner_radius(CornerRadius::same(6))
                            .inner_margin(egui::Margin::symmetric(10, 6))
                            .show(ui, |ui| {
                                ui.horizontal(|ui| {
                                    ui.colored_label(fg, RichText::new(&banner.message).strong());
                                    ui.with_layout(
                                        egui::Layout::right_to_left(egui::Align::Center),
                                        |ui| {
                                            if ui.small_button("✕").clicked() {
                                                self.banner = None;
                                            }
                                        },
                                    );
                                });
                            });
                    } else {
                        self.banner = None;
                    }
                }
            });

        // Main Content Area
        egui::CentralPanel::default()
            .frame(
                egui::Frame::central_panel(&ctx.style())
                    .inner_margin(egui::Margin::symmetric(18, 14)),
            )
            .show(ctx, |ui| match self.active_tab {
                ActiveTab::Enroll => self.render_enroll_tab(ui, ctx),
                ActiveTab::Profiles => self.render_profiles_tab(ui),
                ActiveTab::LiveTest => self.render_live_test_tab(ui, ctx),
                ActiveTab::SystemPam => self.render_system_pam_tab(ui),
            });
    }
}

fn configure_visuals(ctx: &egui::Context) {
    let mut visuals = egui::Visuals::dark();
    visuals.window_corner_radius = CornerRadius::same(10);
    visuals.widgets.noninteractive.bg_fill = Color32::from_rgb(15, 23, 42);
    visuals.panel_fill = Color32::from_rgb(15, 23, 42);
    ctx.set_visuals(visuals);
}

fn main() -> eframe::Result<()> {
    env_logger::init_from_env(env_logger::Env::default().default_filter_or("info"));

    let mut capture_poses_dir: Option<PathBuf> = None;
    let args: Vec<String> = std::env::args().collect();
    let mut i = 1;
    while i < args.len() {
        if args[i] == "--capture-poses" && i + 1 < args.len() {
            capture_poses_dir = Some(PathBuf::from(&args[i + 1]));
            i += 2;
        } else {
            i += 1;
        }
    }

    let native_options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("Linux-Hello — Biometric Facial Authentication")
            .with_app_id("linux-hello-gui")
            .with_inner_size([900.0, 540.0])
            .with_min_inner_size([900.0, 540.0])
            .with_max_inner_size([900.0, 540.0])
            .with_resizable(false),
        ..Default::default()
    };

    eframe::run_native(
        "Linux-Hello",
        native_options,
        Box::new(move |cc| Ok(Box::new(LinuxHelloGuiApp::new(cc, capture_poses_dir)))),
    )
}
