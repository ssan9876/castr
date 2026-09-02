//! Typed V4L2 operations. `RealOps` talks to the kernel through libc;
//! `FakeOps` (tests) scripts the same calls so the queue and decoder state
//! machines are testable without hardware.

use crate::sys::*;
use std::fs::{File, OpenOptions};
use std::io;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::os::unix::fs::OpenOptionsExt;

pub struct PlaneInfo {
    pub length: u32,
    pub mem_offset: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Dequeued {
    pub index: u32,
    pub bytesused: u32,
    pub timestamp_us: u64,
    pub flags: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Event {
    pub kind: u32,
    pub changes: u32,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PollResult {
    pub readable: bool,
    pub writable: bool,
    pub event: bool,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct FormatInfo {
    pub width: u32,
    pub height: u32,
    pub pixelformat: u32,
    pub bytesperline: u32,
    pub sizeimage: u32,
}

/// One mapped plane. Real mappings are unmapped on drop; test mappings own a Vec.
pub struct Mapping {
    ptr: *mut u8,
    len: usize,
    owned: Option<Vec<u8>>,
}

// SAFETY: the mapping is only ever used from the decode thread; `Send` lets the
// decoder move between threads at construction.
unsafe impl Send for Mapping {}

impl Mapping {
    #[cfg(test)]
    pub(crate) fn owned(len: usize) -> Self {
        let mut v = vec![0u8; len];
        let ptr = v.as_mut_ptr();
        Self {
            ptr,
            len,
            owned: Some(v),
        }
    }
    pub fn len(&self) -> usize {
        self.len
    }
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }
    pub fn as_slice(&self) -> &[u8] {
        // SAFETY: ptr/len describe a live mapping (or Vec) for the life of self.
        unsafe { std::slice::from_raw_parts(self.ptr, self.len) }
    }
    pub fn as_mut_slice(&mut self) -> &mut [u8] {
        // SAFETY: as above; &mut self guarantees exclusivity.
        unsafe { std::slice::from_raw_parts_mut(self.ptr, self.len) }
    }
}

impl Drop for Mapping {
    fn drop(&mut self) {
        if self.owned.is_none() && !self.ptr.is_null() {
            // SAFETY: ptr/len came from a successful mmap in RealOps::mmap.
            unsafe {
                libc::munmap(self.ptr as *mut libc::c_void, self.len);
            }
        }
    }
}

pub trait Ops {
    fn query_cap(&mut self) -> io::Result<v4l2_capability>;
    /// Pixel format at `index` for the queue type, or None past the end.
    fn enum_fmt(&mut self, buf_type: u32, index: u32) -> io::Result<Option<u32>>;
    fn g_fmt(&mut self, buf_type: u32) -> io::Result<FormatInfo>;
    /// Sets the format; returns what the driver actually accepted.
    fn s_fmt(&mut self, buf_type: u32, want: &FormatInfo) -> io::Result<FormatInfo>;
    /// Returns the count granted (drivers may round up).
    fn reqbufs(&mut self, buf_type: u32, count: u32) -> io::Result<u32>;
    fn querybuf(&mut self, buf_type: u32, index: u32) -> io::Result<PlaneInfo>;
    fn mmap(&mut self, length: usize, offset: u32) -> io::Result<Mapping>;
    fn expbuf(&mut self, buf_type: u32, index: u32) -> io::Result<OwnedFd>;
    fn qbuf(
        &mut self,
        buf_type: u32,
        index: u32,
        length: u32,
        bytesused: u32,
        timestamp_us: u64,
    ) -> io::Result<()>;
    /// None when nothing is ready (EAGAIN).
    fn dqbuf(&mut self, buf_type: u32) -> io::Result<Option<Dequeued>>;
    fn streamon(&mut self, buf_type: u32) -> io::Result<()>;
    fn streamoff(&mut self, buf_type: u32) -> io::Result<()>;
    fn subscribe(&mut self, event_type: u32) -> io::Result<()>;
    /// None when no event is pending (ENOENT).
    fn dqevent(&mut self) -> io::Result<Option<Event>>;
    fn poll(&mut self, timeout_ms: i32) -> io::Result<PollResult>;
    fn g_selection_compose(&mut self, buf_type: u32) -> io::Result<v4l2_rect>;
}

pub struct RealOps {
    file: File,
}

impl RealOps {
    pub fn open(path: &str) -> io::Result<Self> {
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .custom_flags(libc::O_NONBLOCK)
            .open(path)?;
        Ok(Self { file })
    }

    fn fd(&self) -> libc::c_int {
        self.file.as_raw_fd()
    }

    fn ioctl<T>(&self, req: u64, arg: &mut T) -> io::Result<()> {
        // SAFETY: `req` and `T` are paired by the constants in sys.rs, so the
        // kernel reads/writes exactly size_of::<T>() bytes at `arg`.
        let r = unsafe { libc::ioctl(self.fd(), req as _, arg as *mut T) };
        if r < 0 {
            Err(io::Error::last_os_error())
        } else {
            Ok(())
        }
    }
}

fn split_ts(ts_us: u64) -> v4l2_timeval {
    v4l2_timeval {
        tv_sec: (ts_us / 1_000_000) as i64,
        tv_usec: (ts_us % 1_000_000) as i64,
    }
}

fn join_ts(tv: v4l2_timeval) -> u64 {
    (tv.tv_sec.max(0) as u64) * 1_000_000 + (tv.tv_usec.max(0) as u64)
}

impl Ops for RealOps {
    fn query_cap(&mut self) -> io::Result<v4l2_capability> {
        let mut cap: v4l2_capability = zeroed();
        self.ioctl(VIDIOC_QUERYCAP, &mut cap)?;
        Ok(cap)
    }

    fn enum_fmt(&mut self, buf_type: u32, index: u32) -> io::Result<Option<u32>> {
        let mut d: v4l2_fmtdesc = zeroed();
        d.type_ = buf_type;
        d.index = index;
        match self.ioctl(VIDIOC_ENUM_FMT, &mut d) {
            Ok(()) => Ok(Some(d.pixelformat)),
            Err(e) if e.raw_os_error() == Some(libc::EINVAL) => Ok(None),
            Err(e) => Err(e),
        }
    }

    fn g_fmt(&mut self, buf_type: u32) -> io::Result<FormatInfo> {
        let mut f = v4l2_format::new(buf_type);
        self.ioctl(VIDIOC_G_FMT, &mut f)?;
        let mp = f.pix_mp();
        let pf = mp.plane_fmt[0];
        Ok(FormatInfo {
            width: mp.width,
            height: mp.height,
            pixelformat: mp.pixelformat,
            bytesperline: pf.bytesperline,
            sizeimage: pf.sizeimage,
        })
    }

    fn s_fmt(&mut self, buf_type: u32, want: &FormatInfo) -> io::Result<FormatInfo> {
        let mut f = v4l2_format::new(buf_type);
        {
            let mp = f.pix_mp_mut();
            mp.width = want.width;
            mp.height = want.height;
            mp.pixelformat = want.pixelformat;
            mp.field = V4L2_FIELD_NONE;
            mp.num_planes = 1;
            mp.plane_fmt[0].sizeimage = want.sizeimage;
            mp.plane_fmt[0].bytesperline = want.bytesperline;
        }
        self.ioctl(VIDIOC_S_FMT, &mut f)?;
        let mp = f.pix_mp();
        let pf = mp.plane_fmt[0];
        Ok(FormatInfo {
            width: mp.width,
            height: mp.height,
            pixelformat: mp.pixelformat,
            bytesperline: pf.bytesperline,
            sizeimage: pf.sizeimage,
        })
    }

    fn reqbufs(&mut self, buf_type: u32, count: u32) -> io::Result<u32> {
        let mut r: v4l2_requestbuffers = zeroed();
        r.count = count;
        r.type_ = buf_type;
        r.memory = V4L2_MEMORY_MMAP;
        self.ioctl(VIDIOC_REQBUFS, &mut r)?;
        Ok(r.count)
    }

    fn querybuf(&mut self, buf_type: u32, index: u32) -> io::Result<PlaneInfo> {
        let mut planes = [v4l2_plane::default(); 1];
        let mut b = v4l2_buffer::mplane(buf_type, index, &mut planes);
        self.ioctl(VIDIOC_QUERYBUF, &mut b)?;
        // SAFETY: MMAP memory, so `mem_offset` is the active union member.
        let off = unsafe { planes[0].m.mem_offset };
        Ok(PlaneInfo {
            length: planes[0].length,
            mem_offset: off,
        })
    }

    fn mmap(&mut self, length: usize, offset: u32) -> io::Result<Mapping> {
        // SAFETY: standard shared read/write mapping of a V4L2 buffer.
        let p = unsafe {
            libc::mmap(
                std::ptr::null_mut(),
                length,
                libc::PROT_READ | libc::PROT_WRITE,
                libc::MAP_SHARED,
                self.fd(),
                offset as libc::off_t,
            )
        };
        if p == libc::MAP_FAILED {
            return Err(io::Error::last_os_error());
        }
        Ok(Mapping {
            ptr: p as *mut u8,
            len: length,
            owned: None,
        })
    }

    fn expbuf(&mut self, buf_type: u32, index: u32) -> io::Result<OwnedFd> {
        let mut e: v4l2_exportbuffer = zeroed();
        e.type_ = buf_type;
        e.index = index;
        e.flags = libc::O_RDWR as u32 | libc::O_CLOEXEC as u32;
        self.ioctl(VIDIOC_EXPBUF, &mut e)?;
        // SAFETY: the kernel just handed us a fresh fd we own.
        Ok(unsafe { OwnedFd::from_raw_fd(e.fd) })
    }

    fn qbuf(
        &mut self,
        buf_type: u32,
        index: u32,
        length: u32,
        bytesused: u32,
        timestamp_us: u64,
    ) -> io::Result<()> {
        let mut planes = [v4l2_plane::default(); 1];
        planes[0].length = length;
        planes[0].bytesused = bytesused;
        let mut b = v4l2_buffer::mplane(buf_type, index, &mut planes);
        b.field = V4L2_FIELD_NONE;
        b.timestamp = split_ts(timestamp_us);
        self.ioctl(VIDIOC_QBUF, &mut b)
    }

    fn dqbuf(&mut self, buf_type: u32) -> io::Result<Option<Dequeued>> {
        let mut planes = [v4l2_plane::default(); 1];
        let mut b = v4l2_buffer::mplane(buf_type, 0, &mut planes);
        match self.ioctl(VIDIOC_DQBUF, &mut b) {
            Ok(()) => Ok(Some(Dequeued {
                index: b.index,
                bytesused: planes[0].bytesused,
                timestamp_us: join_ts(b.timestamp),
                flags: b.flags,
            })),
            Err(e) if e.raw_os_error() == Some(libc::EAGAIN) => Ok(None),
            Err(e) => Err(e),
        }
    }

    fn streamon(&mut self, buf_type: u32) -> io::Result<()> {
        let mut t = buf_type as i32;
        self.ioctl(VIDIOC_STREAMON, &mut t)
    }

    fn streamoff(&mut self, buf_type: u32) -> io::Result<()> {
        let mut t = buf_type as i32;
        self.ioctl(VIDIOC_STREAMOFF, &mut t)
    }

    fn subscribe(&mut self, event_type: u32) -> io::Result<()> {
        let mut s: v4l2_event_subscription = zeroed();
        s.type_ = event_type;
        self.ioctl(VIDIOC_SUBSCRIBE_EVENT, &mut s)
    }

    fn dqevent(&mut self) -> io::Result<Option<Event>> {
        let mut e: v4l2_event = zeroed();
        match self.ioctl(VIDIOC_DQEVENT, &mut e) {
            Ok(()) => Ok(Some(Event {
                kind: e.type_,
                changes: e.src_changes(),
            })),
            Err(err) if err.raw_os_error() == Some(libc::ENOENT) => Ok(None),
            Err(err) => Err(err),
        }
    }

    fn poll(&mut self, timeout_ms: i32) -> io::Result<PollResult> {
        let mut pfd = libc::pollfd {
            fd: self.fd(),
            events: libc::POLLIN | libc::POLLOUT | libc::POLLPRI,
            revents: 0,
        };
        // SAFETY: one valid pollfd.
        let r = unsafe { libc::poll(&mut pfd, 1, timeout_ms) };
        if r < 0 {
            let e = io::Error::last_os_error();
            if e.kind() == io::ErrorKind::Interrupted {
                return Ok(PollResult::default());
            }
            return Err(e);
        }
        Ok(PollResult {
            readable: pfd.revents & libc::POLLIN != 0,
            writable: pfd.revents & libc::POLLOUT != 0,
            event: pfd.revents & libc::POLLPRI != 0,
        })
    }

    fn g_selection_compose(&mut self, buf_type: u32) -> io::Result<v4l2_rect> {
        let mut s: v4l2_selection = zeroed();
        s.type_ = buf_type;
        s.target = V4L2_SEL_TGT_COMPOSE;
        self.ioctl(VIDIOC_G_SELECTION, &mut s)?;
        Ok(s.r)
    }
}
