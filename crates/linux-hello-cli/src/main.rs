use clap::{Parser, Subcommand};
use std::env;
use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Instant;

use linux_hello_camera::{discover_cameras, CameraStream};
use linux_hello_core::{verify_face, FaceEngine, Storage, UserProfile, DEFAULT_COSINE_THRESHOLD};

#[derive(Parser, Debug)]
#[command(name = "linux-hello", about = "Linux-Hello: Windows Hello equivalent for Linux / Debian")]
struct Cli {
    #[arg(short, long, default_value = "/run/linux-hello.sock")]
    socket: PathBuf,

    #[arg(short, long)]
    models_dir: Option<PathBuf>,

    #[arg(long)]
    storage_dir: Option<PathBuf>,

    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand, Debug)]
enum Commands {
    /// Enroll face with interactive multi-angle capture (center, left, right, up, down)
    Add {
        /// Optional specific label (if omitted, guided multi-angle enrollment is performed)
        #[arg(value_name = "LABEL")]
        label: Option<String>,

        /// Username to enroll (defaults to current user)
        #[arg(short, long)]
        user: Option<String>,

        /// Clear existing enrolled models for this user before enrolling
        #[arg(short, long)]
        replace: bool,

        /// Specific camera device to use (e.g. /dev/video2)
        #[arg(long)]
        device: Option<PathBuf>,
    },

    /// Test live face recognition against the camera
    Test {
        /// Username to test (defaults to current user)
        #[arg(short, long)]
        user: Option<String>,

        /// Recognition threshold (0.0 to 1.0)
        #[arg(short, long)]
        threshold: Option<f32>,

        /// Run direct test against camera and storage without daemon
        #[arg(short, long)]
        direct: bool,

        /// Specific camera device to use (e.g. /dev/video2)
        #[arg(long)]
        device: Option<PathBuf>,
    },

    /// Live debugging stream: inspect camera, face detection, pose ratios, and matching scores in real time
    Debug {
        /// Username to evaluate against (optional)
        #[arg(short, long)]
        user: Option<String>,

        /// Specific camera device to use (e.g. /dev/video2)
        #[arg(long)]
        device: Option<PathBuf>,

        /// Recognition threshold (0.0 to 1.0)
        #[arg(short, long)]
        threshold: Option<f32>,

        /// Duration in seconds to run live debug (0 for indefinite, default 30)
        #[arg(short, long, default_value = "30")]
        duration: u64,

        /// Analyze an offline image file (PNG) instead of live camera
        #[arg(short, long)]
        image: Option<PathBuf>,
    },

    /// Compare two face images (PNG) and compute their cosine similarity
    Compare {
        /// First image file path (PNG)
        image_a: PathBuf,

        /// Second image file path (PNG)
        image_b: PathBuf,

        /// Recognition threshold (0.0 to 1.0)
        #[arg(short, long)]
        threshold: Option<f32>,
    },

    /// Identify an offline face image (PNG) against an enrolled user profile
    Identify {
        /// Image file path (PNG)
        image: PathBuf,

        /// Username to identify (defaults to current user)
        #[arg(short, long)]
        user: Option<String>,

        /// Recognition threshold (0.0 to 1.0)
        #[arg(short, long)]
        threshold: Option<f32>,
    },

    /// List all enrolled users and their face models
    List,

    /// Remove an enrolled user profile or a specific template label
    Remove {
        /// Username to remove
        username: String,

        /// Optional specific template label to remove (e.g. 'glasses' or 'look_left')
        #[arg(short, long)]
        label: Option<String>,
    },

    /// Import and install a validated UserProfile JSON file into system storage (used by GUI via Polkit)
    ImportProfile {
        /// Target username
        #[arg(short, long)]
        user: String,

        /// Path to source profile JSON file
        #[arg(short, long)]
        file: PathBuf,
    },

    /// Capture a snapshot from the IR camera and save to a PNG file
    Snapshot {
        /// Output file path
        #[arg(short, long, default_value = "snapshot.png")]
        output: PathBuf,

        /// Specific camera device to use (e.g. /dev/video2)
        #[arg(long)]
        device: Option<PathBuf>,
    },

    /// List all detected cameras and identify the IR camera
    Cameras,

    /// Enable Linux-Hello in PAM authentication (system-wide or --sudo-only)
    Enable {
        /// Configure facial authentication only for sudo (/etc/pam.d/sudo-hello + /etc/sudoers.d/linux-hello)
        #[arg(long)]
        sudo_only: bool,
    },

    /// Disable Linux-Hello in PAM authentication (system-wide or --sudo-only)
    Disable {
        /// Remove only the sudo-only PAM configuration (/etc/pam.d/sudo-hello + /etc/sudoers.d/linux-hello)
        #[arg(long)]
        sudo_only: bool,
    },

    /// Launch the standalone Linux-Hello graphical settings & enrollment application
    Gui,
}

fn get_current_username() -> String {
    env::var("SUDO_USER")
        .or_else(|_| env::var("USER"))
        .unwrap_or_else(|_| "user".to_string())
}

fn resolve_models_dir(configured: Option<&Path>) -> PathBuf {
    if let Some(path) = configured {
        if path.exists() {
            return path.to_path_buf();
        }
    }
    if Path::new("models/face_detection_yunet.onnx").exists() {
        PathBuf::from("models")
    } else if Path::new("/usr/share/linux-hello/models/face_detection_yunet.onnx").exists() {
        PathBuf::from("/usr/share/linux-hello/models")
    } else {
        PathBuf::from("models")
    }
}

fn resolve_storage_dir(configured: Option<&Path>) -> PathBuf {
    if let Some(path) = configured {
        return path.to_path_buf();
    }
    let is_root = nix::unistd::Uid::effective().is_root();
    let sys_dir = Path::new("/var/lib/linux-hello/models");
    if is_root {
        return sys_dir.to_path_buf();
    }

    if sys_dir.exists() && sys_dir.read_dir().is_ok() {
        return sys_dir.to_path_buf();
    }
    if let Ok(home) = env::var("HOME") {
        PathBuf::from(home).join(".local/share/linux-hello/models")
    } else {
        sys_dir.to_path_buf()
    }
}

fn resolve_camera_device(device: Option<&Path>) -> Result<PathBuf, Box<dyn std::error::Error>> {
    if let Some(dev) = device {
        return Ok(dev.to_path_buf());
    }
    let cams = discover_cameras();
    if let Some(ir_cam) = cams.into_iter().find(|c| c.is_ir) {
        Ok(ir_cam.path)
    } else {
        Err("No Infrared (IR) camera detected on this system. Linux-Hello requires a dedicated IR camera for secure biometric authentication. If your IR camera is at a custom device node, specify it explicitly with --device /dev/videoX".into())
    }
}

fn describe_pose(yaw: f32, pitch: f32) -> &'static str {
    let yaw_excess = if yaw >= 0.57 {
        yaw - 0.57 + 0.01
    } else if yaw <= 0.39 {
        0.39 - yaw + 0.01
    } else {
        0.0
    };
    let pitch_excess = if pitch <= 0.44 {
        0.44 - pitch + 0.01
    } else if pitch >= 0.565 {
        pitch - 0.565 + 0.01
    } else {
        0.0
    };

    if yaw_excess == 0.0 && pitch_excess == 0.0 {
        "Facing Center"
    } else if yaw_excess >= pitch_excess {
        if yaw >= 0.57 {
            "Turned Left"
        } else {
            "Turned Right"
        }
    } else if pitch <= 0.44 {
        "Tilted Up"
    } else {
        "Tilted Down"
    }
}

