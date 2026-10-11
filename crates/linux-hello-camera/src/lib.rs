pub mod discover;
pub mod stream;
pub mod v4l2_sys;

pub use discover::{discover_cameras, inspect_camera, CameraInfo};
pub use stream::{CameraError, CameraStream, Frame};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_camera_discovery() {
        let cameras = discover_cameras();
        assert!(!cameras.is_empty(), "Expected at least one camera device in /dev");
        let ir_found = cameras.iter().any(|c| c.is_ir);
        assert!(ir_found, "Expected at least one camera identified as IR");
    }
}
