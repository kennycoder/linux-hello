use std::ffi::CString;
use std::os::raw::c_void;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::ptr;
use thiserror::Error;

use crate::v4l2_sys::*;

#[derive(Error, Debug)]
pub enum CameraError {
    #[error("Failed to open device {path}: {source}")]
    OpenDevice {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("Ioctl error ({name}): {source}")]
    Ioctl {
        name: &'static str,
        source: std::io::Error,
    },
    #[error("Timeout waiting for camera frame ({timeout_ms}ms)")]
    Timeout { timeout_ms: u32 },
    #[error("No illuminated frame found after {tries} attempts (min brightness: {min_brightness})")]
    NoIlluminatedFrame { tries: usize, min_brightness: f32 },
}

#[derive(Debug, Clone)]
pub struct Frame {
    pub width: u32,
    pub height: u32,
    pub gray_data: Vec<u8>,
    pub mean_brightness: f32,
}

impl Frame {
    pub fn to_bgr(&self) -> Vec<u8> {
        let mut bgr = Vec::with_capacity(self.gray_data.len() * 3);
        for &pixel in &self.gray_data {
            bgr.push(pixel);
            bgr.push(pixel);
            bgr.push(pixel);
        }
        bgr
    }
}

struct MmapBuffer {
    start: *mut c_void,
    length: usize,
}

pub struct CameraStream {
    fd: i32,
    buffers: Vec<MmapBuffer>,
    pub width: u32,
    pub height: u32,
    pub device_path: PathBuf,
    pixelformat: u32,
    peak_brightness: f32,
    peak_center_brightness: f32,
    frames_seen: usize,
}

fn configure_roi(fd: i32, left: i32, top: i32, width: u32, height: u32) {
    unsafe {
        let mut rect = v4l2_rect {
            left,
            top,
            width,
            height,
        };
        let mut ext_ctrl = v4l2_ext_control {
            id: V4L2_CID_REGION_OF_INTEREST_RECT,
            size: std::mem::size_of::<v4l2_rect>() as u32,
            reserved2: [0],
            ptr: &mut rect as *mut _ as *mut c_void,
        };
        let mut ext_ctrls = v4l2_ext_controls {
            which: 0,
            count: 1,
            error_idx: 0,
            request_fd: 0,
            reserved: [0],
            controls: &mut ext_ctrl as *mut _,
        };
        let _ = xioctl(fd, VIDIOC_S_EXT_CTRLS, &mut ext_ctrls as *mut _ as *mut _);

        let mut auto_ctrl = v4l2_control {
            id: V4L2_CID_REGION_OF_INTEREST_AUTO,
            value: V4L2_CID_REGION_OF_INTEREST_AUTO_EXPOSURE,
        };
        let _ = xioctl(fd, VIDIOC_S_CTRL, &mut auto_ctrl as *mut _ as *mut _);
    }
}

