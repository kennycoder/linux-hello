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
        if cameras.is_empty() {
            println!("No camera devices found in /dev (headless/CI environment); skipping device inspection check");
            return;
        }

        // If camera devices are present on host, verify valid metadata
        for cam in &cameras {
            assert!(!cam.path.as_os_str().is_empty());
        }
        let ir_found = cameras.iter().any(|c| c.is_ir);
        println!("Discovered {} cameras (IR camera present: {})", cameras.len(), ir_found);
    }
}