fn average_embeddings(embeddings: &[Vec<f32>]) -> Vec<f32> {
    if embeddings.is_empty() {
        return Vec::new();
    }
    let dim = embeddings[0].len();
    let mut avg = vec![0.0f32; dim];
    for emb in embeddings {
        for (i, &val) in emb.iter().enumerate() {
            avg[i] += val;
        }
    }
    let n = embeddings.len() as f32;
    let mut norm_sq = 0.0f32;
    for val in &mut avg {
        *val /= n;
        norm_sq += *val * *val;
    }
    let norm = norm_sq.sqrt();
    if norm > 1e-6 {
        for val in &mut avg {
            *val /= norm;
        }
    }
    avg
}

fn load_image_grayscale(path: &Path) -> Result<(Vec<u8>, u32, u32), Box<dyn std::error::Error>> {
    let bytes = fs::read(path)
        .map_err(|e| format!("Failed to read image {:?}: {}", path, e))?;
    let img = image::load_from_memory(&bytes)
        .map_err(|e| format!("Failed to decode image {:?}: {}", path, e))?;
    let gray = img.to_luma8();
    let (w, h) = gray.dimensions();
    Ok((gray.into_raw(), w, h))
}

fn send_daemon_request(socket_path: &Path, req_json: &str) -> Option<String> {
    let mut stream = UnixStream::connect(socket_path).ok()?;
    let _ = stream.write_all(req_json.as_bytes());
    let _ = stream.write_all(b"\n");
    let _ = stream.flush();

    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    reader.read_line(&mut line).ok()?;
    Some(line)
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let cli = Cli::parse();
    let models_dir = resolve_models_dir(cli.models_dir.as_deref());
    let storage_dir = resolve_storage_dir(cli.storage_dir.as_deref());

    match cli.command {
        Commands::Cameras => {
            println!("Detecting connected video devices...");
            let cameras = discover_cameras();
            if cameras.is_empty() {
                println!("No video devices found in /dev/video*");
                return Ok(());
            }

            for cam in cameras {
                let ir_tag = if cam.is_ir { " [IR CAMERA]" } else { "" };
                println!("\nDevice: {:?}{}", cam.path, ir_tag);
                println!("  Card:    {}", cam.card);
                println!("  Driver:  {}", cam.driver);
                println!("  Formats: {}", cam.formats.join(", "));
            }
        }

        Commands::Snapshot { output, device } => {
            let cam_path = resolve_camera_device(device.as_deref())?;
            println!("Opening IR camera {:?}...", cam_path);
            let mut stream = CameraStream::open(&cam_path, 640, 400)?;

            println!("Waiting for illuminated IR frame...");
            let frame = stream.capture_illuminated_frame(15, 25.0)?;
            println!(
                "Captured frame: {}x{}, mean brightness: {:.2}",
                frame.width, frame.height, frame.mean_brightness
            );

            let gray_img = image::GrayImage::from_raw(frame.width, frame.height, frame.gray_data)
                .ok_or("Failed to create image buffer")?;
            gray_img.save(&output)?;
            println!("Saved IR snapshot to {:?}", output);
        }

        Commands::List => {
            let storage = match Storage::new(&storage_dir) {
                Ok(s) => s,
                Err(e) => {
                    eprintln!("Error accessing storage directory {:?}: {}", storage_dir, e);
                    return Err(e.into());
                }
            };
            let users = match storage.list_users() {
                Ok(u) => u,
                Err(linux_hello_core::StorageError::Io(ref e)) if e.kind() == std::io::ErrorKind::PermissionDenied => {
                    eprintln!("\n[PERMISSION DENIED] Cannot access storage directory {:?}: Permission denied.", storage_dir);
                    eprintln!("To fix this, run:\n  sudo chmod 755 {}", storage_dir.display());
                    return Ok(());
                }
                Err(e) => return Err(e.into()),
            };
            if users.is_empty() {
                println!("No users currently enrolled in Linux-Hello (storage: {:?}).", storage_dir);
                println!("Run 'linux-hello add' to enroll your face.");
                return Ok(());
            }

            println!("Enrolled users in Linux-Hello (directory: {:?}):", storage_dir);
            for u in users {
                match storage.load_profile(&u) {
                    Ok(Some(profile)) => {
                        let friendly_labels: Vec<String> = profile.labels.iter().map(|l| match l.as_str() {
                            "center" => "Center".to_string(),
                            "look_left" => "Turn Left".to_string(),
                            "look_right" => "Turn Right".to_string(),
                            "look_up" => "Tilt Up".to_string(),
                            "look_down" => "Tilt Down".to_string(),
                            "glasses" => "glasses [Legacy]".to_string(),
                            "default" => "default [Legacy]".to_string(),
                            other => other.to_string(),
                        }).collect();
                        println!(
                            "  • User: {} ({} template{}, poses/labels: [{}])",
                            profile.username,
                            profile.enrolled_embeddings.len(),
                            if profile.enrolled_embeddings.len() == 1 { "" } else { "s" },
                            friendly_labels.join(", ")
                        );
                    }
                    Ok(None) => {}
                    Err(linux_hello_core::StorageError::Io(ref e)) if e.kind() == std::io::ErrorKind::PermissionDenied => {
                        println!("  • User: {} (Permission denied - run with sudo to view details)", u);
                    }
                    Err(e) => {
                        println!("  • User: {} (Error loading profile: {})", u, e);
                    }
                }
            }
        }

        Commands::Remove { username, label } => {
            let storage = Storage::new(&storage_dir)?;
            if let Some(lbl) = label {
                match storage.load_profile(&username)? {
                    Some(mut profile) => {
                        if profile.remove_label(&lbl) {
                            if profile.enrolled_embeddings.is_empty() {
                                storage.delete_user(&username)?;
                                println!(
                                    "Removed last template '{}' and deleted empty profile for user '{}'.",
                                    lbl, username
                                );
                            } else {
                                storage.save_profile(&profile)?;
                                println!(
                                    "Successfully removed template '{}' from user '{}' ({} remaining).",
                                    lbl,
                                    username,
                                    profile.enrolled_embeddings.len()
                                );
                            }
                        } else {
                            println!("Template label '{}' not found for user '{}'.", lbl, username);
                        }
                    }
                    None => {
                        println!("User '{}' was not enrolled.", username);
                    }
                }
            } else if storage.delete_user(&username)? {
                println!("Successfully removed biometric profile for user '{}'", username);
            } else {
                println!("User '{}' was not enrolled.", username);
            }
        }

        Commands::ImportProfile { user, file } => {
            linux_hello_core::validate_username(&user)?;
            let content = fs::read_to_string(&file)
                .map_err(|e| format!("Failed to read profile file {:?}: {}", file, e))?;
            let profile: UserProfile = serde_json::from_str(&content)
                .map_err(|e| format!("Invalid profile JSON in {:?}: {}", file, e))?;
            if profile.username != user {
                return Err(format!(
                    "Profile username '{}' does not match target '--user {}'",
                    profile.username, user
                )
                .into());
            }
            if profile.enrolled_embeddings.len() != profile.labels.len() {
                return Err("Profile embeddings count does not match labels count".into());
            }
            for emb in &profile.enrolled_embeddings {
                if emb.len() != 128 {
                    return Err(format!(
                        "Invalid embedding dimension {} (expected 128)",
                        emb.len()
                    )
                    .into());
                }
            }
            let storage = Storage::new(&storage_dir)?;
            storage.save_profile(&profile)?;
            println!(
                "Successfully imported profile for '{}' ({} templates) to {:?}",
                user,
                profile.enrolled_embeddings.len(),
                storage.user_path(&user)
            );
        }

        Commands::Add { label, user, replace, device } => {
            let username = user.unwrap_or_else(get_current_username);
            let target_file = storage_dir.join(format!("{}.json", username));

            let storage = match Storage::new(&storage_dir) {
                Ok(s) => s,
                Err(e) => {
                    eprintln!("Error opening storage directory {:?}: {}", storage_dir, e);
                    if let linux_hello_core::StorageError::Io(ref io_err) = e {
                        if io_err.kind() == std::io::ErrorKind::PermissionDenied {
                            eprintln!("Permission denied. Try running with sudo or specify --storage-dir ~/.local/share/linux-hello/models");
                        }
                    }
                    return Err(e.into());
                }
            };

            let mut profile = if replace {
                println!("Resetting face profile for user '{}' (--replace)...", username);
                UserProfile::new(username.clone())
            } else {
                storage.load_profile(&username)?
                    .unwrap_or_else(|| UserProfile::new(username.clone()))
            };

            let cam_path = resolve_camera_device(device.as_deref())?;
            println!("Connecting directly to camera {:?}...", cam_path);
            let mut stream = CameraStream::open(&cam_path, 640, 400)?;
            let det_model = models_dir.join("face_detection_yunet.onnx");
            let rec_model = models_dir.join("face_recognition_sface.onnx");
            let engine = FaceEngine::new(&det_model, &rec_model, stream.width, stream.height)?;

            if let Some(custom_label) = label {
                // Single-shot enrollment with custom label
                println!("\nCapturing single angle '{}' for user '{}'...", custom_label, username);
                println!("Please look directly at the camera...");
                let start = Instant::now();
                let mut found = false;
                let mut frames_scanned = 0usize;
                let mut stable_frames = 0usize;
                let mut missed_frames = 0usize;
                let mut collected_embeddings: Vec<Vec<f32>> = Vec::new();

                while start.elapsed().as_secs() < 12 {
                    let frame = match stream.capture_frame(400) {
                        Ok(f) => f,
                        Err(_) => continue,
                    };
                    frames_scanned += 1;

                    if frame.mean_brightness < 5.0 {
                        continue;
                    }

                    if let Ok(faces) = engine.process_frame(&frame.gray_data, frame.width, frame.height) {
                        let valid_face = faces.iter().find(|f| f.is_valid_human_face()).cloned();
                        if let Some(face) = valid_face {
                            missed_frames = 0;
                            stable_frames += 1;
                            collected_embeddings.push(face.embedding.clone());
                            let meter = match stable_frames {
                                1 => "[●○○]",
                                2 => "[●●○]",
                                _ => "[●●●]",
                            };
                            print!(
                                "\r  {} Face detected! Hold steady... (confidence: {:.2})          ",
                                meter, face.score
                            );
                            let _ = std::io::stdout().flush();

                            if stable_frames >= 3 {
                                let avg_emb = average_embeddings(&collected_embeddings);
                                profile.add_embedding(avg_emb, custom_label.clone());
                                storage.save_profile(&profile)?;
                                println!(
                                    "\n\n[SUCCESS] Enrolled face! Score: {:.2}, Total models for '{}': {}",
                                    face.score, username, profile.enrolled_embeddings.len()
                                );
                                println!("Profile saved to: {:?}", target_file);
                                found = true;
                                break;
                            }
                        } else {
                            missed_frames += 1;
                            if stable_frames == 0 || missed_frames >= 3 {
                                stable_frames = 0;
                                collected_embeddings.clear();
                                let msg = if faces.iter().any(|f| !f.eyes_visible) {
                                    "(eyes covered/occluded - keep eyes open and uncovered)"
                                } else {
                                    "(looking for human face)"
                                };
                                print!(
                                    "\r  Scanning camera frames... [frame {}] {}     ",
                                    frames_scanned, msg
                                );
                                let _ = std::io::stdout().flush();
                            }
                        }
                    }
                }

                if !found {
                    println!("\n\n[FAILED] No valid human face detected and held within 12 seconds.");
                }
            } else {
                // Multi-angle guided enrollment sequence
                struct Pose {
                    id: &'static str,
                    title: &'static str,
                    prompt: &'static str,
                }

                let poses = [
                    Pose { id: "center", title: "Center", prompt: "Look straight into the IR camera" },
                    Pose { id: "look_left", title: "Turn Left", prompt: "Turn your head slightly to your LEFT (15-20°)" },
                    Pose { id: "look_right", title: "Turn Right", prompt: "Turn your head slightly to your RIGHT (15-20°)" },
                    Pose { id: "look_up", title: "Tilt Up", prompt: "Tilt your head slightly UP" },
                    Pose { id: "look_down", title: "Tilt Down", prompt: "Tilt your head slightly DOWN" },
                ];

                println!("\n=======================================================");
                println!("      Linux-Hello Multi-Angle Biometric Enrollment      ");
                println!("=======================================================");
                println!("Enrolling user: '{}'", username);
                println!("Target storage: {:?}", target_file);
                if !profile.enrolled_embeddings.is_empty() {
                    let has_legacy = profile.labels.iter().any(|l| l == "default" || l == "glasses");
                    if has_legacy && !replace {
                        println!(
                            "Notice: Found {} legacy model(s) [{}].",
                            profile.enrolled_embeddings.len(),
                            profile.labels.join(", ")
                        );
                        println!("Upgrading profile: clearing legacy models for a clean 5-pose registration...");
                        profile.enrolled_embeddings.clear();
                        profile.labels.clear();
                    } else if !replace {
                        println!(
                            "Existing models: {} (use --replace to reset completely)",
                            profile.enrolled_embeddings.len()
                        );
                    }
                }
                println!("\nWe will capture 5 verified angles with real-time pose checks:");
                for (i, p) in poses.iter().enumerate() {
                    println!("  {}. {} - {}", i + 1, p.title, p.prompt);
                }
                println!("=======================================================\n");

                let mut captured_count = 0;
                let mut center_baseline: Option<(f32, f32)> = None;

                for (idx, pose) in poses.iter().enumerate() {
                    println!("-------------------------------------------------------");
                    println!("[Step {}/{}] {}", idx + 1, poses.len(), pose.prompt);
                    println!("-------------------------------------------------------");
                    println!("Get ready in 1.2s...");
                    std::thread::sleep(std::time::Duration::from_millis(1200));

                    let start = Instant::now();
                    let mut step_done = false;
                    let mut frames_scanned = 0usize;
                    let mut stable_frames = 0usize;
                    let mut missed_frames = 0usize;
                    let mut pose_missed_frames = 0usize;
                    let mut collected_embeddings: Vec<Vec<f32>> = Vec::new();
                    let mut pose_yaws: Vec<f32> = Vec::new();
                    let mut pose_pitches: Vec<f32> = Vec::new();

                    while start.elapsed().as_secs() < 15 {
                        let frame = match stream.capture_frame(400) {
                            Ok(f) => f,
                            Err(_) => continue,
                        };
                        frames_scanned += 1;

                        if frame.mean_brightness < 5.0 {
                            continue;
                        }

                        if let Ok(faces) = engine.process_frame(&frame.gray_data, frame.width, frame.height) {
                            let valid_face = faces.iter().find(|f| f.is_valid_human_face()).cloned();
                            if let Some(face) = valid_face {
                                missed_frames = 0;
                                let (matched, guidance) = face.matches_target_pose_with_baseline(pose.id, center_baseline);
                                let (yaw, pitch) = face.estimated_pose_ratios();

                                if matched {
                                    pose_missed_frames = 0;
                                    stable_frames += 1;
                                    collected_embeddings.push(face.embedding.clone());
                                    pose_yaws.push(yaw);
                                    pose_pitches.push(pitch);
                                    let meter = match stable_frames {
                                        1 => "[●○○]",
                                        2 => "[●●○]",
                                        _ => "[●●●]",
                                    };
                                    print!(
                                        "\r  {} ✓ In target pose! Hold still... [yaw: {:.2}, pitch: {:.2}]          ",
                                        meter, yaw, pitch
                                    );
                                    let _ = std::io::stdout().flush();

                                    if stable_frames >= 3 {
                                        let avg_emb = average_embeddings(&collected_embeddings);
                                        profile.upsert_embedding(avg_emb, pose.id.to_string());
                                        if pose.id == "center" && !pose_yaws.is_empty() {
                                            let cy = pose_yaws.iter().sum::<f32>() / (pose_yaws.len() as f32);
                                            let cp = pose_pitches.iter().sum::<f32>() / (pose_pitches.len() as f32);
                                            center_baseline = Some((cy, cp));
                                        }
                                        println!(
                                            "\n  ✓ [CAPTURED] {} angle verified! (confidence: {:.2}, yaw: {:.2}, pitch: {:.2})",
                                            pose.title, face.score, yaw, pitch
                                        );
                                        captured_count += 1;
                                        step_done = true;
                                        std::thread::sleep(std::time::Duration::from_millis(1000));
                                        break;
                                    }
                                } else {
                                    pose_missed_frames += 1;
                                    if stable_frames == 0 || pose_missed_frames >= 2 {
                                        stable_frames = 0;
                                        collected_embeddings.clear();
                                        pose_yaws.clear();
                                        pose_pitches.clear();
                                        print!(
                                            "\r  [○○○] ➔ {} [yaw: {:.2}, pitch: {:.2}]              ",
                                            guidance, yaw, pitch
                                        );
                                        let _ = std::io::stdout().flush();
                                    }
                                }
                            } else {
                                missed_frames += 1;
                                if stable_frames == 0 || missed_frames >= 3 {
                                    stable_frames = 0;
                                    collected_embeddings.clear();
                                    pose_yaws.clear();
                                    pose_pitches.clear();
                                    let msg = if faces.iter().any(|f| !f.eyes_visible) {
                                        "Eyes covered or occluded! Keep eyes open and uncovered"
                                    } else {
                                        "Waiting for valid face... (keep head in camera view)"
                                    };
                                    print!(
                                        "\r  [○○○] ➔ {} [frame {}]          ",
                                        msg, frames_scanned
                                    );
                                    let _ = std::io::stdout().flush();
                                }
                            }
                        }
                    }

                    if !step_done {
                        println!("\n  ⚠ Could not verify {} angle within 15s, proceeding to next pose...", pose.title);
                    }
                    println!();
                }

                if captured_count > 0 {
                    storage.save_profile(&profile)?;
                    println!("=======================================================");
                    println!("[SUCCESS] Biometric registration complete!");
                    println!("Captured {} angle(s) for user '{}'.", captured_count, username);
                    let friendly_labels: Vec<String> = profile.labels.iter().map(|l| match l.as_str() {
                        "center" => "Center".to_string(),
                        "look_left" => "Turn Left".to_string(),
                        "look_right" => "Turn Right".to_string(),
                        "look_up" => "Tilt Up".to_string(),
                        "look_down" => "Tilt Down".to_string(),
                        other => other.to_string(),
                    }).collect();
                    println!(
                        "Total models stored: {} (angles: [{}])",
                        profile.enrolled_embeddings.len(),
                        friendly_labels.join(", ")
                    );
                    println!("Profile saved to: {:?}", target_file);
                    println!("\nTest your face authentication anytime with:");
                    println!("  linux-hello test");
                    println!("=======================================================");
                } else {
                    println!("\n[FAILED] No face angles could be captured. Please check camera positioning and run again.");
                }
            }
        }

        Commands::Test { user, threshold, direct, device } => {
            let username = user.unwrap_or_else(get_current_username);
            let thresh = threshold.unwrap_or(DEFAULT_COSINE_THRESHOLD);

            let start = Instant::now();

            // If a custom threshold or device is requested, run directly in-process.
            // Normal authentication tests against the running daemon's authoritative security policy.
            let use_direct = direct || threshold.is_some() || device.is_some();

            if !use_direct && cli.socket.exists() {
                println!("Testing facial authentication for user '{}' via daemon...", username);
                let req = format!(
                    "{{\"action\":\"authenticate\",\"username\":\"{}\",\"timeout_ms\":4000}}",
                    username
                );
                if let Some(resp) = send_daemon_request(&cli.socket, &req) {
                    if let Ok(v) = serde_json::from_str::<serde_json::Value>(&resp) {
                        let elapsed = start.elapsed().as_millis();
                        let success = v.get("success").and_then(|s| s.as_bool()).unwrap_or(false);
                        let score = v.get("score").and_then(|s| s.as_f64()).unwrap_or(0.0);
                        let msg = v["message"].as_str().unwrap_or("");

                        if success {
                            println!("\n[MATCH SUCCESS] {} in {}ms (score: {:.3})", msg, elapsed, score);
                        } else {
                            println!("\n[MATCH FAILED] {} in {}ms (score: {:.3})", msg, elapsed, score);
                        }
                        return Ok(());
                    }
                }
                println!("Daemon did not respond, falling back to direct camera test...");
            }

            println!("Testing direct facial authentication for user '{}' (threshold: {:.2})...", username, thresh);

            // Standalone direct test
            let storage = match Storage::new(&storage_dir) {
                Ok(s) => s,
                Err(e) => {
                    eprintln!("\nError opening storage directory {:?}: {}", storage_dir, e);
                    if let linux_hello_core::StorageError::Io(ref io_err) = e {
                        if io_err.kind() == std::io::ErrorKind::PermissionDenied {
                            eprintln!("Permission denied. Try running with sudo or specify --storage-dir.");
                        }
                    }
                    return Err(e.into());
                }
            };

            let profile = match storage.load_profile(&username) {
                Ok(Some(p)) => p,
                Ok(None) => {
                    let target = storage.user_path(&username);
                    println!("\n[NOT ENROLLED] User '{}' is not enrolled (no profile file at {:?}).", username, target);
                    println!("Run 'linux-hello add' to enroll your face first.");
                    return Ok(());
                }
                Err(linux_hello_core::StorageError::Io(ref e)) if e.kind() == std::io::ErrorKind::PermissionDenied => {
                    let target = storage.user_path(&username);
                    eprintln!("\n[PERMISSION DENIED] Cannot read profile at {:?}: Permission denied.", target);
                    eprintln!("To fix this for user '{}', please run:", username);
                    eprintln!("  sudo chmod 755 {}", storage_dir.display());
                    eprintln!("  sudo chmod 644 {}", target.display());
                    return Ok(());
                }
                Err(e) => return Err(e.into()),
            };

            let cam_path = resolve_camera_device(device.as_deref())?;
            let mut stream = CameraStream::open(&cam_path, 640, 400)?;
            let det_model = models_dir.join("face_detection_yunet.onnx");
            let rec_model = models_dir.join("face_recognition_sface.onnx");
            let engine = FaceEngine::new(&det_model, &rec_model, stream.width, stream.height)?;

            println!("Looking for face in camera {:?} (profile has {} enrolled templates)...", cam_path, profile.enrolled_embeddings.len());
            let mut matched = false;
            let mut best_score = -1.0f32;
            let mut frames_scanned = 0usize;

            while start.elapsed().as_millis() < 4000 {
                let frame = match stream.capture_frame(400) {
                    Ok(f) => f,
                    Err(_) => continue,
                };

                frames_scanned += 1;
                print!(
                    "\rAuthenticating... [frame {}] (brightness: {:.1}, best score: {:.3}/{:.3})",
                    frames_scanned, frame.mean_brightness, best_score.max(0.0), thresh
                );
                let _ = std::io::stdout().flush();

                if frame.mean_brightness < 5.0 {
                    continue;
                }

                if let Ok(faces) = engine.process_frame(&frame.gray_data, frame.width, frame.height) {
                    let mut face_seen_eyes_covered = false;
                    for face in faces {
                        if !face.eyes_visible {
                            face_seen_eyes_covered = true;
                            continue;
                        }
                        if !face.is_valid_human_face() {
                            continue;
                        }
                        let res = verify_face(&face, &profile, thresh);
                        if res.best_score > best_score {
                            best_score = res.best_score;
                        }
                        if res.matched {
                            let elapsed = start.elapsed().as_millis();
                            println!(
                                "\n\n[MATCH SUCCESS] Authenticated as '{}' in {}ms! (score: {:.3}, template: {:?})",
                                username, elapsed, res.best_score, res.matched_label
                            );
                            matched = true;
                            break;
                        }
                    }
                    if matched {
                        break;
                    }
                    if face_seen_eyes_covered {
                        print!(
                            "\rAuthenticating... [frame {}] (eyes covered or occluded - uncover eyes)        ",
                            frames_scanned
                        );
                        let _ = std::io::stdout().flush();
                    }
                }
            }

            if !matched {
                let elapsed = start.elapsed().as_millis();
                println!(
                    "\n\n[MATCH FAILED] Authentication timed out after {}ms (scanned {} frames, best score: {:.3}, threshold: {:.3})",
                    elapsed, frames_scanned, best_score.max(0.0), thresh
                );
            }
        }

        Commands::Debug { user, device, threshold, duration, image } => {
            let thresh = threshold.unwrap_or(DEFAULT_COSINE_THRESHOLD);
            let det_model = models_dir.join("face_detection_yunet.onnx");
            let rec_model = models_dir.join("face_recognition_sface.onnx");
            let storage = Storage::new(&storage_dir).ok();

            let profile = if let Some(ref st) = storage {
                let target_user = user.or_else(|| {
                    st.list_users().ok().and_then(|u| u.into_iter().next())
                        .or_else(|| Some(get_current_username()))
                });
                if let Some(ref u) = target_user {
                    st.load_profile(u).ok().flatten()
                } else {
                    None
                }
            } else {
                None
            };

            // Mode A: Offline Image Debug
            if let Some(img_path) = image {
                println!("\n=======================================================");
                println!("           Linux-Hello Face Debugger (Image)           ");
                println!("=======================================================");
                println!("Image file:  {:?}", img_path);
                let (gray_data, w, h) = load_image_grayscale(&img_path)?;
                println!("Dimensions:  {}x{}", w, h);

                let engine = FaceEngine::new(&det_model, &rec_model, w, h)?;
                let start = Instant::now();
                let faces = engine.process_frame(&gray_data, w, h)?;
                let det_ms = start.elapsed().as_secs_f64() * 1000.0;

                println!("Inference:   {:.2}ms | Faces detected: {}", det_ms, faces.len());
                if faces.is_empty() {
                    println!("\n[RESULT] No face detected in image.");
                }

                for (i, face) in faces.iter().enumerate() {
                    let (yaw, pitch) = face.estimated_pose_ratios();
                    let pose_desc = describe_pose(yaw, pitch);
                    println!("\n[Face #{}]", i + 1);
                    println!("  Detector Conf: {:.2}", face.score);
                    println!("  Eyes Visible:  {} (R: std={:.1}, rng={:.0} | L: std={:.1}, rng={:.0})",
                             if face.eyes_visible { "YES (unoccluded)" } else { "NO (OCCLUDED / COVERED)" },
                             face.eye_contrast.0, face.eye_range.0,
                             face.eye_contrast.1, face.eye_range.1);
                    println!("  BoundingBox:   [x: {:.1}, y: {:.1}, w: {:.1}, h: {:.1}]", face.bbox.0, face.bbox.1, face.bbox.2, face.bbox.3);
                    println!("  Pose Ratios:   yaw: {:.2}, pitch: {:.2} ({})", yaw, pitch, pose_desc);
                    println!("  Landmarks:     R-Eye ({:.1},{:.1}) | L-Eye ({:.1},{:.1}) | Nose ({:.1},{:.1})",
                             face.landmarks[0].0, face.landmarks[0].1,
                             face.landmarks[1].0, face.landmarks[1].1,
                             face.landmarks[2].0, face.landmarks[2].1);

                    if !face.eyes_visible {
                        println!("\n  Verdict:       [REJECTED - EYES OCCLUDED] Eyes are covered or occluded. Matching blocked.");
                        continue;
                    }

                    if let Some(ref p) = profile {
                        println!("\n  Comparison against '{}' ({} templates):", p.username, p.enrolled_embeddings.len());
                        let mut best_score = -1.0f32;
                        let mut best_label = "";
                        for (j, emb) in p.enrolled_embeddings.iter().enumerate() {
                            let sim = FaceEngine::cosine_similarity(&face.embedding, emb);
                            let label = p.labels.get(j).map(|s| s.as_str()).unwrap_or("unknown");
                            println!("    • Template {:<12}: similarity = {:.4}", label, sim);
                            if sim > best_score {
                                best_score = sim;
                                best_label = label;
                            }
                        }
                        println!("  Best Match:    {:.4} (template: '{}')", best_score, best_label);
                        if best_score >= thresh {
                            println!("  Verdict:       [MATCH SUCCESS] similarity {:.4} >= threshold {:.4}", best_score, thresh);
                        } else {
                            println!("  Verdict:       [NO MATCH] similarity {:.4} < threshold {:.4}", best_score, thresh);
                        }
                    }
                }
                println!("=======================================================\n");
                return Ok(());
            }

            // Mode B: Live Camera Debugger
            let cam_path = resolve_camera_device(device.as_deref())?;
            println!("\n=======================================================");
            println!("        Linux-Hello Live Face Stream Debugger          ");
            println!("=======================================================");
            println!(" Camera Device: {:?}", cam_path);
            println!(" Models Dir:    {:?}", models_dir);
            println!(" Storage Dir:   {:?}", storage_dir);
            if let Some(ref p) = profile {
                println!(" Evaluating:    '{}' ({} templates: [{}])", p.username, p.enrolled_embeddings.len(), p.labels.join(", "));
            } else {
                println!(" Evaluating:    No enrolled profile (face detection & pose only)");
            }
            println!(" Threshold:     {:.3}", thresh);
            if duration > 0 {
                println!(" Duration:      {} seconds (Press Ctrl+C to stop)", duration);
            } else {
                println!(" Duration:      Indefinite (Press Ctrl+C to stop)");
            }
            println!("=======================================================\n");

            let mut stream = CameraStream::open(&cam_path, 640, 400)?;
            let engine = FaceEngine::new(&det_model, &rec_model, stream.width, stream.height)?;

            let start_time = Instant::now();
            let mut frame_count = 0usize;
            let mut face_detected_count = 0usize;
            let mut match_count = 0usize;
            let mut peak_score = -1.0f32;

            while duration == 0 || start_time.elapsed().as_secs() < duration {
                let frame = match stream.capture_frame(400) {
                    Ok(f) => f,
                    Err(e) => {
                        eprintln!("Capture error: {}", e);
                        std::thread::sleep(std::time::Duration::from_millis(50));
                        continue;
                    }
                };
                frame_count += 1;

                let det_start = Instant::now();
                let faces = engine.process_frame(&frame.gray_data, frame.width, frame.height).unwrap_or_default();
                let det_ms = det_start.elapsed().as_secs_f64() * 1000.0;

                let elapsed_sec = start_time.elapsed().as_secs_f64();
                let fps = if elapsed_sec > 0.0 { frame_count as f64 / elapsed_sec } else { 0.0 };

                if faces.is_empty() {
                    print!(
                        "\r[{:04}] {:.1}s | FPS: {:4.1} | Bright: {:5.1} | No face detected (in {:.1}ms)          ",
                        frame_count, elapsed_sec, fps, frame.mean_brightness, det_ms
                    );
                    let _ = std::io::stdout().flush();
                } else {
                    face_detected_count += 1;
                    let face = &faces[0];
                    let (yaw, pitch) = face.estimated_pose_ratios();
                    let pose_desc = describe_pose(yaw, pitch);

                    let eye_tag = if face.eyes_visible {
                        "Eyes: OK"
                    } else {
                        "Eyes: COVERED"
                    };

                    let match_info = if !face.eyes_visible {
                        format!(" | [BLOCKED: Eyes Covered] (R:{:.1}, L:{:.1})", face.eye_contrast.0, face.eye_contrast.1)
                    } else if let Some(ref p) = profile {
                        let mut best = -1.0f32;
                        let mut best_lbl = "";
                        for (j, emb) in p.enrolled_embeddings.iter().enumerate() {
                            let s = FaceEngine::cosine_similarity(&face.embedding, emb);
                            if s > best {
                                best = s;
                                best_lbl = p.labels.get(j).map(|x| x.as_str()).unwrap_or("?");
                            }
                        }
                        if best > peak_score {
                            peak_score = best;
                        }
                        if best >= thresh {
                            match_count += 1;
                            format!(" | MATCH: {:.3} ('{}')", best, best_lbl)
                        } else {
                            format!(" | Sim: {:.3} ('{}')", best, best_lbl)
                        }
                    } else {
                        String::new()
                    };

                    println!(
                        "\r[{:04}] Bright: {:4.1} | DetConf: {:.2} | {} | Pose: yaw={:.2} pitch={:.2} ({}) ({:.1}ms){}",
                        frame_count, frame.mean_brightness, face.score,
                        eye_tag,
                        yaw, pitch, pose_desc, det_ms, match_info
                    );
                }
            }

            println!("\n\n=======================================================");
            println!("                 Debug Session Summary                 ");
            println!("=======================================================");
            println!("Total Frames Scanned:       {}", frame_count);
            println!(
                "Frames with Face:           {} ({:.1}%)",
                face_detected_count,
                if frame_count > 0 { (face_detected_count as f64 / frame_count as f64) * 100.0 } else { 0.0 }
            );
            if profile.is_some() {
                println!(
                    "Frames with Positive Match: {} ({:.1}%)",
                    match_count,
                    if face_detected_count > 0 { (match_count as f64 / face_detected_count as f64) * 100.0 } else { 0.0 }
                );
                println!("Peak Similarity Score:      {:.3}", peak_score.max(0.0));
            }
            println!("=======================================================\n");
        }

        Commands::Compare { image_a, image_b, threshold } => {
            let thresh = threshold.unwrap_or(DEFAULT_COSINE_THRESHOLD);
            let det_model = models_dir.join("face_detection_yunet.onnx");
            let rec_model = models_dir.join("face_recognition_sface.onnx");

            println!("\n=======================================================");
            println!("       Linux-Hello Face Comparison (Independent)       ");
            println!("=======================================================");
            println!("Image A:   {:?}", image_a);
            println!("Image B:   {:?}", image_b);
            println!("Threshold: {:.3}\n", thresh);

            let (gray_a, w_a, h_a) = load_image_grayscale(&image_a)?;
            let (gray_b, w_b, h_b) = load_image_grayscale(&image_b)?;

            let engine = FaceEngine::new(&det_model, &rec_model, w_a, h_a)?;

            let faces_a = engine.process_frame(&gray_a, w_a, h_a)?;
            if faces_a.is_empty() {
                eprintln!("[ERROR] No face detected in Image A ({:?})", image_a);
                return Ok(());
            }
            let face_a = &faces_a[0];
            let (yaw_a, pitch_a) = face_a.estimated_pose_ratios();
            println!("Image A ({}x{}):", w_a, h_a);
            println!("  Detector Conf: {:.2}", face_a.score);
            println!("  Eyes Visible:  {}", if face_a.eyes_visible { "YES" } else { "NO (OCCLUDED / COVERED)" });
            println!("  BBox:          [x: {:.1}, y: {:.1}, w: {:.1}, h: {:.1}]", face_a.bbox.0, face_a.bbox.1, face_a.bbox.2, face_a.bbox.3);
            println!("  Pose:          yaw: {:.2}, pitch: {:.2} ({})", yaw_a, pitch_a, describe_pose(yaw_a, pitch_a));

            let faces_b = engine.process_frame(&gray_b, w_b, h_b)?;
            if faces_b.is_empty() {
                eprintln!("\n[ERROR] No face detected in Image B ({:?})", image_b);
                return Ok(());
            }
            let face_b = &faces_b[0];
            let (yaw_b, pitch_b) = face_b.estimated_pose_ratios();
            println!("\nImage B ({}x{}):", w_b, h_b);
            println!("  Detector Conf: {:.2}", face_b.score);
            println!("  Eyes Visible:  {}", if face_b.eyes_visible { "YES" } else { "NO (OCCLUDED / COVERED)" });
            println!("  BBox:          [x: {:.1}, y: {:.1}, w: {:.1}, h: {:.1}]", face_b.bbox.0, face_b.bbox.1, face_b.bbox.2, face_b.bbox.3);
            println!("  Pose:          yaw: {:.2}, pitch: {:.2} ({})", yaw_b, pitch_b, describe_pose(yaw_b, pitch_b));

            let sim = FaceEngine::cosine_similarity(&face_a.embedding, &face_b.embedding);

            println!("\n-------------------------------------------------------");
            println!("Cosine Similarity: {:.4}", sim);
            println!("Threshold:         {:.4}", thresh);
            if sim >= thresh {
                println!("Verdict:           [MATCH SUCCESS] The two images match the SAME person!");
            } else {
                println!("Verdict:           [MATCH FAILED] The two images are DIFFERENT persons (or too dissimilar).");
            }
            println!("=======================================================\n");
        }

        Commands::Identify { image, user, threshold } => {
            let username = user.unwrap_or_else(get_current_username);
            let thresh = threshold.unwrap_or(DEFAULT_COSINE_THRESHOLD);
            let det_model = models_dir.join("face_detection_yunet.onnx");
            let rec_model = models_dir.join("face_recognition_sface.onnx");
            let storage = Storage::new(&storage_dir)?;

            let profile = match storage.load_profile(&username)? {
                Some(p) => p,
                None => {
                    println!("User '{}' is not enrolled in storage ({:?}).", username, storage_dir);
                    return Ok(());
                }
            };

            println!("\n=======================================================");
            println!("        Linux-Hello Offline Face Identification        ");
            println!("=======================================================");
            println!("Target Image: {:?}", image);
            println!("User Profile: '{}' ({} templates in storage)", username, profile.enrolled_embeddings.len());
            println!("Threshold:    {:.3}\n", thresh);

            let (gray, w, h) = load_image_grayscale(&image)?;
            let engine = FaceEngine::new(&det_model, &rec_model, w, h)?;
            let faces = engine.process_frame(&gray, w, h)?;

            if faces.is_empty() {
                println!("[FAILED] No face detected in image {:?}.", image);
                return Ok(());
            }

            let face = &faces[0];
            let (yaw, pitch) = face.estimated_pose_ratios();
            println!("Detected Face in Image:");
            println!("  Detector Conf: {:.2}", face.score);
            println!("  Eyes Visible:  {} (R: std={:.1}, L: std={:.1})",
                     if face.eyes_visible { "YES" } else { "NO (OCCLUDED / COVERED)" },
                     face.eye_contrast.0, face.eye_contrast.1);
            println!("  BBox:          [x: {:.1}, y: {:.1}, w: {:.1}, h: {:.1}]", face.bbox.0, face.bbox.1, face.bbox.2, face.bbox.3);
            println!("  Pose:          yaw: {:.2}, pitch: {:.2} ({})", yaw, pitch, describe_pose(yaw, pitch));

            if !face.eyes_visible {
                println!("\n-------------------------------------------------------");
                println!("Verdict:          [AUTH FAILED] Face eyes are occluded or covered. Matching blocked.");
                println!("=======================================================\n");
                return Ok(());
            }

            println!("\nSimilarity with Enrolled Templates:");
            let mut best_score = -1.0f32;
            let mut best_label = "";
            for (i, emb) in profile.enrolled_embeddings.iter().enumerate() {
                let sim = FaceEngine::cosine_similarity(&face.embedding, emb);
                let lbl = profile.labels.get(i).map(|s| s.as_str()).unwrap_or("unknown");
                println!("  • {:<12}: {:.4}", lbl, sim);
                if sim > best_score {
                    best_score = sim;
                    best_label = lbl;
                }
            }

            println!("\n-------------------------------------------------------");
            println!("Best Match Score: {:.4} (template '{}')", best_score, best_label);
            if best_score >= thresh {
                println!("Verdict:          [AUTH SUCCESS] Face identified as '{}'!", username);
            } else {
                println!("Verdict:          [AUTH FAILED] Face does not match '{}' (best: {:.4} < {:.4})", username, best_score, thresh);
            }
            println!("=======================================================\n");
        }

        Commands::Enable { sudo_only } => {
            if sudo_only {
                println!("Configuring Sudo-Only PAM integration for Linux-Hello...");
                let base_sudo_pam = fs::read_to_string("/etc/pam.d/sudo").unwrap_or_else(|_| {
                    if Path::new("/etc/pam.d/system-auth").exists() {
                        "#%PAM-1.0\nauth       include      system-auth\naccount    include      system-auth\npassword   include      system-auth\nsession    include      system-auth\n".to_string()
                    } else {
                        "#%PAM-1.0\n@include common-auth\n@include common-account\n@include common-session-noninteractive\n".to_string()
                    }
                });
                let sudo_hello_content = if base_sudo_pam.contains("pam_linux_hello.so") {
                    base_sudo_pam
                } else if let Some(rest) = base_sudo_pam.strip_prefix("#%PAM-1.0\n") {
                    format!("#%PAM-1.0\nauth    sufficient    pam_linux_hello.so\n{}", rest)
                } else {
                    format!("#%PAM-1.0\nauth    sufficient    pam_linux_hello.so\n{}", base_sudo_pam)
                };

                let pam_target = "/etc/pam.d/sudo-hello";
                fs::write(pam_target, sudo_hello_content)
                    .map_err(|e| format!("Failed to write {}: {} (run with sudo/pkexec)", pam_target, e))?;

                let sudoers_target = "/etc/sudoers.d/linux-hello";
                fs::write(sudoers_target, "Defaults pam_service=\"sudo-hello\"\n")
                    .map_err(|e| format!("Failed to write {}: {} (run with sudo/pkexec)", sudoers_target, e))?;
                use std::os::unix::fs::PermissionsExt;
                let _ = fs::set_permissions(sudoers_target, fs::Permissions::from_mode(0o440));

                println!("[SUCCESS] Sudo-Only facial authentication enabled ({} + {}).", pam_target, sudoers_target);
            } else {
                let has_authselect = Command::new("which")
                    .arg("authselect")
                    .output()
                    .map(|o| o.status.success())
                    .unwrap_or(false)
                    || Path::new("/usr/bin/authselect").exists();

                let has_pam_auth_update = Command::new("which")
                    .arg("pam-auth-update")
                    .output()
                    .map(|o| o.status.success())
                    .unwrap_or(false)
                    || Path::new("/usr/share/pam-configs").exists();

                if has_authselect {
                    enable_system_wide_authselect()?;
                } else if has_pam_auth_update {
                    println!("Configuring system-wide Debian PAM integration for Linux-Hello...");
                    let pam_config = r#"Name: Linux-Hello face authentication
Default: yes
Priority: 260
Auth-Type: Primary
Auth:
	[success=end default=ignore] pam_linux_hello.so
"#;
                    let config_path = "/usr/share/pam-configs/linux-hello";
                    println!("Writing PAM profile to {}...", config_path);
                    match fs::write(config_path, pam_config) {
                        Ok(_) => {
                            println!("Running pam-auth-update to enable Linux-Hello across sudo and login...");
                            let status = Command::new("pam-auth-update")
                                .arg("--package")
                                .status()?;
                            if status.success() {
                                println!("\n[SUCCESS] Linux-Hello is now enabled for system authentication!");
                            } else {
                                println!("\n[WARNING] pam-auth-update exited with status: {}", status);
                            }
                        }
                        Err(e) => {
                            eprintln!("\n[ERROR] Failed to write {}: {}. Run with sudo.", config_path, e);
                            return Err(e.into());
                        }
                    }
                } else {
                    println!("[WARNING] Neither 'authselect' (Fedora/RHEL) nor 'pam-auth-update' (Debian/Ubuntu) was found.");
                    println!("To enable Linux-Hello manually, add 'auth sufficient pam_linux_hello.so' to your PAM configuration (e.g. /etc/pam.d/system-auth or /etc/pam.d/common-auth).");
                }
            }
        }

        Commands::Disable { sudo_only } => {
            if sudo_only {
                let sudoers_target = "/etc/sudoers.d/linux-hello";
                let pam_target = "/etc/pam.d/sudo-hello";
                let mut removed = false;
                if Path::new(sudoers_target).exists() {
                    fs::remove_file(sudoers_target)
                        .map_err(|e| format!("Failed to remove {}: {}", sudoers_target, e))?;
                    removed = true;
                }
                if Path::new(pam_target).exists() {
                    fs::remove_file(pam_target)
                        .map_err(|e| format!("Failed to remove {}: {}", pam_target, e))?;
                    removed = true;
                }
                if removed {
                    println!("[SUCCESS] Sudo-Only Linux-Hello configuration has been disabled.");
                } else {
                    println!("Sudo-Only Linux-Hello configuration was not installed.");
                }
            } else {
                let custom_authselect_dir = Path::new("/etc/authselect/custom/linux-hello");
                let has_authselect = Command::new("which")
                    .arg("authselect")
                    .output()
                    .map(|o| o.status.success())
                    .unwrap_or(false)
                    || Path::new("/usr/bin/authselect").exists()
                    || custom_authselect_dir.exists();

                let config_path = "/usr/share/pam-configs/linux-hello";

                if custom_authselect_dir.exists() || (has_authselect && !Path::new(config_path).exists()) {
                    disable_system_wide_authselect()?;
                } else if Path::new(config_path).exists() {
                    println!("Removing {}...", config_path);
                    match fs::remove_file(config_path) {
                        Ok(_) => {
                            println!("Running pam-auth-update to apply changes...");
                            let _ = Command::new("pam-auth-update")
                                .arg("--package")
                                .status();
                            println!("\n[SUCCESS] Linux-Hello system-wide PAM has been disabled.");
                        }
                        Err(e) => {
                            eprintln!("\n[ERROR] Failed to remove {}: {}. Run with sudo.", config_path, e);
                            return Err(e.into());
                        }
                    }
                } else {
                    println!("Linux-Hello system-wide PAM configuration is not currently installed.");
                }
            }
        }

        Commands::Gui => {
            let gui_bin = env::current_exe()
                .ok()
                .and_then(|p| p.parent().map(|dir| dir.join("linux-hello-gui")))
                .filter(|p| p.exists())
                .unwrap_or_else(|| PathBuf::from("linux-hello-gui"));

            let status = Command::new(&gui_bin).status().map_err(|e| {
                format!(
                    "Failed to launch {:?}: {}. Make sure 'linux-hello-gui' is built and installed.",
                    gui_bin, e
                )
            })?;
            if !status.success() {
                return Err(format!("linux-hello-gui exited with status: {}", status).into());
            }
        }
    }

    Ok(())
}

