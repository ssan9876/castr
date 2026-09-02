//! `V4l2Decoder`: H.264 in, NV12 out, over the bcm2835 V4L2 M2M decoder.
//! See spec section 4 for the sequence this implements.

use crate::annexb;
use crate::ops::{FormatInfo, Ops, RealOps};
use crate::queue::Queue;
use crate::sys::*;
use anyhow::{anyhow, bail, Context};
use castr_media::{PixelFormat, RawFrame, VideoDecoder};

pub const DEFAULT_DEVICE: &str = "/dev/video10";
pub const OUTPUT_BUFFERS: u32 = 4;
pub const OUTPUT_BUFFER_SIZE: u32 = 1 << 20;
pub const CAPTURE_BUFFERS: u32 = 6;
/// Compressed frames queued but not yet consumed; bounds decoder latency.
pub const MAX_IN_FLIGHT: usize = 2;
/// Inputs queued with no picture back before we declare the decoder dead.
pub const STALL_INPUTS: u32 = 60;
/// Wall-clock budget for waiting on the hardware inside one `decode` call.
pub const STALL_POLL_MS: u64 = 2_000;
const POLL_STEP_MS: i32 = 200;

struct Capture {
    queue: Queue,
    /// Coded (allocated) size and stride reported by the driver.
    coded: FormatInfo,
    /// Visible picture inside the coded frame.
    visible: v4l2_rect,
}

pub struct V4l2Decoder<O: Ops = RealOps> {
    pub(crate) ops: O,
    pub(crate) output: Queue,
    capture: Option<Capture>,
    unanswered: u32,
    finished: bool,
}

impl V4l2Decoder<RealOps> {
    pub fn open() -> anyhow::Result<Self> {
        let path = std::env::var("CASTR_V4L2_DEVICE").unwrap_or_else(|_| DEFAULT_DEVICE.into());
        Self::open_path(&path)
    }

    pub fn open_path(path: &str) -> anyhow::Result<Self> {
        let ops = RealOps::open(path).with_context(|| format!("open {path}"))?;
        Self::with_ops(ops).with_context(|| format!("initialise {path}"))
    }
}

impl<O: Ops> V4l2Decoder<O> {
    pub fn with_ops(mut ops: O) -> anyhow::Result<Self> {
        let cap = ops.query_cap().context("QUERYCAP")?;
        let caps = if cap.capabilities & V4L2_CAP_DEVICE_CAPS != 0 {
            cap.device_caps
        } else {
            cap.capabilities
        };
        if caps & V4L2_CAP_VIDEO_M2M_MPLANE == 0 {
            bail!("device is not a multiplanar memory-to-memory codec (caps {caps:#x})");
        }
        if !Self::supports(
            &mut ops,
            V4L2_BUF_TYPE_VIDEO_OUTPUT_MPLANE,
            V4L2_PIX_FMT_H264,
        )? {
            bail!("device does not accept H.264");
        }
        if !Self::supports(
            &mut ops,
            V4L2_BUF_TYPE_VIDEO_CAPTURE_MPLANE,
            V4L2_PIX_FMT_NV12,
        )? {
            bail!("device does not produce NV12");
        }
        ops.s_fmt(
            V4L2_BUF_TYPE_VIDEO_OUTPUT_MPLANE,
            &FormatInfo {
                pixelformat: V4L2_PIX_FMT_H264,
                sizeimage: OUTPUT_BUFFER_SIZE,
                ..Default::default()
            },
        )
        .context("S_FMT output")?;
        ops.subscribe(V4L2_EVENT_SOURCE_CHANGE)
            .context("subscribe SOURCE_CHANGE")?;
        ops.subscribe(V4L2_EVENT_EOS).context("subscribe EOS")?;
        let mut output = Queue::new(V4L2_BUF_TYPE_VIDEO_OUTPUT_MPLANE);
        output
            .allocate(&mut ops, OUTPUT_BUFFERS, false)
            .context("allocate output buffers")?;
        if output.buffers.len() < MAX_IN_FLIGHT {
            bail!(
                "driver granted only {} output buffer(s), need at least {MAX_IN_FLIGHT}",
                output.buffers.len()
            );
        }
        if let Some(b) = output
            .buffers
            .iter()
            .find(|b| b.mapping.len() < OUTPUT_BUFFER_SIZE as usize)
        {
            bail!(
                "driver granted an output buffer of {} bytes, need at least {OUTPUT_BUFFER_SIZE}",
                b.mapping.len()
            );
        }
        output.stream_on(&mut ops).context("STREAMON output")?;
        Ok(Self {
            ops,
            output,
            capture: None,
            unanswered: 0,
            finished: false,
        })
    }

