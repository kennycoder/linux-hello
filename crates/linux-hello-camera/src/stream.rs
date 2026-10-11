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

        Ok(CameraStream {
            fd,
            buffers,
            width: actual_width,
            height: actual_height,
            device_path: path.to_path_buf(),
        })
    }

    pub fn capture_frame(&mut self, timeout_ms: u32) -> Result<Frame, CameraError> {
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
            let src_slice = std::slice::from_raw_parts(mmap_buf.start as *const u8, bytes_used);
            if bytes_used >= expected_size {
                gray_data.copy_from_slice(&src_slice[..expected_size]);
            } else {
                gray_data[..bytes_used].copy_from_slice(src_slice);
            }
        }

        // Calculate mean brightness
        let sum: u64 = gray_data.iter().map(|&x| x as u64).sum();
        let mean_brightness = if !gray_data.is_empty() {
            sum as f32 / gray_data.len() as f32
        } else {
            0.0
        };

        // Re-queue the buffer
        if unsafe { xioctl(self.fd, VIDIOC_QBUF, &mut buf as *mut _ as *mut _) } < 0 {
            return Err(CameraError::Ioctl {
                name: "VIDIOC_QBUF",
                source: std::io::Error::last_os_error(),
            });
        }

        Ok(Frame {
            width: self.width,
            height: self.height,
            gray_data,
            mean_brightness,
        })
    }

    pub fn capture_illuminated_frame(
        &mut self,
        max_tries: usize,
        min_brightness: f32,
    ) -> Result<Frame, CameraError> {
        let mut best_frame: Option<Frame> = None;

        for _ in 0..max_tries {
            match self.capture_frame(500) {
                Ok(frame) => {
                    if frame.mean_brightness >= min_brightness {
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
