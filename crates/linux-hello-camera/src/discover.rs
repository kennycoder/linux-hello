use std::ffi::CStr;
use std::fs;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};

use crate::v4l2_sys::*;

#[derive(Debug, Clone)]
pub struct CameraInfo {
    pub path: PathBuf,
    pub card: String,
    pub driver: String,
    pub formats: Vec<String>,
    pub is_ir: bool,
}

fn c_bytes_to_string(bytes: &[u8]) -> String {
    CStr::from_bytes_until_nul(bytes)
        .map(|cstr| cstr.to_string_lossy().into_owned())
        .unwrap_or_else(|_| String::from_utf8_lossy(bytes).into_owned())
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

pub fn inspect_camera(path: &Path) -> Option<CameraInfo> {
    let file = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .custom_flags(nix::libc::O_NONBLOCK)
        .open(path)
        .ok()?;

    let mut cap = v4l2_capability::default();
    querycap(&file, &mut cap).ok()?;

    let card = c_bytes_to_string(&cap.card);
    let driver = c_bytes_to_string(&cap.driver);

    let mut formats = Vec::new();
    let mut is_ir = false;

    let mut fmtdesc = v4l2_fmtdesc {
        type_: V4L2_BUF_TYPE_VIDEO_CAPTURE,
        ..Default::default()
    };

    while enum_fmt(&file, &mut fmtdesc).is_ok() {
        let fourcc = fmtdesc.pixelformat.to_le_bytes();
        let fourcc_str = String::from_utf8_lossy(&fourcc).into_owned();
        formats.push(fourcc_str.clone());

        if fmtdesc.pixelformat == V4L2_PIX_FMT_GREY || fourcc_str == "Y16 " || fourcc_str == "Z16 " {
            is_ir = true;
        }

        fmtdesc.index += 1;
    }

    // Must have at least one capture format
    if formats.is_empty() {
        return None;
    }

    let lower_card = card.to_lowercase();
    if lower_card.contains("ir") || lower_card.contains("infra") {
        is_ir = true;
    }

    Some(CameraInfo {
        path: path.to_path_buf(),
        card,
        driver,
        formats,
        is_ir,
    })
}

