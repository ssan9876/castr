// crates/castr-codec-v4l2/src/sys.rs
//! The subset of <linux/videodev2.h> the decoder needs, written by hand so no
//! bindgen or kernel headers are needed at build time. Layouts are the 64-bit
//! Linux ABI (identical on aarch64 and x86_64) and are pinned by tests.
#![allow(non_camel_case_types, dead_code)]

pub const V4L2_BUF_TYPE_VIDEO_CAPTURE_MPLANE: u32 = 9;
pub const V4L2_BUF_TYPE_VIDEO_OUTPUT_MPLANE: u32 = 10;
pub const V4L2_MEMORY_MMAP: u32 = 1;
pub const V4L2_FIELD_NONE: u32 = 1;
pub const V4L2_CAP_VIDEO_M2M_MPLANE: u32 = 0x0000_4000;
pub const V4L2_CAP_STREAMING: u32 = 0x0400_0000;
pub const V4L2_CAP_DEVICE_CAPS: u32 = 0x8000_0000;
pub const V4L2_EVENT_EOS: u32 = 2;
pub const V4L2_EVENT_SOURCE_CHANGE: u32 = 5;
pub const V4L2_EVENT_SRC_CH_RESOLUTION: u32 = 1;
pub const V4L2_SEL_TGT_COMPOSE: u32 = 0x0100;
pub const V4L2_BUF_FLAG_LAST: u32 = 0x0010_0000;
pub const VIDEO_MAX_PLANES: usize = 8;

pub const fn fourcc(a: u8, b: u8, c: u8, d: u8) -> u32 {
    (a as u32) | ((b as u32) << 8) | ((c as u32) << 16) | ((d as u32) << 24)
}
pub const V4L2_PIX_FMT_H264: u32 = fourcc(b'H', b'2', b'6', b'4');
pub const V4L2_PIX_FMT_NV12: u32 = fourcc(b'N', b'V', b'1', b'2');

// _IOC encoding (asm-generic): dir:2 | size:14 | type:8 | nr:8.
const IOC_NONE: u32 = 0;
const IOC_WRITE: u32 = 1;
const IOC_READ: u32 = 2;
const fn ioc(dir: u32, nr: u32, size: usize) -> u64 {
    ((dir << 30) | ((size as u32) << 16) | ((b'V' as u32) << 8) | nr) as u64
}
const fn ior(nr: u32, size: usize) -> u64 {
    ioc(IOC_READ, nr, size)
}
const fn iow(nr: u32, size: usize) -> u64 {
    ioc(IOC_WRITE, nr, size)
}
const fn iowr(nr: u32, size: usize) -> u64 {
    ioc(IOC_READ | IOC_WRITE, nr, size)
}

use std::mem::size_of;
pub const VIDIOC_QUERYCAP: u64 = ior(0, size_of::<v4l2_capability>());
pub const VIDIOC_ENUM_FMT: u64 = iowr(2, size_of::<v4l2_fmtdesc>());
pub const VIDIOC_G_FMT: u64 = iowr(4, size_of::<v4l2_format>());
pub const VIDIOC_S_FMT: u64 = iowr(5, size_of::<v4l2_format>());
pub const VIDIOC_REQBUFS: u64 = iowr(8, size_of::<v4l2_requestbuffers>());
pub const VIDIOC_QUERYBUF: u64 = iowr(9, size_of::<v4l2_buffer>());
pub const VIDIOC_QBUF: u64 = iowr(15, size_of::<v4l2_buffer>());
pub const VIDIOC_EXPBUF: u64 = iowr(16, size_of::<v4l2_exportbuffer>());
pub const VIDIOC_DQBUF: u64 = iowr(17, size_of::<v4l2_buffer>());
pub const VIDIOC_STREAMON: u64 = iow(18, size_of::<i32>());
pub const VIDIOC_STREAMOFF: u64 = iow(19, size_of::<i32>());
pub const VIDIOC_DQEVENT: u64 = ior(89, size_of::<v4l2_event>());
pub const VIDIOC_SUBSCRIBE_EVENT: u64 = iow(90, size_of::<v4l2_event_subscription>());
pub const VIDIOC_G_SELECTION: u64 = iowr(94, size_of::<v4l2_selection>());