fn enable_system_wide_authselect() -> Result<(), Box<dyn std::error::Error>> {
    println!("Configuring system-wide authselect PAM integration for Linux-Hello (Fedora/RHEL)...");

    // 1. Query current authselect configuration
    let current_output = Command::new("authselect")
        .arg("current")
        .output()
        .map_err(|e| format!("Failed to run 'authselect current': {}. Is authselect installed?", e))?;

    let current_str = String::from_utf8_lossy(&current_output.stdout);

    // Extract profile ID from "Profile ID: <id>"
    let mut base_profile = "sssd".to_string();
    let mut features: Vec<String> = Vec::new();

    for line in current_str.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("Profile ID:") {
            let prof = trimmed.trim_start_matches("Profile ID:").trim();
            // If already custom/linux-hello, read base profile if recorded
            if prof.starts_with("custom/") {
                let recorded_base = Path::new("/etc/authselect/custom/linux-hello/.base-profile");
                if let Ok(b) = fs::read_to_string(recorded_base) {
                    let b = b.trim();
                    if !b.is_empty() {
                        base_profile = b.to_string();
                    }
                }
            } else if !prof.is_empty() {
                base_profile = prof.to_string();
            }
        } else if trimmed.starts_with("- with-") || trimmed.starts_with("- without-") {
            let feat = trimmed.trim_start_matches("- ").trim();
            if !feat.is_empty() {
                features.push(feat.to_string());
            }
        }
    }

    let custom_dir = Path::new("/etc/authselect/custom/linux-hello");
    if !custom_dir.exists() {
        println!("Creating custom authselect profile 'linux-hello' based on '{}'...", base_profile);
        let create_status = Command::new("authselect")
            .args(["create-profile", "linux-hello", "-b", &base_profile, "--symlink-meta"])
            .status()
            .map_err(|e| format!("Failed to create authselect profile: {}. Run with sudo.", e))?;

        if !create_status.success() {
            return Err(format!("'authselect create-profile' exited with code {:?}", create_status.code()).into());
        }

        // Record the base profile for clean rollback
        let _ = fs::write(custom_dir.join(".base-profile"), format!("{}\n", base_profile));
    }

    // Helper to insert pam_linux_hello.so into an authselect pam file
    let inject_pam = |filename: &str| -> Result<(), Box<dyn std::error::Error>> {
        let file_path = custom_dir.join(filename);
        if !file_path.exists() {
            return Ok(());
        }
        let content = fs::read_to_string(&file_path)?;
        if content.contains("pam_linux_hello.so") {
            return Ok(());
        }

        let mut lines: Vec<String> = content.lines().map(|s| s.to_string()).collect();
        let hello_line = "auth        sufficient    pam_linux_hello.so".to_string();

        // Search for insertion point: before first auth sufficient/required pam_unix.so or pam_fprintd.so or with-fingerprint
        let mut insert_idx = None;
        for (i, line) in lines.iter().enumerate() {
            let trimmed = line.trim();
            if trimmed.starts_with("auth") && (trimmed.contains("pam_unix.so") || trimmed.contains("pam_fprintd.so") || trimmed.contains("pam_sss.so") || trimmed.contains("pam_usermode.so")) {
                insert_idx = Some(i);
                break;
            }
        }

        if let Some(idx) = insert_idx {
            lines.insert(idx, hello_line);
        } else {
            // Append after header or first auth line
            lines.push(hello_line);
        }

        let mut new_content = lines.join("\n");
        new_content.push('\n');
        fs::write(&file_path, new_content)?;
        Ok(())
    };

    println!("Injecting pam_linux_hello.so into authselect profile templates...");
    inject_pam("system-auth")?;
    inject_pam("password-auth")?;

    println!("Activating custom/linux-hello profile with authselect...");
    let mut select_cmd = Command::new("authselect");
    select_cmd.args(["select", "custom/linux-hello"]);
    for feat in &features {
        select_cmd.arg(feat);
    }
    select_cmd.arg("--force");

    let status = select_cmd.status().map_err(|e| format!("Failed to apply authselect profile: {}", e))?;
    if status.success() {
        println!("\n[SUCCESS] Linux-Hello is now enabled system-wide via authselect!");
        Ok(())
    } else {
        Err(format!("'authselect select' exited with status: {}", status).into())
    }
}

