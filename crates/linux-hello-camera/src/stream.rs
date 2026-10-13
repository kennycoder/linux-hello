use std::fs::{self, File};
use std::io;
use std::num::NonZeroUsize;
use std::os::fd::{AsFd, AsRawFd};
use std::os::raw::c_void;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::ptr::NonNull;
use thiserror::Error;

use nix::poll::{poll, PollFd, PollFlags, PollTimeout};
use nix::sys::mman::{mmap, munmap, MapFlags, ProtFlags};

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
    ptr: NonNull<c_void>,
    length: usize,
}

impl MmapBuffer {
    fn map(file: &File, length: usize, offset: u32) -> io::Result<Self> {
        let non_zero_len = NonZeroUsize::new(length).ok_or_else(|| {
            io::Error::new(io::ErrorKind::InvalidInput, "V4L2 buffer length is zero")
        })?;

        // SAFETY: Mapping a valid V4L2 MMAP buffer file descriptor and offset returned by `VIDIOC_QUERYBUF`.
        let ptr = unsafe {
            mmap(
                None,
                non_zero_len,
                ProtFlags::PROT_READ | ProtFlags::PROT_WRITE,
                MapFlags::MAP_SHARED,
                file,
                i64::from(offset),
            )
        }
        .map_err(io::Error::from)?;

        Ok(Self { ptr, length })
    }

    fn as_slice(&self, bytes_used: usize) -> &[u8] {
        let len = bytes_used.min(self.length);
        // SAFETY: `self.ptr` points to a valid `MAP_SHARED` region of `self.length` bytes for the lifetime of `self`.
        unsafe { std::slice::from_raw_parts(self.ptr.as_ptr() as *const u8, len) }
    }
}

impl Drop for MmapBuffer {
    fn drop(&mut self) {
        // SAFETY: `self.ptr` and `self.length` were obtained from a successful `mmap` call in `MmapBuffer::map`.
        unsafe {
            let _ = munmap(self.ptr, self.length);
        }
    }
}

pub struct CameraStream {
    file: File,
    buffers: Vec<MmapBuffer>,
    pub width: u32,
    pub height: u32,
    pub device_path: PathBuf,
    pixelformat: u32,
    peak_brightness: f32,
    peak_center_brightness: f32,
    frames_seen: usize,
}

fn configure_roi(fd: &impl AsRawFd, left: i32, top: i32, width: u32, height: u32) {
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
        ptr: (&mut rect as *mut v4l2_rect).cast::<c_void>(),
    };
    let mut ext_ctrls = v4l2_ext_controls {
        which: 0,
        count: 1,
        error_idx: 0,
        request_fd: 0,
        reserved: [0],
        controls: &mut ext_ctrl,
    };
    let _ = s_ext_ctrls(fd, &mut ext_ctrls);

    let mut auto_ctrl = v4l2_control {
        id: V4L2_CID_REGION_OF_INTEREST_AUTO,
        value: V4L2_CID_REGION_OF_INTEREST_AUTO_EXPOSURE,
    };
    let _ = s_ctrl(fd, &mut auto_ctrl);
}

