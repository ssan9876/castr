//! Scripted stand-in for the kernel, for unit tests of the queue and decoder
//! state machines. Records every call as a string and serves queued
//! dequeue results, events and formats that tests push in.

use crate::ops::*;
use crate::sys::*;
use std::collections::VecDeque;
use std::io;
use std::os::fd::OwnedFd;
use std::sync::{Arc, Mutex};

pub(crate) struct FakeOps {
    pub calls: Vec<String>,
    pub caps: u32,
    pub output_formats: Vec<u32>,
    pub capture_formats: Vec<u32>,
    pub capture_format: FormatInfo,
    pub output_format: FormatInfo,
    pub compose: v4l2_rect,
    pub granted: Option<u32>,
    dequeues: VecDeque<(u32, Dequeue)>,
    events: VecDeque<Event>,
    pub polls: VecDeque<PollResult>,
    pub fail_next: Option<&'static str>,
    pub captures_filled: Vec<u8>,
    /// Value served for V4L2_CID_MIN_BUFFERS_FOR_CAPTURE.
    pub min_capture_buffers: i32,
    /// Virtual clock; only `advance` moves it.
    clock: std::time::Instant,
    pub sink: Option<Arc<Mutex<Vec<String>>>>,
}

pub(crate) trait HasSink {
    fn set_sink(&mut self, sink: Arc<Mutex<Vec<String>>>);
}

impl HasSink for FakeOps {
    fn set_sink(&mut self, sink: Arc<Mutex<Vec<String>>>) {
        self.sink = Some(sink);
    }
}

impl Drop for FakeOps {
    fn drop(&mut self) {
        if let Some(s) = &self.sink {
            *s.lock().unwrap() = self.calls.clone();
        }
    }
}

impl FakeOps {
    pub fn new() -> Self {
        Self {
            calls: Vec::new(),
            caps: V4L2_CAP_VIDEO_M2M_MPLANE | V4L2_CAP_STREAMING | V4L2_CAP_DEVICE_CAPS,
            output_formats: vec![V4L2_PIX_FMT_H264],
            capture_formats: vec![V4L2_PIX_FMT_NV12],
            capture_format: FormatInfo {
                width: 640,
                height: 368,
                pixelformat: V4L2_PIX_FMT_NV12,
                bytesperline: 640,
                sizeimage: 640 * 368 * 3 / 2,
            },
            output_format: FormatInfo {
                pixelformat: V4L2_PIX_FMT_H264,
                sizeimage: 1 << 20,
                ..Default::default()
            },
            compose: v4l2_rect {
                left: 0,
                top: 0,
                width: 640,
                height: 360,
            },
            granted: None,
            dequeues: VecDeque::new(),
            events: VecDeque::new(),
            polls: VecDeque::new(),
            fail_next: None,
            captures_filled: Vec::new(),
            min_capture_buffers: 1,
            clock: std::time::Instant::now(),
            sink: None,
        }
    }
    /// Move the virtual clock forward.
    pub fn advance(&mut self, d: std::time::Duration) {
        self.clock += d;
    }
    pub fn push_dequeue(&mut self, buf_type: u32, d: Dequeued) {
        self.dequeues.push_back((buf_type, Dequeue::Buffer(d)));
    }
    /// Script the EPIPE the kernel returns once the sequence is finished.
    pub fn push_end_of_sequence(&mut self, buf_type: u32) {
        self.dequeues.push_back((buf_type, Dequeue::EndOfSequence));
    }
    pub fn push_event(&mut self, e: Event) {
        self.events.push_back(e);
    }
    fn record(&mut self, s: String) -> io::Result<()> {
        self.calls.push(s);
        if let Some(name) = self.fail_next.take() {
            return Err(io::Error::other(name));
        }
        Ok(())
    }
}