    fn supports(ops: &mut O, buf_type: u32, fourcc: u32) -> anyhow::Result<bool> {
        for i in 0.. {
            match ops.enum_fmt(buf_type, i).context("ENUM_FMT")? {
                Some(f) if f == fourcc => return Ok(true),
                Some(_) => continue,
                None => return Ok(false),
            }
        }
        Ok(false)
    }

    /// Visible (width, height) once the stream's SPS has been parsed.
    pub fn frame_size(&self) -> Option<(u32, u32)> {
        self.capture
            .as_ref()
            .map(|c| (c.visible.width, c.visible.height))
    }

    /// (Re)build the CAPTURE side after a SOURCE_CHANGE (spec 4.2 step 5, 4.4).
    fn reconfigure_capture(&mut self) -> anyhow::Result<()> {
        if let Some(mut old) = self.capture.take() {
            old.queue
                .release(&mut self.ops)
                .context("release old capture buffers")?;
        }
        let cap_type = V4L2_BUF_TYPE_VIDEO_CAPTURE_MPLANE;
        let reported = self.ops.g_fmt(cap_type).context("G_FMT capture")?;
        let coded = self
            .ops
            .s_fmt(
                cap_type,
                &FormatInfo {
                    pixelformat: V4L2_PIX_FMT_NV12,
                    ..reported
                },
            )
            .context("S_FMT capture")?;
        if coded.pixelformat != V4L2_PIX_FMT_NV12 {
            bail!("driver refused NV12 (gave {:#x})", coded.pixelformat);
        }
        let mut queue = Queue::new(cap_type);
        // If allocation, queueing, or STREAMON fails partway through, release
        // whatever was set up so a half-built capture queue isn't leaked.
        if let Err(e) = self.reconfigure_capture_allocate(&mut queue) {
            let _ = queue.release(&mut self.ops);
            return Err(e);
        }
        let mut visible = self.ops.g_selection_compose(cap_type).unwrap_or(v4l2_rect {
            left: 0,
            top: 0,
            width: coded.width,
            height: coded.height,
        });
        if visible.width == 0 || visible.height == 0 {
            visible = v4l2_rect {
                left: 0,
                top: 0,
                width: coded.width,
                height: coded.height,
            };
        }
        tracing::info!(
            "v4l2 decoder: {}x{} visible in {}x{} coded, stride {}",
            visible.width,
            visible.height,
            coded.width,
            coded.height,
            coded.bytesperline
        );
        self.capture = Some(Capture {
            queue,
            coded,
            visible,
        });
        Ok(())
    }

    fn reconfigure_capture_allocate(&mut self, queue: &mut Queue) -> anyhow::Result<()> {
        queue
            .allocate(&mut self.ops, CAPTURE_BUFFERS, true)
            .context("allocate capture buffers")?;
        for i in 0..queue.buffers.len() {
            queue
                .queue(&mut self.ops, i, 0, 0)
                .context("queue capture buffer")?;
        }
        queue.stream_on(&mut self.ops).context("STREAMON capture")?;
        Ok(())
    }