impl CameraStream {
    pub fn open(path: &Path, width: u32, height: u32) -> Result<Self, CameraError> {
        let file = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .custom_flags(nix::libc::O_NONBLOCK)
            .open(path)
            .map_err(|source| CameraError::OpenDevice {
                path: path.to_path_buf(),
                source,
            })?;

        // Set pixel format to GREY, falling back to YUYV if GREY is unsupported
        let mut fmt = v4l2_format::default();
        fmt.set_pix_format(width, height, V4L2_PIX_FMT_GREY, V4L2_FIELD_ANY);

        if s_fmt(&file, &mut fmt).is_err() {
            fmt.set_pix_format(width, height, V4L2_PIX_FMT_YUYV, V4L2_FIELD_ANY);
            s_fmt(&file, &mut fmt).map_err(|source| CameraError::Ioctl {
                name: "VIDIOC_S_FMT",
                source,
            })?;
        }

        let pix = fmt.pix_format();
        let actual_width = pix.width;
        let actual_height = pix.height;
        let actual_pixfmt = pix.pixelformat;

        // Initialize UVC Region of Interest (ROI) Auto-Exposure to the full frame
        // (fixing devices that boot with a broken 1x1 ROI at (0,0)), and narrow to the face once detected.
        if actual_width >= 64 && actual_height >= 64 {
            configure_roi(&file, 0, 0, actual_width, actual_height);
        }

        // Request 4 MMAP buffers
        let mut req = v4l2_requestbuffers {
            count: 4,
            type_: V4L2_BUF_TYPE_VIDEO_CAPTURE,
            memory: V4L2_MEMORY_MMAP,
            ..Default::default()
        };

        reqbufs(&file, &mut req).map_err(|source| CameraError::Ioctl {
            name: "VIDIOC_REQBUFS",
            source,
        })?;

        let mut buffers = Vec::with_capacity(req.count as usize);

        for i in 0..req.count {
            let mut buf = v4l2_buffer {
                type_: V4L2_BUF_TYPE_VIDEO_CAPTURE,
                memory: V4L2_MEMORY_MMAP,
                index: i,
                ..Default::default()
            };

            querybuf(&file, &mut buf).map_err(|source| CameraError::Ioctl {
                name: "VIDIOC_QUERYBUF",
                source,
            })?;

            let mmap_buf = MmapBuffer::map(&file, buf.length as usize, buf.offset()).map_err(|source| {
                CameraError::Ioctl {
                    name: "mmap",
                    source,
                }
            })?;
            buffers.push(mmap_buf);

            // Queue buffer
            qbuf(&file, &mut buf).map_err(|source| CameraError::Ioctl {
                name: "VIDIOC_QBUF",
                source,
            })?;
        }

        // Stream ON
        streamon(&file, V4L2_BUF_TYPE_VIDEO_CAPTURE).map_err(|source| CameraError::Ioctl {
            name: "VIDIOC_STREAMON",
            source,
        })?;

        let mut stream = CameraStream {
            file,
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
        configure_roi(&self.file, left, top, roi_w, roi_h);
    }

    /// Resets the camera's hardware Auto-Exposure ROI to the full frame when no face is currently tracked.
    pub fn reset_roi(&mut self) {
        if self.width >= 64 && self.height >= 64 {
            configure_roi(&self.file, 0, 0, self.width, self.height);
        }
    }

    fn dequeue_raw_frame(&mut self, timeout_ms: u32) -> Result<(Frame, f32, f32, f32), CameraError> {
        let mut fds = [PollFd::new(self.file.as_fd(), PollFlags::POLLIN)];
        let poll_timeout = PollTimeout::try_from(timeout_ms).unwrap_or(PollTimeout::MAX);

        match poll(&mut fds, poll_timeout) {
            Ok(n) if n > 0 => {}
            _ => return Err(CameraError::Timeout { timeout_ms }),
        }

        let mut buf = v4l2_buffer {
            type_: V4L2_BUF_TYPE_VIDEO_CAPTURE,
            memory: V4L2_MEMORY_MMAP,
            ..Default::default()
        };

        dqbuf(&self.file, &mut buf).map_err(|source| CameraError::Ioctl {
            name: "VIDIOC_DQBUF",
            source,
        })?;

        let index = buf.index as usize;
        let bytes_used = buf.bytesused as usize;
        let src_slice = self.buffers[index].as_slice(bytes_used);

        let expected_size = (self.width * self.height) as usize;
        let mut gray_data = vec![0u8; expected_size];

        if self.pixelformat == V4L2_PIX_FMT_YUYV && src_slice.len() >= expected_size * 2 {
            for (i, dst) in gray_data.iter_mut().enumerate() {
                *dst = src_slice[i * 2];
            }
        } else if src_slice.len() >= expected_size {
            gray_data.copy_from_slice(&src_slice[..expected_size]);
        } else {
            gray_data[..src_slice.len()].copy_from_slice(src_slice);
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
        qbuf(&self.file, &mut buf).map_err(|source| CameraError::Ioctl {
            name: "VIDIOC_QBUF",
            source,
        })?;

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
        let _ = streamoff(&self.file, V4L2_BUF_TYPE_VIDEO_CAPTURE);
        // `self.buffers` and `self.file` are automatically unmapped and closed via their `Drop` impls.
    }
}

