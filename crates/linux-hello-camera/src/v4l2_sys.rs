// Low-level V4L2 ioctl bindings and safe wrappers via `nix`
use std::io;
use std::os::fd::AsRawFd;
use std::os::raw::{c_uchar, c_void};

use nix::errno::Errno;

pub const V4L2_CID_REGION_OF_INTEREST_RECT: u32 = 0x00981ae1;
pub const V4L2_CID_REGION_OF_INTEREST_AUTO: u32 = 0x00981ae2;
pub const V4L2_CID_REGION_OF_INTEREST_AUTO_EXPOSURE: i32 = 1;

pub const V4L2_BUF_TYPE_VIDEO_CAPTURE: u32 = 1;
pub const V4L2_MEMORY_MMAP: u32 = 1;
pub const V4L2_FIELD_ANY: u32 = 0;

// Pixel formats
pub const V4L2_PIX_FMT_GREY: u32 = u32::from_le_bytes(*b"GREY");
pub const V4L2_PIX_FMT_YUYV: u32 = u32::from_le_bytes(*b"YUYV");
pub const V4L2_PIX_FMT_MJPEG: u32 = u32::from_le_bytes(*b"MJPG");

#[repr(C)]
#[derive(Debug, Default, Copy, Clone)]
pub struct v4l2_control {
    pub id: u32,
    pub value: i32,
}

#[repr(C)]
#[derive(Debug, Default, Copy, Clone)]
pub struct v4l2_rect {
    pub left: i32,
    pub top: i32,
    pub width: u32,
    pub height: u32,
}

#[repr(C, packed)]
#[derive(Copy, Clone)]
pub struct v4l2_ext_control {
    pub id: u32,
    pub size: u32,
    pub reserved2: [u32; 1],
    pub ptr: *mut c_void,
}

#[repr(C)]
#[derive(Copy, Clone)]
pub struct v4l2_ext_controls {
    pub which: u32,
    pub count: u32,
    pub error_idx: u32,
    pub request_fd: i32,
    pub reserved: [u32; 1],
    pub controls: *mut v4l2_ext_control,
}

#[repr(C)]
#[derive(Debug, Default, Copy, Clone)]
pub struct v4l2_capability {
    pub driver: [c_uchar; 16],
    pub card: [c_uchar; 32],
    pub bus_info: [c_uchar; 32],
    pub version: u32,
    pub capabilities: u32,
    pub device_caps: u32,
    pub reserved: [u32; 3],
}

#[repr(C)]
#[derive(Debug, Default, Copy, Clone)]
pub struct v4l2_fmtdesc {
    pub index: u32,
    pub type_: u32,
    pub flags: u32,
    pub description: [c_uchar; 32],
    pub pixelformat: u32,
    pub mbus_code: u32,
    pub reserved: [u32; 3],
}

#[repr(C)]
#[derive(Debug, Default, Copy, Clone)]
pub struct v4l2_pix_format {
    pub width: u32,
    pub height: u32,
    pub pixelformat: u32,
    pub field: u32,
    pub bytesperline: u32,
    pub sizeimage: u32,
    pub colorspace: u32,
    pub priv_: u32,
    pub flags: u32,
    pub ycbcr_enc: u32,
    pub quantization: u32,
    pub xfer_func: u32,
}

#[repr(C)]
#[derive(Copy, Clone)]
pub struct v4l2_format {
    pub type_: u32,
    pub _pad: u32,
    pub pix: v4l2_pix_format,
    pub _fmt_pad: [c_uchar; 152],
}

const _: () = assert!(std::mem::size_of::<v4l2_format>() == 208);

impl Default for v4l2_format {
    fn default() -> Self {
        Self {
            type_: 0,
            _pad: 0,
            pix: v4l2_pix_format::default(),
            _fmt_pad: [0; 152],
        }
    }
}

impl v4l2_format {
    pub fn set_pix_format(&mut self, width: u32, height: u32, pixelformat: u32, field: u32) {
        self.type_ = V4L2_BUF_TYPE_VIDEO_CAPTURE;
        self.pix = v4l2_pix_format {
            width,
            height,
            pixelformat,
            field,
            ..Default::default()
        };
    }

    pub fn pix_format(&self) -> v4l2_pix_format {
        self.pix
    }
}

#[repr(C)]
#[derive(Debug, Default, Copy, Clone)]
pub struct v4l2_requestbuffers {
    pub count: u32,
    pub type_: u32,
    pub memory: u32,
    pub capabilities: u32,
    pub flags: u8,
    pub reserved: [u8; 3],
}

#[repr(C)]
#[derive(Debug, Default, Copy, Clone)]
pub struct v4l2_timeval {
    pub tv_sec: i64,
    pub tv_usec: i64,
}

#[repr(C)]
#[derive(Debug, Default, Copy, Clone)]
pub struct v4l2_buffer {
    pub index: u32,
    pub type_: u32,
    pub bytesused: u32,
    pub flags: u32,
    pub field: u32,
    pub _pad: u32,
    pub timestamp: v4l2_timeval,
    pub timecode: [c_uchar; 16],
    pub sequence: u32,
    pub memory: u32,
    pub offset: u32,
    pub _m_pad: u32,
    pub length: u32,
    pub reserved2: u32,
    pub request_fd: i32,
}

const _: () = assert!(std::mem::size_of::<v4l2_buffer>() == 88);

impl v4l2_buffer {
    pub fn offset(&self) -> u32 {
        self.offset
    }
}