    fn drain_events(&mut self) -> anyhow::Result<()> {
        while let Some(ev) = self.ops.dqevent().context("DQEVENT")? {
            match ev.kind {
                V4L2_EVENT_SOURCE_CHANGE => {
                    if ev.changes & V4L2_EVENT_SRC_CH_RESOLUTION != 0 || self.capture.is_none() {
                        self.reconfigure_capture()?;
                    }
                }
                V4L2_EVENT_EOS => self.finished = true,
                _ => {}
            }
        }
        Ok(())
    }

    fn drain_output(&mut self) -> anyhow::Result<bool> {
        let mut any = false;
        while self
            .output
            .dequeue(&mut self.ops)
            .context("DQBUF output")?
            .is_some()
        {
            any = true;
        }
        Ok(any)
    }

    /// Copy one decoded picture out of a capture buffer and requeue it.
    fn take_capture(&mut self) -> anyhow::Result<Option<RawFrame>> {
        let Some(cap) = self.capture.as_mut() else {
            return Ok(None);
        };
        let Some(d) = cap.queue.dequeue(&mut self.ops).context("DQBUF capture")? else {
            return Ok(None);
        };
        let idx = d.index as usize;
        let frame = if d.bytesused == 0 {
            Ok(None)
        } else {
            let (w, h) = (cap.visible.width as usize, cap.visible.height as usize);
            let stride = cap.coded.bytesperline as usize;
            let coded_h = cap.coded.height as usize;
            let top = cap.visible.top.max(0) as usize;
            let left = cap.visible.left.max(0) as usize;
            let src = cap.queue.buffers[idx].mapping.as_slice();
            // `sizeimage` (QUERYBUF, which sized this mapping) and
            // `bytesperline`/`height` (S_FMT) come from separate driver
            // reports; a bad compose rect or a driver that disagrees with
            // itself must not be allowed to index past the mapping.
            if stride * coded_h * 3 / 2 > src.len() || top + h > coded_h || left + w > stride {
                Err(anyhow!(
                    "capture geometry out of bounds: visible {w}x{h} at ({left},{top}) in {stride}x{coded_h} coded, mapping {} bytes",
                    src.len()
                ))
            } else {
                let mut data = Vec::with_capacity(w * h * 3 / 2);
                for row in 0..h {
                    let o = (top + row) * stride + left;
                    data.extend_from_slice(&src[o..o + w]);
                }
                let uv_base = stride * coded_h;
                for row in 0..h / 2 {
                    let o = uv_base + (top / 2 + row) * stride + left;
                    data.extend_from_slice(&src[o..o + w]);
                }
                Ok(Some(RawFrame {
                    format: PixelFormat::Nv12,
                    width: w as u32,
                    height: h as u32,
                    stride: w as u32,
                    data,
                    timestamp_us: d.timestamp_us,
                }))
            }
        };
        // Requeue regardless of outcome so a geometry error doesn't leak the
        // capture buffer out of the driver's rotation.
        cap.queue
            .queue(&mut self.ops, idx, 0, 0)
            .context("requeue capture buffer")?;
        let frame = frame?;
        if frame.is_some() {
            self.unanswered = 0;
        }
        Ok(frame)
    }

    /// Wait until an OUTPUT slot is free (at most MAX_IN_FLIGHT queued),
    /// servicing events and pictures meanwhile. A picture found here is held
    /// in `pending` for the caller. Progress is measured in poll rounds, not
    /// wall clock, so the fake (whose poll returns at once) sees the same
    /// number of rounds as the hardware: STALL_POLL_MS / POLL_STEP_MS.
    fn wait_for_slot(&mut self, pending: &mut Option<RawFrame>) -> anyhow::Result<()> {
        let max_idle = STALL_POLL_MS / POLL_STEP_MS as u64;
        let mut idle = 0u64;
        while self.output.in_flight() >= MAX_IN_FLIGHT {
            let r = self.ops.poll(POLL_STEP_MS).context("poll")?;
            let mut progressed = false;
            if r.event {
                self.drain_events()?;
                progressed = true;
            }
            if self.drain_output()? {
                progressed = true;
            }
            if pending.is_none() {
                if let Some(f) = self.take_capture()? {
                    *pending = Some(f);
                    progressed = true;
                }
            }
            if progressed {
                idle = 0;
            } else {
                idle += 1;
                if idle >= max_idle {
                    bail!("decoder stalled: no progress for {STALL_POLL_MS} ms");
                }
            }
        }
        Ok(())
    }