fn disable_system_wide_authselect() -> Result<(), Box<dyn std::error::Error>> {
    println!("Disabling authselect PAM integration for Linux-Hello...");

    // Query features and active profile
    let current_output = Command::new("authselect")
        .arg("current")
        .output()
        .map_err(|e| format!("Failed to run 'authselect current': {}", e))?;

    let current_str = String::from_utf8_lossy(&current_output.stdout);
    let mut features: Vec<String> = Vec::new();
    for line in current_str.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("- with-") || trimmed.starts_with("- without-") {
            let feat = trimmed.trim_start_matches("- ").trim();
            if !feat.is_empty() {
                features.push(feat.to_string());
            }
        }
    }

    let custom_dir = Path::new("/etc/authselect/custom/linux-hello");
    let base_profile = if let Ok(b) = fs::read_to_string(custom_dir.join(".base-profile")) {
        let b = b.trim().to_string();
        if b.is_empty() { "sssd".to_string() } else { b }
    } else {
        "sssd".to_string()
    };

    println!("Reverting authselect profile to '{}'...", base_profile);
    let mut select_cmd = Command::new("authselect");
    select_cmd.args(["select", &base_profile]);
    for feat in &features {
        select_cmd.arg(feat);
    }
    select_cmd.arg("--force");

    let status = select_cmd.status().map_err(|e| format!("Failed to revert authselect profile: {}", e))?;
    if !status.success() {
        eprintln!("[WARNING] authselect revert exited with status: {}", status);
    }

    if custom_dir.exists() {
        println!("Cleaning up {}...", custom_dir.display());
        let _ = fs::remove_dir_all(custom_dir);
    }

    println!("\n[SUCCESS] Linux-Hello system-wide authselect profile has been disabled.");
    Ok(())
}