impl CameraStream {
    pub fn open(path: &Path, width: u32, height: u32) -> Result<Self, CameraError> {
        let c_path = CString::new(path.as_os_str().as_bytes()).map_err(|e| CameraError::OpenDevice {
            path: path.to_path_buf(),
            source: std::io::Error::new(std::io::ErrorKind::InvalidInput, e),
        })?;

        let fd = unsafe { libc::open(c_path.as_ptr(), libc::O_RDWR | libc::O_NONBLOCK) };
        if fd < 0 {
            return Err(CameraError::OpenDevice {
                path: path.to_path_buf(),
                source: std::io::Error::last_os_error(),
            });
        }

        // Set pixel format to GREY
        let mut fmt: v4l2_format = unsafe { std::mem::zeroed() };
        fmt.type_ = V4L2_BUF_TYPE_VIDEO_CAPTURE;
        unsafe {
            fmt.fmt.pix.width = width;
            fmt.fmt.pix.height = height;
            fmt.fmt.pix.pixelformat = V4L2_PIX_FMT_GREY;
            fmt.fmt.pix.field = V4L2_FIELD_ANY;

            if xioctl(fd, VIDIOC_S_FMT, &mut fmt as *mut _ as *mut _) < 0 {
                // If GREY failed, try YUYV
                fmt.fmt.pix.pixelformat = V4L2_PIX_FMT_YUYV;
                if xioctl(fd, VIDIOC_S_FMT, &mut fmt as *mut _ as *mut _) < 0 {
                    libc::close(fd);
                    return Err(CameraError::Ioctl {
                        name: "VIDIOC_S_FMT",
                        source: std::io::Error::last_os_error(),
                    });
                }
            }
        }

        let actual_width = unsafe { fmt.fmt.pix.width };
        let actual_height = unsafe { fmt.fmt.pix.height };
        let actual_pixfmt = unsafe { fmt.fmt.pix.pixelformat };

        // Initialize UVC Region of Interest (ROI) Auto-Exposure to the full frame
        // (fixing devices that boot with a broken 1x1 ROI at (0,0)), and narrow to the face once detected.
        if actual_width >= 64 && actual_height >= 64 {
            configure_roi(fd, 0, 0, actual_width, actual_height);
        }

        // Request 4 MMAP buffers
        let mut req: v4l2_requestbuffers = unsafe { std::mem::zeroed() };
        req.count = 4;
        req.type_ = V4L2_BUF_TYPE_VIDEO_CAPTURE;
        req.memory = V4L2_MEMORY_MMAP;

        if unsafe { xioctl(fd, VIDIOC_REQBUFS, &mut req as *mut _ as *mut _) } < 0 {
            unsafe { libc::close(fd) };
            return Err(CameraError::Ioctl {
                name: "VIDIOC_REQBUFS",
                source: std::io::Error::last_os_error(),
            });
        }

        let mut buffers = Vec::with_capacity(req.count as usize);

        for i in 0..req.count {
            let mut buf: v4l2_buffer = unsafe { std::mem::zeroed() };
            buf.type_ = V4L2_BUF_TYPE_VIDEO_CAPTURE;
            buf.memory = V4L2_MEMORY_MMAP;
            buf.index = i;

            if unsafe { xioctl(fd, VIDIOC_QUERYBUF, &mut buf as *mut _ as *mut _) } < 0 {
                unsafe { libc::close(fd) };
                return Err(CameraError::Ioctl {
                    name: "VIDIOC_QUERYBUF",
                    source: std::io::Error::last_os_error(),
                });
            }

            let start = unsafe {
                libc::mmap(
                    ptr::null_mut(),
                    buf.length as usize,
                    libc::PROT_READ | libc::PROT_WRITE,
                    libc::MAP_SHARED,
                    fd,
                    buf.m.offset as i64,
                )
            };

            if start == libc::MAP_FAILED {
                unsafe { libc::close(fd) };
                return Err(CameraError::Ioctl {
                    name: "mmap",
                    source: std::io::Error::last_os_error(),
                });
            }

            buffers.push(MmapBuffer {
                start,
                length: buf.length as usize,
            });

            // Queue buffer
            if unsafe { xioctl(fd, VIDIOC_QBUF, &mut buf as *mut _ as *mut _) } < 0 {
                unsafe { libc::close(fd) };
                return Err(CameraError::Ioctl {
                    name: "VIDIOC_QBUF",
                    source: std::io::Error::last_os_error(),
                });
            }
        }

        // Stream ON
        let mut type_ = V4L2_BUF_TYPE_VIDEO_CAPTURE;
        if unsafe { xioctl(fd, VIDIOC_STREAMON, &mut type_ as *mut _ as *mut _) } < 0 {
            unsafe { libc::close(fd) };
            return Err(CameraError::Ioctl {
                name: "VIDIOC_STREAMON",
                source: std::io::Error::last_os_error(),
            });
        }

        let mut stream = CameraStream {
            fd,
            buffers,
            width: actual_width,
            height: actual_height,
            device_path: path.to_path_buf(),
            pixelformat: actual_pixfmt,
            peak_brightness: 0.0,
            peak_center_brightness: 0.0,
            frames_seen: 0,
        };

        // Prime the stream by reading 2 initial frames so the IR emitter strobe
        // and hardware auto-exposure settle and peak_brightness is calibrated.
        for _ in 0..2 {
            if let Ok((frame, center_mean, _, center_sat_ratio)) = stream.dequeue_raw_frame(250) {
                stream.frames_seen += 1;
                if center_sat_ratio <= 0.25 {
                    stream.peak_brightness = stream.peak_brightness.max(frame.mean_brightness);
                    stream.peak_center_brightness = stream.peak_center_brightness.max(center_mean);
                }
            }
        }

        Ok(stream)
    }

