use std::ffi::CStr;
use std::fs;
use std::path::PathBuf;

use crate::v4l2_sys::*;

#[derive(Debug, Clone)]
pub struct CameraInfo {
    pub path: PathBuf,
    pub card: String,
    pub driver: String,
    pub formats: Vec<String>,
    pub is_ir: bool,
}

pub fn discover_cameras() -> Vec<CameraInfo> {
    let mut cameras = Vec::new();

    // Iterate through /dev/video* entries
    if let Ok(entries) = fs::read_dir("/dev") {
        for entry in entries.flatten() {
            let file_name = entry.file_name();
            let name = file_name.to_string_lossy();
            if !name.starts_with("video") {
                continue;
            }

            let path = entry.path();
            if let Some(info) = inspect_camera(&path) {
                cameras.push(info);
            }
        }
    }

    // Sort so IR cameras appear first, then by device path
    cameras.sort_by(|a, b| {
        b.is_ir.cmp(&a.is_ir).then_with(|| a.path.cmp(&b.path))
    });

    cameras
}

pub fn inspect_camera(path: &PathBuf) -> Option<CameraInfo> {
    use std::os::unix::ffi::OsStrExt;
    let path_cstr = std::ffi::CString::new(path.as_os_str().as_bytes()).ok()?;

    unsafe {
        let fd = libc::open(path_cstr.as_ptr(), libc::O_RDWR | libc::O_NONBLOCK);
        if fd < 0 {
            return None;
        }

        let mut cap: v4l2_capability = std::mem::zeroed();
        if xioctl(fd, VIDIOC_QUERYCAP, &mut cap as *mut _ as *mut _) < 0 {
            libc::close(fd);
            return None;
        }

        let card = CStr::from_ptr(cap.card.as_ptr() as *const _).to_string_lossy().into_owned();
        let driver = CStr::from_ptr(cap.driver.as_ptr() as *const _).to_string_lossy().into_owned();

        let mut formats = Vec::new();
        let mut is_ir = false;

        let mut fmtdesc: v4l2_fmtdesc = std::mem::zeroed();
        fmtdesc.type_ = V4L2_BUF_TYPE_VIDEO_CAPTURE;

        loop {
            if xioctl(fd, VIDIOC_ENUM_FMT, &mut fmtdesc as *mut _ as *mut _) < 0 {
                break;
            }

            let fourcc = fmtdesc.pixelformat.to_le_bytes();
            let fourcc_str = String::from_utf8_lossy(&fourcc).into_owned();
            formats.push(fourcc_str.clone());

            if fmtdesc.pixelformat == V4L2_PIX_FMT_GREY || fourcc_str == "Y16 " || fourcc_str == "Z16 " {
                is_ir = true;
            }

            fmtdesc.index += 1;
        }

        libc::close(fd);

        let lower_card = card.to_lowercase();
        if lower_card.contains("ir") || lower_card.contains("infra") {
            is_ir = true;
        }

        // Must have at least one capture format
        if formats.is_empty() {
            return None;
        }

        Some(CameraInfo {
            path: path.clone(),
            card,
            driver,
            formats,
            is_ir,
        })
    }
}