    #[cfg(test)]
    pub(crate) fn take_calls_on_drop(&mut self) -> std::sync::Arc<std::sync::Mutex<Vec<String>>>
    where
        O: crate::fake::HasSink,
    {
        let sink = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        self.ops.set_sink(sink.clone());
        sink
    }
}

impl<O: Ops + Send> VideoDecoder for V4l2Decoder<O> {
    fn decode(&mut self, data: &[u8], timestamp_us: u64) -> anyhow::Result<Option<RawFrame>> {
        annexb::check_access_unit(data)?;
        if data.len() > OUTPUT_BUFFER_SIZE as usize {
            bail!(
                "access unit of {} bytes exceeds the {} byte decoder buffer",
                data.len(),
                OUTPUT_BUFFER_SIZE
            );
        }
        if self.finished {
            bail!("decoder reported end of stream");
        }
        let mut pending: Option<RawFrame> = None;
        self.wait_for_slot(&mut pending)?;
        let slot = self
            .output
            .free_slot()
            .ok_or_else(|| anyhow!("no free output buffer"))?;
        let mapping_len = self.output.buffers[slot].mapping.len();
        if data.len() > mapping_len {
            bail!(
                "access unit of {} bytes exceeds the {} byte output buffer actually allocated",
                data.len(),
                mapping_len
            );
        }
        self.output.buffers[slot].mapping.as_mut_slice()[..data.len()].copy_from_slice(data);
        self.output
            .queue(&mut self.ops, slot, data.len() as u32, timestamp_us)
            .context("QBUF output")?;
        self.unanswered += 1;
        self.drain_events()?;
        if pending.is_none() {
            pending = self.take_capture()?;
        }
        if pending.is_none() && self.unanswered >= STALL_INPUTS {
            bail!("decoder stalled: {STALL_INPUTS} access units queued with no picture");
        }
        Ok(pending)
    }

    fn name(&self) -> &'static str {
        "v4l2-bcm2835"
    }
}

