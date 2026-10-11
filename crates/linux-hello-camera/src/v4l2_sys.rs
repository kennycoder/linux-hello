// Low-level V4L2 ioctl bindings for Linux x86_64
use std::os::raw::{c_int, c_uchar, c_ulong, c_void};

pub const VIDIOC_QUERYCAP: c_ulong = 0x80685600;
pub const VIDIOC_ENUM_FMT: c_ulong = 0xc0405602;
pub const VIDIOC_G_FMT: c_ulong = 0xc0d05604;
pub const VIDIOC_S_FMT: c_ulong = 0xc0d05605;
pub const VIDIOC_REQBUFS: c_ulong = 0xc0145608;
pub const VIDIOC_QUERYBUF: c_ulong = 0xc0585609;
pub const VIDIOC_QBUF: c_ulong = 0xc058560f;
pub const VIDIOC_DQBUF: c_ulong = 0xc0585611;
pub const VIDIOC_STREAMON: c_ulong = 0x40045612;
pub const VIDIOC_STREAMOFF: c_ulong = 0x40045613;

pub const V4L2_BUF_TYPE_VIDEO_CAPTURE: u32 = 1;
pub const V4L2_MEMORY_MMAP: u32 = 1;
pub const V4L2_FIELD_ANY: u32 = 0;

// Pixel formats
pub const V4L2_PIX_FMT_GREY: u32 = u32::from_le_bytes(*b"GREY");
pub const V4L2_PIX_FMT_YUYV: u32 = u32::from_le_bytes(*b"YUYV");
pub const V4L2_PIX_FMT_MJPEG: u32 = u32::from_le_bytes(*b"MJPG");

#[repr(C)]
#[derive(Debug, Copy, Clone)]
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
#[derive(Debug, Copy, Clone)]
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
#[derive(Debug, Copy, Clone)]
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
    pub fmt: v4l2_format_union,
}

#[repr(C)]
#[derive(Copy, Clone)]
pub union v4l2_format_union {
    pub pix: v4l2_pix_format,
    pub raw_data: [c_uchar; 200],
}

#[repr(C)]
#[derive(Debug, Copy, Clone)]
pub struct v4l2_requestbuffers {
    pub count: u32,
    pub type_: u32,
    pub memory: u32,
    pub capabilities: u32,
    pub flags: u8,
    pub reserved: [u8; 3],
}

#[repr(C)]
#[derive(Copy, Clone)]
pub struct v4l2_timeval {
    pub tv_sec: i64,
    pub tv_usec: i64,
}

#[repr(C)]
#[derive(Copy, Clone)]
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
    pub m: v4l2_buffer_union,
    pub length: u32,
    pub reserved2: u32,
    pub request_fd: i32,
}

#[repr(C)]
#[derive(Copy, Clone)]
pub union v4l2_buffer_union {
    pub offset: u32,
    pub userptr: c_ulong,
    pub planes: *mut c_void,
    pub fd: i32,
}

extern "C" {
    pub fn ioctl(fd: c_int, request: c_ulong, ...) -> c_int;
}

pub unsafe fn xioctl(fd: c_int, request: c_ulong, arg: *mut c_void) -> c_int {
    let mut r;
    loop {
        r = ioctl(fd, request, arg);
        if r == -1 && *libc::__errno_location() == libc::EINTR {
            continue;
        }
        break;
    }
    r
}
