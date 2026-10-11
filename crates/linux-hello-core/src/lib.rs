pub mod engine;
pub mod ffi;
pub mod matcher;
pub mod storage;

pub use engine::{EngineError, Face, FaceEngine};
pub use matcher::{verify_face, MatchResult, DEFAULT_COSINE_THRESHOLD};
pub use storage::{Storage, StorageError, UserProfile};

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::{Storage, UserProfile};
    use tempfile::tempdir;

    #[test]
    fn test_cosine_similarity() {
        let v1 = vec![1.0, 0.0, 0.0];
        let v2 = vec![1.0, 0.0, 0.0];
        let sim = FaceEngine::cosine_similarity(&v1, &v2);
        assert!((sim - 1.0).abs() < 1e-4);

        let v3 = vec![0.0, 1.0, 0.0];
        let sim_ortho = FaceEngine::cosine_similarity(&v1, &v3);
        assert!(sim_ortho.abs() < 1e-4);
    }

    #[test]
    fn test_storage_profile_lifecycle() {
        let dir = tempdir().unwrap();
        let storage = Storage::new(dir.path()).unwrap();

        let mut profile = UserProfile::new("alice".to_string());
        profile.add_embedding(vec![0.5; 128], "front".to_string());
        storage.save_profile(&profile).unwrap();

        let loaded = storage.load_profile("alice").unwrap().unwrap();
        assert_eq!(loaded.username, "alice");
        assert_eq!(loaded.enrolled_embeddings.len(), 1);
        assert_eq!(loaded.labels[0], "front");

        // Verify file mode is 0644
        use std::os::unix::fs::PermissionsExt;
        let perms = std::fs::metadata(storage.user_path("alice")).unwrap().permissions();
        assert_eq!(perms.mode() & 0o777, 0o644);

        let users = storage.list_users().unwrap();
        assert_eq!(users, vec!["alice".to_string()]);

        assert!(storage.delete_user("alice").unwrap());
        assert!(storage.load_profile("alice").unwrap().is_none());
    }
}

    #[test]
    fn test_face_engine_model_loading() {
        use std::path::Path;
        let det = Path::new("../../models/face_detection_yunet.onnx");
        let rec = Path::new("../../models/face_recognition_sface.onnx");
        if det.exists() && rec.exists() {
            let engine = FaceEngine::new(det, rec, 640, 400).expect("Failed to initialize FaceEngine");
            let dummy_frame = vec![128u8; 640 * 400];
            let faces = engine.process_frame(&dummy_frame, 640, 400).expect("Process frame failed");
            assert_eq!(faces.len(), 0);
        }
    }