impl Ops for FakeOps {
    fn query_cap(&mut self) -> io::Result<v4l2_capability> {
        self.record("query_cap".into())?;
        let mut c: v4l2_capability = zeroed();
        c.capabilities = self.caps;
        c.device_caps = self.caps & !V4L2_CAP_DEVICE_CAPS;
        c.driver[..13].copy_from_slice(b"bcm2835-codec");
        Ok(c)
    }
    fn enum_fmt(&mut self, buf_type: u32, index: u32) -> io::Result<Option<u32>> {
        self.record(format!("enum_fmt({buf_type},{index})"))?;
        let list = if buf_type == V4L2_BUF_TYPE_VIDEO_OUTPUT_MPLANE {
            &self.output_formats
        } else {
            &self.capture_formats
        };
        Ok(list.get(index as usize).copied())
    }
    fn g_fmt(&mut self, buf_type: u32) -> io::Result<FormatInfo> {
        self.record(format!("g_fmt({buf_type})"))?;
        Ok(if buf_type == V4L2_BUF_TYPE_VIDEO_OUTPUT_MPLANE {
            self.output_format
        } else {
            self.capture_format
        })
    }
    fn s_fmt(&mut self, buf_type: u32, want: &FormatInfo) -> io::Result<FormatInfo> {
        self.record(format!(
            "s_fmt({buf_type},{:#x},{}x{})",
            want.pixelformat, want.width, want.height
        ))?;
        if buf_type == V4L2_BUF_TYPE_VIDEO_OUTPUT_MPLANE {
            self.output_format = FormatInfo {
                sizeimage: want.sizeimage,
                ..*want
            };
            Ok(self.output_format)
        } else {
            self.capture_format.pixelformat = want.pixelformat;
            Ok(self.capture_format)
        }
    }
    fn reqbufs(&mut self, buf_type: u32, count: u32) -> io::Result<u32> {
        self.record(format!("reqbufs({buf_type},{count})"))?;
        Ok(if count == 0 {
            0
        } else {
            self.granted.unwrap_or(count)
        })
    }
    fn querybuf(&mut self, buf_type: u32, index: u32) -> io::Result<PlaneInfo> {
        self.record(format!("querybuf({buf_type},{index})"))?;
        let len = if buf_type == V4L2_BUF_TYPE_VIDEO_OUTPUT_MPLANE {
            self.output_format.sizeimage
        } else {
            self.capture_format.sizeimage
        };
        Ok(PlaneInfo {
            length: len,
            mem_offset: index * 0x1000,
        })
    }
    fn mmap(&mut self, length: usize, offset: u32) -> io::Result<Mapping> {
        self.record(format!("mmap({length},{offset})"))?;
        let mut m = Mapping::owned(length);
        if !self.captures_filled.is_empty() {
            // Fill positionally (not a single repeated byte) so tests that
            // read specific offsets can actually detect a wrong stride/base.
            for (i, b) in m.as_mut_slice().iter_mut().enumerate() {
                *b = (i % 251) as u8;
            }
        }
        Ok(m)
    }
    fn expbuf(&mut self, buf_type: u32, index: u32) -> io::Result<OwnedFd> {
        self.record(format!("expbuf({buf_type},{index})"))?;
        // A real fd so OwnedFd's drop is sound: /dev/null is always openable on Linux.
        Ok(OwnedFd::from(std::fs::File::open("/dev/null")?))
    }
    fn qbuf(
        &mut self,
        buf_type: u32,
        index: u32,
        _length: u32,
        bytesused: u32,
        timestamp_us: u64,
    ) -> io::Result<()> {
        self.record(format!(
            "qbuf({buf_type},{index},{bytesused},{timestamp_us})"
        ))
    }
    fn dqbuf(&mut self, buf_type: u32) -> io::Result<Dequeue> {
        self.record(format!("dqbuf({buf_type})"))?;
        if let Some(pos) = self.dequeues.iter().position(|(t, _)| *t == buf_type) {
            return Ok(self.dequeues.remove(pos).unwrap().1);
        }
        Ok(Dequeue::Idle)
    }
    fn streamon(&mut self, buf_type: u32) -> io::Result<()> {
        self.record(format!("streamon({buf_type})"))
    }
    fn streamoff(&mut self, buf_type: u32) -> io::Result<()> {
        self.record(format!("streamoff({buf_type})"))
    }
    fn subscribe(&mut self, event_type: u32) -> io::Result<()> {
        self.record(format!("subscribe({event_type})"))
    }
    fn dqevent(&mut self) -> io::Result<Option<Event>> {
        self.record("dqevent".into())?;
        Ok(self.events.pop_front())
    }
    fn poll(&mut self, timeout_ms: i32) -> io::Result<PollResult> {
        self.record(format!("poll({timeout_ms})"))?;
        Ok(self.polls.pop_front().unwrap_or_default())
    }
    fn now(&mut self) -> std::time::Instant {
        self.clock
    }
    fn g_ctrl(&mut self, id: u32) -> io::Result<i32> {
        self.record(format!("g_ctrl({id:#x})"))?;
        if id == V4L2_CID_MIN_BUFFERS_FOR_CAPTURE {
            Ok(self.min_capture_buffers)
        } else {
            Err(io::Error::from_raw_os_error(libc::EINVAL))
        }
    }
    fn g_selection_compose(&mut self, buf_type: u32) -> io::Result<v4l2_rect> {
        self.record(format!("g_selection({buf_type})"))?;
        Ok(self.compose)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ops::Ops;

    #[test]
    fn fake_records_calls_and_serves_scripted_capture_buffers() {
        let mut f = FakeOps::new();
        assert_eq!(f.reqbufs(V4L2_BUF_TYPE_VIDEO_OUTPUT_MPLANE, 4).unwrap(), 4);
        let m = f.mmap(16, 0).unwrap();
        assert_eq!(m.len(), 16);
        f.qbuf(V4L2_BUF_TYPE_VIDEO_OUTPUT_MPLANE, 1, 16, 5, 1000)
            .unwrap();
        assert_eq!(
            f.calls,
            vec!["reqbufs(10,4)", "mmap(16,0)", "qbuf(10,1,5,1000)"]
        );
        // Nothing scripted: dequeue is EAGAIN -> Idle.
        assert_eq!(
            f.dqbuf(V4L2_BUF_TYPE_VIDEO_OUTPUT_MPLANE).unwrap(),
            Dequeue::Idle
        );
        f.push_dequeue(
            V4L2_BUF_TYPE_VIDEO_OUTPUT_MPLANE,
            Dequeued {
                index: 1,
                bytesused: 0,
                timestamp_us: 1000,
                flags: 0,
            },
        );
        let Dequeue::Buffer(d) = f.dqbuf(V4L2_BUF_TYPE_VIDEO_OUTPUT_MPLANE).unwrap() else {
            panic!("expected a buffer")
        };
        assert_eq!(d.index, 1);
        f.push_end_of_sequence(V4L2_BUF_TYPE_VIDEO_CAPTURE_MPLANE);
        assert_eq!(
            f.dqbuf(V4L2_BUF_TYPE_VIDEO_CAPTURE_MPLANE).unwrap(),
            Dequeue::EndOfSequence
        );
    }

    #[test]
    fn fake_serves_events_and_formats() {
        let mut f = FakeOps::new();
        assert!(f.dqevent().unwrap().is_none());
        f.push_event(Event {
            kind: V4L2_EVENT_SOURCE_CHANGE,
            changes: V4L2_EVENT_SRC_CH_RESOLUTION,
        });
        assert_eq!(f.dqevent().unwrap().unwrap().kind, V4L2_EVENT_SOURCE_CHANGE);
        f.capture_format = FormatInfo {
            width: 640,
            height: 368,
            pixelformat: V4L2_PIX_FMT_NV12,
            bytesperline: 640,
            sizeimage: 640 * 368 * 3 / 2,
        };
        f.compose = v4l2_rect {
            left: 0,
            top: 0,
            width: 640,
            height: 360,
        };
        assert_eq!(
            f.g_fmt(V4L2_BUF_TYPE_VIDEO_CAPTURE_MPLANE).unwrap().height,
            368
        );
        assert_eq!(
            f.g_selection_compose(V4L2_BUF_TYPE_VIDEO_CAPTURE_MPLANE)
                .unwrap()
                .height,
            360
        );
    }
}