    /// Updates the camera's hardware Auto-Exposure Region of Interest (ROI) to track a detected face.
    pub fn update_face_roi(&mut self, x: f32, y: f32, w: f32, h: f32) {
        if self.width < 64 || self.height < 64 || w < 24.0 || h < 24.0 {
            return;
        }
        let pad_x = w * 0.25;
        let pad_y = h * 0.25;
        let left = ((x - pad_x).round() as i32).clamp(0, self.width as i32 - 32);
        let top = ((y - pad_y).round() as i32).clamp(0, self.height as i32 - 32);
        let roi_w = ((w + 2.0 * pad_x).round() as u32).clamp(32, self.width - left as u32);
        let roi_h = ((h + 2.0 * pad_y).round() as u32).clamp(32, self.height - top as u32);
        configure_roi(self.fd, left, top, roi_w, roi_h);
    }

    /// Resets the camera's hardware Auto-Exposure ROI to the full frame when no face is currently tracked.
    pub fn reset_roi(&mut self) {
        if self.width >= 64 && self.height >= 64 {
            configure_roi(self.fd, 0, 0, self.width, self.height);
        }
    }

    fn dequeue_raw_frame(&mut self, timeout_ms: u32) -> Result<(Frame, f32, f32, f32), CameraError> {
        let mut fds = libc::pollfd {
            fd: self.fd,
            events: libc::POLLIN,
            revents: 0,
        };

        let ret = unsafe { libc::poll(&mut fds, 1, timeout_ms as i32) };
        if ret <= 0 {
            return Err(CameraError::Timeout { timeout_ms });
        }

        let mut buf: v4l2_buffer = unsafe { std::mem::zeroed() };
        buf.type_ = V4L2_BUF_TYPE_VIDEO_CAPTURE;
        buf.memory = V4L2_MEMORY_MMAP;

        if unsafe { xioctl(self.fd, VIDIOC_DQBUF, &mut buf as *mut _ as *mut _) } < 0 {
            return Err(CameraError::Ioctl {
                name: "VIDIOC_DQBUF",
                source: std::io::Error::last_os_error(),
            });
        }

        let index = buf.index as usize;
        let bytes_used = buf.bytesused as usize;
        let mmap_buf = &self.buffers[index];

        let expected_size = (self.width * self.height) as usize;
        let mut gray_data = vec![0u8; expected_size];

        unsafe {
            let src_slice = std::slice::from_raw_parts(mmap_buf.start as *const u8, bytes_used.min(mmap_buf.length));
            if self.pixelformat == V4L2_PIX_FMT_YUYV && src_slice.len() >= expected_size * 2 {
                for (i, dst) in gray_data.iter_mut().enumerate() {
                    *dst = src_slice[i * 2];
                }
            } else if src_slice.len() >= expected_size {
                gray_data.copy_from_slice(&src_slice[..expected_size]);
            } else {
                gray_data[..src_slice.len()].copy_from_slice(src_slice);
            }
        }

        // Calculate full-frame mean brightness
        let sum: u64 = gray_data.iter().map(|&x| x as u64).sum();
        let mean_brightness = if !gray_data.is_empty() {
            sum as f32 / gray_data.len() as f32
        } else {
            0.0
        };

        // Calculate central ROI stats (where user face is typically illuminated by IR emitter)
        let x0 = (self.width / 6) as usize;
        let x1 = ((self.width * 5) / 6) as usize;
        let y0 = (self.height / 6) as usize;
        let y1 = ((self.height * 5) / 6) as usize;
        let w = self.width as usize;

        let mut c_sum = 0.0f64;
        let mut c_sum_sq = 0.0f64;
        let mut c_sat = 0usize;
        let mut c_count = 0usize;

        for y in (y0..y1).step_by(2) {
            let row = y * w;
            for x in (x0..x1).step_by(2) {
                let p = gray_data[row + x];
                let pf = p as f64;
                c_sum += pf;
                c_sum_sq += pf * pf;
                if p >= 248 {
                    c_sat += 1;
                }
                c_count += 1;
            }
        }

        let (center_mean, center_std, center_sat_ratio) = if c_count > 0 {
            let cm = c_sum / (c_count as f64);
            let cv = (c_sum_sq / (c_count as f64)) - (cm * cm);
            (
                cm as f32,
                if cv > 0.0 { cv.sqrt() as f32 } else { 0.0 },
                (c_sat as f32) / (c_count as f32),
            )
        } else {
            (mean_brightness, 0.0, 0.0)
        };

        // Re-queue the buffer
        if unsafe { xioctl(self.fd, VIDIOC_QBUF, &mut buf as *mut _ as *mut _) } < 0 {
            return Err(CameraError::Ioctl {
                name: "VIDIOC_QBUF",
                source: std::io::Error::last_os_error(),
            });
        }

        Ok((
            Frame {
                width: self.width,
                height: self.height,
                gray_data,
                mean_brightness,
            },
            center_mean,
            center_std,
            center_sat_ratio,
        ))
    }

