use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use linux_hello_camera::{discover_cameras, CameraStream};
use linux_hello_core::{verify_face, FaceEngine, Storage, UserProfile, DEFAULT_COSINE_THRESHOLD};

use crate::protocol::{Request, Response};

pub struct HelloService {
    engine: FaceEngine,
    storage: Storage,
    camera_path: PathBuf,
    threshold: f32,
    lock: Mutex<()>,
}

impl HelloService {
    pub fn new(
        models_dir: &Path,
        storage_dir: &Path,
        preferred_cam: Option<&Path>,
        threshold: Option<f32>,
    ) -> Result<Self, Box<dyn std::error::Error>> {
        let det_model = models_dir.join("face_detection_yunet.onnx");
        let rec_model = models_dir.join("face_recognition_sface.onnx");

        let engine = FaceEngine::new(&det_model, &rec_model, 640, 400)?;
        let storage = Storage::new(storage_dir)?;

        // Discover camera: require a genuine IR camera unless explicitly specified
        let camera_path = if let Some(p) = preferred_cam {
            p.to_path_buf()
        } else {
            let cams = discover_cameras();
            cams.into_iter()
                .find(|c| c.is_ir)
                .map(|c| c.path)
                .ok_or("No Infrared (IR) camera detected on this system. Linux-Hello requires a dedicated IR camera for secure biometric authentication.")?
        };

        Ok(HelloService {
            engine,
            storage,
            camera_path,
            threshold: threshold.unwrap_or(DEFAULT_COSINE_THRESHOLD),
            lock: Mutex::new(()),
        })
    }

    pub fn handle_request(&self, request: Request) -> Response {
        let _guard = match self.lock.lock() {
            Ok(g) => g,
            Err(_) => {
                return Response::Error {
                    message: "Internal mutex poisoned".into(),
                }
            }
        };

        match request {
            Request::Authenticate { username, timeout_ms } => {
                self.authenticate(&username, timeout_ms)
            }
            Request::Enroll { username, label } => {
                self.enroll(&username, &label)
            }
            Request::Status => {
                self.status()
            }
        }
    }

    fn authenticate(&self, username: &str, timeout_ms: u64) -> Response {
        let profile = match self.storage.load_profile(username) {
            Ok(Some(p)) => p,
            Ok(None) => {
                return Response::AuthResult {
                    success: false,
                    score: 0.0,
                    message: format!("User '{}' is not enrolled in linux-hello (expected {:?})", username, self.storage.user_path(username)),
                };
            }
            Err(e) => {
                return Response::Error {
                    message: format!("Failed to load profile for '{}': {}", username, e),
                };
            }
        };

        let mut stream = match CameraStream::open(&self.camera_path, 640, 400) {
            Ok(s) => s,
            Err(e) => {
                return Response::Error {
                    message: format!("Failed to open camera {:?}: {}", self.camera_path, e),
                };
            }
        };

        let start_time = Instant::now();
        let timeout = Duration::from_millis(timeout_ms);
        let mut best_score = -1.0f32;

        while start_time.elapsed() < timeout {
            // Read frame
            let frame = match stream.capture_frame(400) {
                Ok(f) => f,
                Err(_) => continue,
            };

            // Only skip pitch black or uninitialized frames
            if frame.mean_brightness < 5.0 {
                continue;
            }

            let faces = match self.engine.process_frame(&frame.gray_data, frame.width, frame.height) {
                Ok(f) => f,
                Err(_) => continue,
            };

            for face in faces {
                if !face.is_valid_human_face() {
                    continue;
                }

                let result = verify_face(&face, &profile, self.threshold);
                if result.best_score > best_score {
                    best_score = result.best_score;
                }

                if result.matched {
                    return Response::AuthResult {
                        success: true,
                        score: result.best_score,
                        message: format!(
                            "Authentication successful (matched: {:?}, score: {:.3})",
                            result.matched_label.unwrap_or_else(|| "default".into()),
                            result.best_score
                        ),
                    };
                }
            }
        }

        Response::AuthResult {
            success: false,
            score: best_score.max(0.0),
            message: format!(
                "Authentication timed out (best confidence score: {:.3}, threshold: {:.3})",
                best_score.max(0.0),
                self.threshold
            ),
        }
    }

    fn enroll(&self, username: &str, label: &str) -> Response {
        let mut stream = match CameraStream::open(&self.camera_path, 640, 400) {
            Ok(s) => s,
            Err(e) => {
                return Response::Error {
                    message: format!("Failed to open camera {:?}: {}", self.camera_path, e),
                };
            }
        };

        let start_time = Instant::now();
        let timeout = Duration::from_secs(6);

        while start_time.elapsed() < timeout {
            let frame = match stream.capture_frame(400) {
                Ok(f) => f,
                Err(_) => continue,
            };

            if frame.mean_brightness < 5.0 {
                continue;
            }

            let faces = match self.engine.process_frame(&frame.gray_data, frame.width, frame.height) {
                Ok(f) => f,
                Err(_) => continue,
            };

            let valid_face = faces.into_iter().find(|f| f.is_valid_human_face());
            if let Some(face) = valid_face {
                let mut profile = self
                    .storage
                    .load_profile(username)
                    .unwrap_or(None)
                    .unwrap_or_else(|| UserProfile::new(username.to_string()));

                profile.add_embedding(face.embedding.clone(), label.to_string());
                if let Err(e) = self.storage.save_profile(&profile) {
                    return Response::Error {
                        message: format!("Failed to save profile: {}", e),
                    };
                }

                return Response::EnrollResult {
                    success: true,
                    score: face.score,
                    total_enrolled: profile.enrolled_embeddings.len(),
                    message: format!(
                        "Successfully enrolled face for user '{}' (label: '{}', total models: {})",
                        username,
                        label,
                        profile.enrolled_embeddings.len()
                    ),
                };
            }
        }

        Response::EnrollResult {
            success: false,
            score: 0.0,
            total_enrolled: 0,
            message: "No face detected in IR camera stream during enrollment. Ensure you are directly facing the camera.".into(),
        }
    }

    fn status(&self) -> Response {
        let cams = discover_cameras();
        let camera_found = cams.iter().any(|c| c.path == self.camera_path);
        let enrolled_users = self.storage.list_users().unwrap_or_default();

        Response::StatusResult {
            ready: true,
            camera_found,
            camera_path: self.camera_path.to_string_lossy().into_owned(),
            enrolled_users,
        }
    }
}