impl<O: Ops> Drop for V4l2Decoder<O> {
    fn drop(&mut self) {
        if let Some(mut c) = self.capture.take() {
            if let Err(e) = c.queue.release(&mut self.ops) {
                tracing::warn!("v4l2 decoder: release capture: {e}");
            }
        }
        if let Err(e) = self.output.release(&mut self.ops) {
            tracing::warn!("v4l2 decoder: release output: {e}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fake::FakeOps;
    use crate::ops::{Dequeued, Event, FormatInfo, PollResult};
    use castr_media::VideoDecoder;

    const OUT: u32 = V4L2_BUF_TYPE_VIDEO_OUTPUT_MPLANE;
    const CAP: u32 = V4L2_BUF_TYPE_VIDEO_CAPTURE_MPLANE;
    const KEY: [u8; 9] = [0, 0, 0, 1, 0x67, 0, 0, 1, 0x65];
    const DELTA: [u8; 5] = [0, 0, 0, 1, 0x41];

    fn src_change() -> Event {
        Event {
            kind: V4L2_EVENT_SOURCE_CHANGE,
            changes: V4L2_EVENT_SRC_CH_RESOLUTION,
        }
    }

    #[test]
    fn startup_configures_output_side_only() {
        let ops = FakeOps::new();
        let d = V4l2Decoder::with_ops(ops).unwrap();
        let calls = &d.ops.calls;
        assert!(calls.contains(&"query_cap".to_string()));
        assert!(calls.iter().any(|c| c.starts_with("s_fmt(10,")));
        assert!(calls.contains(&format!("subscribe({V4L2_EVENT_SOURCE_CHANGE})")));
        assert!(calls.contains(&format!("subscribe({V4L2_EVENT_EOS})")));
        assert!(calls.contains(&"reqbufs(10,4)".to_string()));
        assert!(calls.contains(&"streamon(10)".to_string()));
        assert!(
            !calls.iter().any(|c| c.starts_with("reqbufs(9,")),
            "capture must wait for SOURCE_CHANGE"
        );
        assert!(d.frame_size().is_none());
    }

    #[test]
    fn open_fails_without_h264_or_nv12_support() {
        let mut ops = FakeOps::new();
        ops.output_formats = vec![V4L2_PIX_FMT_NV12];
        assert!(V4l2Decoder::with_ops(ops).is_err());
        let mut ops = FakeOps::new();
        ops.capture_formats = vec![];
        assert!(V4l2Decoder::with_ops(ops).is_err());
        let mut ops = FakeOps::new();
        ops.caps = V4L2_CAP_STREAMING;
        assert!(V4l2Decoder::with_ops(ops).is_err());
    }

    #[test]
    fn rejects_non_annex_b_and_oversized_input() {
        let mut d = V4l2Decoder::with_ops(FakeOps::new()).unwrap();
        assert!(d.decode(&[1, 2, 3], 0).is_err());
        let huge = vec![0u8; OUTPUT_BUFFER_SIZE as usize + 1];
        let mut au = KEY.to_vec();
        au.extend_from_slice(&huge);
        assert!(d.decode(&au, 0).is_err());
    }

    #[test]
    fn first_source_change_brings_up_capture_and_frames_flow() {
        let mut ops = FakeOps::new();
        ops.push_event(src_change());
        let mut d = V4l2Decoder::with_ops(ops).unwrap();
        // Feeding the keyframe queues it, drains the event, configures capture.
        assert!(d.decode(&KEY, 33_000).unwrap().is_none());
        let c = &d.ops.calls;
        assert!(c.contains(&"qbuf(10,0,9,33000)".to_string()));
        assert!(c.iter().any(|x| x.starts_with("g_fmt(9)")));
        assert!(c
            .iter()
            .any(|x| x == &format!("s_fmt(9,{:#x},640x368)", V4L2_PIX_FMT_NV12)));
        assert!(c.contains(&"reqbufs(9,6)".to_string()));
        assert!(c.contains(&"streamon(9)".to_string()));
        assert_eq!(
            c.iter().filter(|x| x.starts_with("qbuf(9,")).count(),
            6,
            "all capture buffers queued"
        );
        assert_eq!(d.frame_size(), Some((640, 360)));
        // A decoded picture appears: NV12, visible size, requeued.
        d.ops.push_dequeue(
            CAP,
            Dequeued {
                index: 2,
                bytesused: 640 * 368 * 3 / 2,
                timestamp_us: 33_000,
                flags: 0,
            },
        );
        let f = d.decode(&DELTA, 66_000).unwrap().expect("frame");
        assert_eq!((f.width, f.height, f.stride), (640, 360, 640));
        assert_eq!(f.format, castr_media::PixelFormat::Nv12);
        assert_eq!(f.data.len(), 640 * 360 * 3 / 2);
        assert_eq!(f.timestamp_us, 33_000);
        assert!(
            d.ops.calls.iter().any(|x| x.starts_with("qbuf(9,2,")),
            "capture buffer 2 requeued"
        );
    }

    #[test]
    fn capture_copy_crops_to_the_visible_rectangle() {
        let mut ops = FakeOps::new();
        ops.push_event(src_change());
        ops.captures_filled = vec![0x80]; // non-empty triggers FakeOps's positional fill
                                          // A compose rect with non-zero left/top inside the 640x368 coded frame,
                                          // so a wrong uv_base, top/2, or dropped left offset makes this fail.
        ops.compose = v4l2_rect {
            left: 8,
            top: 4,
            width: 624,
            height: 352,
        };
        let mut d = V4l2Decoder::with_ops(ops).unwrap();
        d.decode(&KEY, 0).unwrap();
        d.ops.push_dequeue(
            CAP,
            Dequeued {
                index: 0,
                bytesused: 640 * 368 * 3 / 2,
                timestamp_us: 0,
                flags: 0,
            },
        );
        let f = d.decode(&DELTA, 33_000).unwrap().unwrap();
        let (stride, coded_h) = (640usize, 368usize);
        let (left, top, w, h) = (8usize, 4usize, 624usize, 352usize);
        let byte_at = |o: usize| (o % 251) as u8;
        let mut expected = Vec::with_capacity(w * h * 3 / 2);
        for row in 0..h {
            let o = (top + row) * stride + left;
            expected.extend((0..w).map(|c| byte_at(o + c)));
        }
        let uv_base = stride * coded_h;
        for row in 0..h / 2 {
            let o = uv_base + (top / 2 + row) * stride + left;
            expected.extend((0..w).map(|c| byte_at(o + c)));
        }
        assert_eq!(f.data, expected);
        assert_eq!(f.data.len(), 624 * 352 + 624 * 176);
    }

    #[test]
    fn resolution_change_reallocates_capture() {
        let mut ops = FakeOps::new();
        ops.push_event(src_change());
        let mut d = V4l2Decoder::with_ops(ops).unwrap();
        d.decode(&KEY, 0).unwrap();
        let before = d.ops.calls.len();
        d.ops.capture_format = FormatInfo {
            width: 1280,
            height: 720,
            pixelformat: V4L2_PIX_FMT_NV12,
            bytesperline: 1280,
            sizeimage: 1280 * 720 * 3 / 2,
        };
        d.ops.compose = v4l2_rect {
            left: 0,
            top: 0,
            width: 1280,
            height: 720,
        };
        d.ops.push_event(src_change());
        assert!(d.decode(&KEY, 33_000).unwrap().is_none());
        let after: Vec<_> = d.ops.calls[before..].to_vec();
        let pos = |s: &str| {
            after
                .iter()
                .position(|c| c.starts_with(s))
                .unwrap_or_else(|| panic!("missing {s}: {after:?}"))
        };
        assert!(pos("streamoff(9)") < pos("reqbufs(9,0)"));
        assert!(pos("reqbufs(9,0)") < pos("s_fmt(9,"));
        assert!(pos("s_fmt(9,") < pos("reqbufs(9,6)"));
        assert!(pos("reqbufs(9,6)") < pos("streamon(9)"));
        assert_eq!(d.frame_size(), Some((1280, 720)));
    }

    #[test]
    fn at_most_two_inputs_in_flight_and_polling_dequeues_them() {
        let mut ops = FakeOps::new();
        ops.push_event(src_change());
        // The third input must wait: poll reports writable, and a dequeue is scripted.
        ops.polls.push_back(PollResult {
            writable: true,
            ..Default::default()
        });
        ops.push_dequeue(
            OUT,
            Dequeued {
                index: 0,
                bytesused: 0,
                timestamp_us: 0,
                flags: 0,
            },
        );
        let mut d = V4l2Decoder::with_ops(ops).unwrap();
        d.decode(&KEY, 0).unwrap();
        d.decode(&DELTA, 33_000).unwrap();
        assert_eq!(d.output.in_flight(), 2);
        d.decode(&DELTA, 66_000).unwrap();
        assert_eq!(d.output.in_flight(), 2);
        assert!(d.ops.calls.iter().any(|c| c.starts_with("poll(")));
        assert!(
            d.ops.calls.contains(&"qbuf(10,0,5,66000)".to_string()),
            "slot 0 reused after dequeue"
        );
    }

    #[test]
    fn stalls_after_sixty_unanswered_inputs() {
        let mut ops = FakeOps::new();
        ops.push_event(src_change());
        // Every wait for a slot immediately "completes" an output buffer.
        for _ in 0..200 {
            ops.polls.push_back(PollResult {
                writable: true,
                ..Default::default()
            });
        }
        let mut d = V4l2Decoder::with_ops(ops).unwrap();
        d.decode(&KEY, 0).unwrap();
        let mut err = None;
        for i in 1..=STALL_INPUTS + 2 {
            // Return the buffer the next decode will need, alternating slots 0 and 1,
            // so a free slot is always available and the stall comes only from the
            // 60-unanswered-inputs rule.
            let idx = i % 2;
            d.ops.push_dequeue(
                OUT,
                Dequeued {
                    index: idx,
                    bytesused: 0,
                    timestamp_us: 0,
                    flags: 0,
                },
            );
            match d.decode(&DELTA, i as u64 * 33_000) {
                Ok(None) => {}
                Ok(Some(_)) => panic!("no frames were scripted"),
                Err(e) => {
                    err = Some(e);
                    break;
                }
            }
        }
        let e = err.expect("stall error");
        assert!(
            e.to_string()
                .contains("access units queued with no picture"),
            "{e:#}"
        );
    }

    #[test]
    fn polling_gives_up_after_two_seconds_without_progress() {
        let mut ops = FakeOps::new();
        ops.push_event(src_change());
        let mut d = V4l2Decoder::with_ops(ops).unwrap();
        d.decode(&KEY, 0).unwrap();
        d.decode(&DELTA, 1).unwrap();
        // Third input: every poll times out (default PollResult) and nothing dequeues.
        let e = d.decode(&DELTA, 2).unwrap_err();
        assert!(e.to_string().contains("stalled"), "{e:#}");
        let polls = d
            .ops
            .calls
            .iter()
            .filter(|c| c.starts_with("poll("))
            .count();
        assert_eq!(polls as u64, STALL_POLL_MS / 200);
    }

    #[test]
    fn ioctl_failure_is_reported() {
        let mut ops = FakeOps::new();
        ops.push_event(src_change());
        let mut d = V4l2Decoder::with_ops(ops).unwrap();
        d.ops.fail_next = Some("EIO");
        assert!(d.decode(&KEY, 0).is_err());
    }

    #[test]
    fn drop_stops_both_queues() {
        let mut ops = FakeOps::new();
        ops.push_event(src_change());
        let mut d = V4l2Decoder::with_ops(ops).unwrap();
        // FakeOps is moved into the decoder; it copies its call log into this
        // sink when it is dropped, after the decoder's own Drop has run.
        let log = d.take_calls_on_drop();
        d.decode(&KEY, 0).unwrap();
        drop(d);
        let log = log.lock().unwrap();
        assert!(log.contains(&"streamoff(10)".to_string()));
        assert!(log.contains(&"streamoff(9)".to_string()));
    }

    #[test]
    fn bad_capture_geometry_is_reported_and_the_buffer_is_still_requeued() {
        let mut ops = FakeOps::new();
        ops.push_event(src_change());
        // sizeimage smaller than bytesperline * height * 3 / 2: the mapping is
        // too small for the coded geometry the driver itself reported.
        ops.capture_format = FormatInfo {
            width: 640,
            height: 368,
            pixelformat: V4L2_PIX_FMT_NV12,
            bytesperline: 640,
            sizeimage: 640 * 368, // should be 640*368*3/2
        };
        let mut d = V4l2Decoder::with_ops(ops).unwrap();
        d.decode(&KEY, 0).unwrap();
        d.ops.push_dequeue(
            CAP,
            Dequeued {
                index: 0,
                bytesused: 640 * 368 * 3 / 2,
                timestamp_us: 0,
                flags: 0,
            },
        );
        let e = d.decode(&DELTA, 33_000).unwrap_err();
        assert!(e.to_string().contains("geometry"), "{e:#}");
        assert!(
            d.ops.calls.iter().any(|c| c.starts_with("qbuf(9,0,")),
            "capture buffer still requeued despite the geometry error"
        );
    }

    #[test]
    fn startup_fails_if_the_driver_grants_too_few_output_buffers() {
        let mut ops = FakeOps::new();
        ops.granted = Some(1);
        let e = V4l2Decoder::with_ops(ops).err().unwrap();
        assert!(e.to_string().contains("buffer"), "{e:#}");
    }
}