    pub fn capture_frame(&mut self, timeout_ms: u32) -> Result<Frame, CameraError> {
        let mut last_frame: Option<(Frame, f32)> = None;

        // Many laptop IR cameras pulse the IR emitter on alternating frames (1 frame ON, 1 frame OFF).
        // Automatically skip dark IR-OFF ambient frames and initial blown-out AE warm-up frames
        // so callers only receive properly illuminated IR-ON frames.
        for attempt in 0..5 {
            let (frame, center_mean, center_std, center_sat_ratio) = self.dequeue_raw_frame(timeout_ms)?;
            self.frames_seen += 1;

            let is_ir_off = frame.mean_brightness < 5.0
                || (self.peak_brightness > 8.0
                    && frame.mean_brightness < self.peak_brightness * 0.94
                    && (self.peak_brightness - frame.mean_brightness) >= 3.0)
                || (self.peak_center_brightness > 10.0
                    && center_mean < self.peak_center_brightness * 0.92
                    && (self.peak_center_brightness - center_mean) >= 3.5)
                || (center_mean < frame.mean_brightness * 0.80 && center_std < 12.0);

            let is_blown_warmup = self.frames_seen <= 6 && center_sat_ratio > 0.15;

            if (is_ir_off || is_blown_warmup) && attempt < 4 {
                self.peak_brightness *= 0.98;
                self.peak_center_brightness *= 0.98;
                if last_frame.as_ref().map_or(true, |(_, cm)| center_mean > *cm) {
                    last_frame = Some((frame, center_mean));
                }
                continue;
            }

            if center_sat_ratio <= 0.25 {
                self.peak_brightness = (self.peak_brightness * 0.96).max(frame.mean_brightness);
                self.peak_center_brightness = (self.peak_center_brightness * 0.96).max(center_mean);
            }
            return Ok(frame);
        }

        Ok(last_frame.unwrap().0)
    }

    pub fn capture_illuminated_frame(
        &mut self,
        max_tries: usize,
        min_brightness: f32,
    ) -> Result<Frame, CameraError> {
        let mut best_frame: Option<Frame> = None;

        // Allow a few initial frames for hardware auto-exposure to settle after camera open
        // so the snapshot is not blown out into a white silhouette.
        for _ in 0..4 {
            let _ = self.capture_frame(300);
        }

        let effective_min = min_brightness.min(15.0);

        for _ in 0..max_tries {
            match self.capture_frame(500) {
                Ok(frame) => {
                    if frame.mean_brightness >= effective_min {
                        return Ok(frame);
                    }
                    if best_frame.as_ref().map_or(true, |b| frame.mean_brightness > b.mean_brightness) {
                        best_frame = Some(frame);
                    }
                }
                Err(err) => {
                    log::warn!("Frame capture warning: {}", err);
                }
            }
        }

        best_frame.ok_or(CameraError::NoIlluminatedFrame {
            tries: max_tries,
            min_brightness,
        })
    }
}

impl Drop for CameraStream {
    fn drop(&mut self) {
        unsafe {
            let mut type_ = V4L2_BUF_TYPE_VIDEO_CAPTURE;
            xioctl(self.fd, VIDIOC_STREAMOFF, &mut type_ as *mut _ as *mut _);

            for buf in &self.buffers {
                libc::munmap(buf.start, buf.length);
            }

            libc::close(self.fd);
        }
    }
}