#[repr(C)]
#[derive(Clone, Copy)]
pub struct v4l2_capability {
    pub driver: [u8; 16],
    pub card: [u8; 32],
    pub bus_info: [u8; 32],
    pub version: u32,
    pub capabilities: u32,
    pub device_caps: u32,
    pub reserved: [u32; 3],
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct v4l2_fmtdesc {
    pub index: u32,
    pub type_: u32,
    pub flags: u32,
    pub description: [u8; 32],
    pub pixelformat: u32,
    pub mbus_code: u32,
    pub reserved: [u32; 3],
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct v4l2_plane_pix_format {
    pub sizeimage: u32,
    pub bytesperline: u32,
    pub reserved: [u16; 6],
}

/// `__attribute__((packed))` in the kernel header. Rust's default `repr(C)`
/// layout for this field set already has zero padding (every field up to
/// `num_planes` is 4-byte aligned and the trailing `u8`s need no padding to
/// reach a 4-byte-multiple size), so it is intentionally *not* `packed` here:
/// that lets callers index into `plane_fmt` and read/write fields directly
/// through a normal reference instead of `read_unaligned`/`write_unaligned`.
/// The byte layout matches the kernel's packed struct either way; the size
/// test below pins it.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct v4l2_pix_format_mplane {
    pub width: u32,
    pub height: u32,
    pub pixelformat: u32,
    pub field: u32,
    pub colorspace: u32,
    pub plane_fmt: [v4l2_plane_pix_format; VIDEO_MAX_PLANES],
    pub num_planes: u8,
    pub flags: u8,
    pub ycbcr_enc: u8,
    pub quantization: u8,
    pub xfer_func: u8,
    pub reserved: [u8; 7],
}

/// The `fmt` union: 200 bytes, 8-aligned because some members hold pointers.
#[repr(C, align(8))]
#[derive(Clone, Copy)]
pub struct v4l2_format_fmt {
    pub raw_data: [u8; 200],
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct v4l2_format {
    pub type_: u32,
    pub fmt: v4l2_format_fmt,
}

impl v4l2_format {
    pub fn new(type_: u32) -> Self {
        Self {
            type_,
            fmt: v4l2_format_fmt { raw_data: [0; 200] },
        }
    }
    /// Multiplanar view of `fmt`.
    pub fn pix_mp(&self) -> v4l2_pix_format_mplane {
        // SAFETY: raw_data is 200 bytes >= size_of::<v4l2_pix_format_mplane>()
        // (192), the union buffer is 8-aligned (>= the 4-byte alignment this
        // struct needs), and every bit pattern is a valid instance of it.
        unsafe { *(self.fmt.raw_data.as_ptr() as *const v4l2_pix_format_mplane) }
    }
    /// Mutable multiplanar view of `fmt`, writing straight through to the
    /// underlying union storage.
    pub fn pix_mp_mut(&mut self) -> &mut v4l2_pix_format_mplane {
        // SAFETY: see `pix_mp`.
        unsafe { &mut *(self.fmt.raw_data.as_mut_ptr() as *mut v4l2_pix_format_mplane) }
    }
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct v4l2_requestbuffers {
    pub count: u32,
    pub type_: u32,
    pub memory: u32,
    pub capabilities: u32,
    pub flags: u8,
    pub reserved: [u8; 3],
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct v4l2_timeval {
    pub tv_sec: i64,
    pub tv_usec: i64,
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct v4l2_timecode {
    pub type_: u32,
    pub flags: u32,
    pub frames: u8,
    pub seconds: u8,
    pub minutes: u8,
    pub hours: u8,
    pub userbits: [u8; 4],
}

/// `union { __u32 mem_offset; unsigned long userptr; __s32 fd; } m;`
#[repr(C)]
#[derive(Clone, Copy)]
pub union v4l2_plane_m {
    pub mem_offset: u32,
    pub userptr: u64,
    pub fd: i32,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct v4l2_plane {
    pub bytesused: u32,
    pub length: u32,
    pub m: v4l2_plane_m,
    pub data_offset: u32,
    pub reserved: [u32; 11],
}

impl Default for v4l2_plane {
    fn default() -> Self {
        // SAFETY: all-zero is a valid v4l2_plane.
        unsafe { std::mem::zeroed() }
    }
}

/// `union { __u32 offset; unsigned long userptr; struct v4l2_plane *planes; __s32 fd; } m;`
#[repr(C)]
#[derive(Clone, Copy)]
pub union v4l2_buffer_m {
    pub offset: u32,
    pub userptr: u64,
    pub planes: *mut v4l2_plane,
    pub fd: i32,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct v4l2_buffer {
    pub index: u32,
    pub type_: u32,
    pub bytesused: u32,
    pub flags: u32,
    pub field: u32,
    pub timestamp: v4l2_timeval,
    pub timecode: v4l2_timecode,
    pub sequence: u32,
    pub memory: u32,
    pub m: v4l2_buffer_m,
    pub length: u32,
    pub reserved2: u32,
    pub request_fd: i32,
}

impl v4l2_buffer {
    /// A zeroed multiplanar buffer descriptor pointing at `planes`.
    pub fn mplane(type_: u32, index: u32, planes: &mut [v4l2_plane]) -> Self {
        // SAFETY: all-zero is a valid v4l2_buffer; fields are then set.
        let mut b: v4l2_buffer = unsafe { std::mem::zeroed() };
        b.index = index;
        b.type_ = type_;
        b.memory = V4L2_MEMORY_MMAP;
        b.length = planes.len() as u32;
        b.m = v4l2_buffer_m {
            planes: planes.as_mut_ptr(),
        };
        b
    }
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct v4l2_exportbuffer {
    pub type_: u32,
    pub index: u32,
    pub plane: u32,
    pub flags: u32,
    pub fd: i32,
    pub reserved: [u32; 11],
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct v4l2_event_subscription {
    pub type_: u32,
    pub id: u32,
    pub flags: u32,
    pub reserved: [u32; 5],
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct v4l2_timespec {
    pub tv_sec: i64,
    pub tv_nsec: i64,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct v4l2_event {
    pub type_: u32,
    /// Union payload; for SOURCE_CHANGE the first u32 is `changes`.
    pub u: [u8; 64],
    pub pending: u32,
    pub sequence: u32,
    pub timestamp: v4l2_timespec,
    pub id: u32,
    pub reserved: [u32; 8],
}

impl v4l2_event {
    pub fn src_changes(&self) -> u32 {
        u32::from_ne_bytes([self.u[0], self.u[1], self.u[2], self.u[3]])
    }
}

#[repr(C)]
#[derive(Clone, Copy, Default, Debug, PartialEq, Eq)]
pub struct v4l2_rect {
    pub left: i32,
    pub top: i32,
    pub width: u32,
    pub height: u32,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct v4l2_selection {
    pub type_: u32,
    pub target: u32,
    pub flags: u32,
    pub r: v4l2_rect,
    pub reserved: [u32; 9],
}

pub fn zeroed<T: Copy>() -> T {
    // SAFETY: every struct in this module is plain data for which all-zero is valid.
    unsafe { std::mem::zeroed() }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::mem::size_of;

    // Sizes from <linux/videodev2.h> on 64-bit Linux (aarch64 and x86_64 agree).
    #[test]
    fn struct_sizes_match_the_kernel_abi() {
        assert_eq!(size_of::<v4l2_capability>(), 104);
        assert_eq!(size_of::<v4l2_fmtdesc>(), 64);
        assert_eq!(size_of::<v4l2_plane_pix_format>(), 20);
        assert_eq!(size_of::<v4l2_pix_format_mplane>(), 192);
        assert_eq!(size_of::<v4l2_format>(), 208);
        assert_eq!(size_of::<v4l2_requestbuffers>(), 20);
        assert_eq!(size_of::<v4l2_plane>(), 64);
        assert_eq!(size_of::<v4l2_buffer>(), 88);
        assert_eq!(size_of::<v4l2_exportbuffer>(), 64);
        assert_eq!(size_of::<v4l2_event_subscription>(), 32);
        assert_eq!(size_of::<v4l2_event>(), 136);
        assert_eq!(size_of::<v4l2_selection>(), 64);
    }

    #[test]
    fn ioctl_numbers_match_the_kernel() {
        // Values printed by `v4l2-ctl` sources / kernel headers on 64-bit Linux.
        assert_eq!(VIDIOC_QUERYCAP, 0x8068_5600);
        assert_eq!(VIDIOC_ENUM_FMT, 0xC040_5602);
        assert_eq!(VIDIOC_G_FMT, 0xC0D0_5604);
        assert_eq!(VIDIOC_S_FMT, 0xC0D0_5605);
        assert_eq!(VIDIOC_REQBUFS, 0xC014_5608);
        assert_eq!(VIDIOC_QUERYBUF, 0xC058_5609);
        assert_eq!(VIDIOC_QBUF, 0xC058_560F);
        assert_eq!(VIDIOC_EXPBUF, 0xC040_5610);
        assert_eq!(VIDIOC_DQBUF, 0xC058_5611);
        assert_eq!(VIDIOC_STREAMON, 0x4004_5612);
        assert_eq!(VIDIOC_STREAMOFF, 0x4004_5613);
        assert_eq!(VIDIOC_DQEVENT, 0x8088_5659);
        assert_eq!(VIDIOC_SUBSCRIBE_EVENT, 0x4020_565A);
        assert_eq!(VIDIOC_G_SELECTION, 0xC040_565E);
    }

    #[test]
    fn fourcc_matches_v4l2() {
        assert_eq!(V4L2_PIX_FMT_H264, 0x3436_3248);
        assert_eq!(V4L2_PIX_FMT_NV12, 0x3231_564E);
    }

    #[test]
    fn format_pix_mp_view_round_trips() {
        let mut f = v4l2_format::new(V4L2_BUF_TYPE_VIDEO_CAPTURE_MPLANE);
        {
            let mp = f.pix_mp_mut();
            mp.width = 1920;
            mp.height = 1088;
            mp.pixelformat = V4L2_PIX_FMT_NV12;
            mp.num_planes = 1;
            mp.plane_fmt[0].bytesperline = 1920;
            mp.plane_fmt[0].sizeimage = 1920 * 1088 * 3 / 2;
        }
        let mp = f.pix_mp();
        assert_eq!((mp.width, mp.height, mp.num_planes), (1920, 1088, 1));
        assert_eq!(mp.plane_fmt[0].bytesperline, 1920);
        assert_eq!(f.type_, V4L2_BUF_TYPE_VIDEO_CAPTURE_MPLANE);
    }
}