nix::ioctl_read!(vidioc_querycap, b'V', 0, v4l2_capability);
nix::ioctl_readwrite!(vidioc_enum_fmt, b'V', 2, v4l2_fmtdesc);
nix::ioctl_readwrite!(vidioc_g_fmt, b'V', 4, v4l2_format);
nix::ioctl_readwrite!(vidioc_s_fmt, b'V', 5, v4l2_format);
nix::ioctl_readwrite!(vidioc_reqbufs, b'V', 8, v4l2_requestbuffers);
nix::ioctl_readwrite!(vidioc_querybuf, b'V', 9, v4l2_buffer);
nix::ioctl_readwrite!(vidioc_qbuf, b'V', 15, v4l2_buffer);
nix::ioctl_readwrite!(vidioc_dqbuf, b'V', 17, v4l2_buffer);
nix::ioctl_write_ptr!(vidioc_streamon, b'V', 18, u32);
nix::ioctl_write_ptr!(vidioc_streamoff, b'V', 19, u32);
nix::ioctl_readwrite!(vidioc_g_ctrl, b'V', 27, v4l2_control);
nix::ioctl_readwrite!(vidioc_s_ctrl, b'V', 28, v4l2_control);
nix::ioctl_readwrite!(vidioc_g_ext_ctrls, b'V', 71, v4l2_ext_controls);
nix::ioctl_readwrite!(vidioc_s_ext_ctrls, b'V', 72, v4l2_ext_controls);

/// Retries a `nix` ioctl closure automatically if interrupted by a signal (`EINTR`).
pub fn xioctl<F, T>(mut f: F) -> io::Result<T>
where
    F: FnMut() -> nix::Result<T>,
{
    loop {
        match f() {
            Ok(val) => return Ok(val),
            Err(Errno::EINTR) => continue,
            Err(err) => return Err(io::Error::from(err)),
        }
    }
}

pub fn querycap(fd: &impl AsRawFd, cap: &mut v4l2_capability) -> io::Result<()> {
    let raw_fd = fd.as_raw_fd();
    xioctl(|| unsafe { vidioc_querycap(raw_fd, cap) }).map(|_| ())
}

pub fn enum_fmt(fd: &impl AsRawFd, fmtdesc: &mut v4l2_fmtdesc) -> io::Result<()> {
    let raw_fd = fd.as_raw_fd();
    xioctl(|| unsafe { vidioc_enum_fmt(raw_fd, fmtdesc) }).map(|_| ())
}

pub fn g_fmt(fd: &impl AsRawFd, fmt: &mut v4l2_format) -> io::Result<()> {
    let raw_fd = fd.as_raw_fd();
    xioctl(|| unsafe { vidioc_g_fmt(raw_fd, fmt) }).map(|_| ())
}

pub fn s_fmt(fd: &impl AsRawFd, fmt: &mut v4l2_format) -> io::Result<()> {
    let raw_fd = fd.as_raw_fd();
    xioctl(|| unsafe { vidioc_s_fmt(raw_fd, fmt) }).map(|_| ())
}

pub fn reqbufs(fd: &impl AsRawFd, req: &mut v4l2_requestbuffers) -> io::Result<()> {
    let raw_fd = fd.as_raw_fd();
    xioctl(|| unsafe { vidioc_reqbufs(raw_fd, req) }).map(|_| ())
}

pub fn querybuf(fd: &impl AsRawFd, buf: &mut v4l2_buffer) -> io::Result<()> {
    let raw_fd = fd.as_raw_fd();
    xioctl(|| unsafe { vidioc_querybuf(raw_fd, buf) }).map(|_| ())
}

pub fn qbuf(fd: &impl AsRawFd, buf: &mut v4l2_buffer) -> io::Result<()> {
    let raw_fd = fd.as_raw_fd();
    xioctl(|| unsafe { vidioc_qbuf(raw_fd, buf) }).map(|_| ())
}

pub fn dqbuf(fd: &impl AsRawFd, buf: &mut v4l2_buffer) -> io::Result<()> {
    let raw_fd = fd.as_raw_fd();
    xioctl(|| unsafe { vidioc_dqbuf(raw_fd, buf) }).map(|_| ())
}

pub fn streamon(fd: &impl AsRawFd, buf_type: u32) -> io::Result<()> {
    let raw_fd = fd.as_raw_fd();
    xioctl(|| unsafe { vidioc_streamon(raw_fd, &buf_type) }).map(|_| ())
}

pub fn streamoff(fd: &impl AsRawFd, buf_type: u32) -> io::Result<()> {
    let raw_fd = fd.as_raw_fd();
    xioctl(|| unsafe { vidioc_streamoff(raw_fd, &buf_type) }).map(|_| ())
}

pub fn g_ctrl(fd: &impl AsRawFd, ctrl: &mut v4l2_control) -> io::Result<()> {
    let raw_fd = fd.as_raw_fd();
    xioctl(|| unsafe { vidioc_g_ctrl(raw_fd, ctrl) }).map(|_| ())
}

pub fn s_ctrl(fd: &impl AsRawFd, ctrl: &mut v4l2_control) -> io::Result<()> {
    let raw_fd = fd.as_raw_fd();
    xioctl(|| unsafe { vidioc_s_ctrl(raw_fd, ctrl) }).map(|_| ())
}

pub fn g_ext_ctrls(fd: &impl AsRawFd, ctrls: &mut v4l2_ext_controls) -> io::Result<()> {
    let raw_fd = fd.as_raw_fd();
    xioctl(|| unsafe { vidioc_g_ext_ctrls(raw_fd, ctrls) }).map(|_| ())
}

pub fn s_ext_ctrls(fd: &impl AsRawFd, ctrls: &mut v4l2_ext_controls) -> io::Result<()> {
    let raw_fd = fd.as_raw_fd();
    xioctl(|| unsafe { vidioc_s_ext_ctrls(raw_fd, ctrls) }).map(|_| ())
}

