//! `V4l2Decoder`: H.264 in, NV12 out, over the bcm2835 V4L2 M2M decoder.
//! See spec section 4 for the sequence this implements.

use crate::annexb;
use crate::ops::{Dequeue, FormatInfo, Ops, RealOps};
use crate::queue::Queue;
use crate::sys::*;
use anyhow::{anyhow, bail, Context};
use castr_media::{PixelFormat, RawFrame, VideoDecoder};
use std::collections::VecDeque;

pub const DEFAULT_DEVICE: &str = "/dev/video10";
pub const OUTPUT_BUFFERS: u32 = 4;
pub const OUTPUT_BUFFER_SIZE: u32 = 1 << 20;
/// CAPTURE buffers requested, raised to `MIN_BUFFERS_FOR_CAPTURE + 2` when the
/// driver asks for more.
pub const CAPTURE_BUFFERS: u32 = 6;
/// Compressed frames queued but not yet consumed; bounds decoder latency.
pub const MAX_IN_FLIGHT: usize = 2;
/// Inputs queued with no picture back before we declare the decoder dead.
/// It takes STALL_POLL_MS of wall clock as well: 60 access units are 2 s of
/// video only when the caller feeds in real time, and a receiver catching up
/// after a jitter-buffer burst can legitimately push a whole second of video
/// into the hardware in a few milliseconds.
pub const STALL_INPUTS: u32 = 60;
/// Wall-clock budget for waiting on the hardware inside one `decode` call.
pub const STALL_POLL_MS: u64 = 2_000;
const POLL_STEP_MS: i32 = 200;
/// Poll rounds spent waiting for the end-of-sequence buffer when a resolution
/// change arrives, before the CAPTURE queue is rebuilt anyway.
const DRAIN_ROUNDS: u32 = 5;

struct Capture {
    queue: Queue,
    /// Coded (allocated) size and stride reported by the driver.
    coded: FormatInfo,
    /// Visible picture inside the coded frame.
    visible: v4l2_rect,
    /// True until the first SOURCE_CHANGE. The queue streams with the
    /// driver's default (tiny) format so the M2M pipeline runs while the
    /// decoder parses the first SPS; no picture ever comes out of it.
    provisional: bool,
}

pub struct V4l2Decoder<O: Ops = RealOps> {
    pub(crate) ops: O,
    pub(crate) output: Queue,
    capture: Option<Capture>,
    /// Pictures dequeued but not yet handed to the caller (a `decode` call
    /// returns at most one, and draining a sequence can produce several).
    ready: VecDeque<RawFrame>,
    unanswered: u32,
    /// When the decoder last produced a picture (or was opened).
    last_picture: std::time::Instant,
    finished: bool,
    /// Set when a CAPTURE buffer carried `V4L2_BUF_FLAG_LAST`, i.e. the
    /// driver has finished the sequence it was decoding.
    saw_last: bool,
}

impl<O: Ops> std::fmt::Debug for V4l2Decoder<O> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("V4l2Decoder")
            .field("unanswered", &self.unanswered)
            .field("finished", &self.finished)
            .field("frame_size", &self.frame_size())
            .finish()
    }
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
        // The CAPTURE side is brought up provisionally (driver-default format,
        // real buffers) before the first access unit, the way ffmpeg's
        // v4l2_m2m and GStreamer's v4l2videodec do: an M2M job only runs when
        // both queues are streaming with buffers queued. It does not make the
        // first SOURCE_CHANGE arrive sooner (measured on a Pi 3: about 70 ms
        // after the first access unit either way), but it means the pipeline
        // is running during that warm-up, so the pictures decoded from the
        // access units fed meanwhile survive the reconfiguration, and the
        // first source change then takes exactly the same code path as every
        // later one.
        let capture = match Self::bring_up_capture(&mut ops, true) {
            Ok(c) => c,
            Err(e) => {
                let _ = output.release(&mut ops);
                return Err(e);
            }
        };
        let last_picture = ops.now();
        Ok(Self {
            ops,
            output,
            capture: Some(capture),
            ready: VecDeque::new(),
            unanswered: 0,
            last_picture,
            finished: false,
            saw_last: false,
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
            .filter(|c| !c.provisional)
            .map(|c| (c.visible.width, c.visible.height))
    }

    /// Format the CAPTURE queue to NV12 at whatever size the driver reports,
    /// allocate and queue buffers, and start it streaming.
    fn bring_up_capture(ops: &mut O, provisional: bool) -> anyhow::Result<Capture> {
        let cap_type = V4L2_BUF_TYPE_VIDEO_CAPTURE_MPLANE;
        let reported = ops.g_fmt(cap_type).context("G_FMT capture")?;
        let coded = ops
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
        // The driver may need more buffers than our default to keep decoding.
        let min = ops
            .g_ctrl(V4L2_CID_MIN_BUFFERS_FOR_CAPTURE)
            .unwrap_or(0)
            .max(0) as u32;
        let count = CAPTURE_BUFFERS.max(min + 2);
        let mut queue = Queue::new(cap_type);
        // If allocation, queueing, or STREAMON fails partway through, release
        // whatever was set up so a half-built capture queue isn't leaked.
        if let Err(e) = Self::allocate_and_start(ops, &mut queue, count) {
            let _ = queue.release(ops);
            return Err(e);
        }
        let visible = Self::visible_rect(ops, &coded);
        if !provisional {
            tracing::info!(
                "v4l2 decoder: {}x{} visible in {}x{} coded, stride {}, {} buffers",
                visible.width,
                visible.height,
                coded.width,
                coded.height,
                coded.bytesperline,
                queue.buffers.len()
            );
        }
        Ok(Capture {
            queue,
            coded,
            visible,
            provisional,
        })
    }

    fn allocate_and_start(ops: &mut O, queue: &mut Queue, count: u32) -> anyhow::Result<()> {
        queue
            .allocate(ops, count, true)
            .context("allocate capture buffers")?;
        Self::queue_all_and_start(ops, queue)
    }

    fn queue_all_and_start(ops: &mut O, queue: &mut Queue) -> anyhow::Result<()> {
        for i in 0..queue.buffers.len() {
            queue.queue(ops, i, 0, 0).context("queue capture buffer")?;
        }
        queue.stream_on(ops).context("STREAMON capture")
    }

    fn visible_rect(ops: &mut O, coded: &FormatInfo) -> v4l2_rect {
        let full = v4l2_rect {
            left: 0,
            top: 0,
            width: coded.width,
            height: coded.height,
        };
        match ops.g_selection_compose(V4L2_BUF_TYPE_VIDEO_CAPTURE_MPLANE) {
            Ok(r) if r.width > 0 && r.height > 0 => r,
            _ => full,
        }
    }

    /// Apply a SOURCE_CHANGE (spec 4.2 step 5, 4.4).
    ///
    /// The kernel's stateful decoder contract is that the old sequence is
    /// finished first: pictures already decoded keep coming until one is
    /// marked `V4L2_BUF_FLAG_LAST`. Stopping the queue before that throws
    /// those pictures away - including the new sequence's IDR, after which
    /// every following delta frame decodes to nothing.
    fn reconfigure_capture(&mut self) -> anyhow::Result<()> {
        let provisional = self.capture.as_ref().is_some_and(|c| c.provisional);
        if !provisional {
            self.drain_to_last()?;
        }
        let Some(mut old) = self.capture.take() else {
            bail!("capture queue missing");
        };
        if old.queue.is_streaming() {
            old.queue
                .stream_off(&mut self.ops)
                .context("STREAMOFF capture")?;
        }
        let cap_type = V4L2_BUF_TYPE_VIDEO_CAPTURE_MPLANE;
        let reported = self.ops.g_fmt(cap_type).context("G_FMT capture")?;
        let unchanged = !old.provisional
            && reported.width == old.coded.width
            && reported.height == old.coded.height
            && reported.bytesperline == old.coded.bytesperline
            && reported.sizeimage == old.coded.sizeimage;
        if unchanged {
            // Same coded geometry: the buffers are still the right size, so
            // only restart the queue and re-read the compose rectangle.
            old.visible = Self::visible_rect(&mut self.ops, &old.coded);
            Self::queue_all_and_start(&mut self.ops, &mut old.queue)?;
            tracing::debug!(
                "v4l2 decoder: source change with unchanged {}x{} geometry",
                old.coded.width,
                old.coded.height
            );
            self.saw_last = false;
            self.capture = Some(old);
            return Ok(());
        }
        old.queue
            .release(&mut self.ops)
            .context("release old capture buffers")?;
        drop(old);
        self.capture = Some(Self::bring_up_capture(&mut self.ops, false)?);
        self.saw_last = false;
        Ok(())
    }

    /// Collect the tail of the current sequence, up to the buffer the driver
    /// flags `LAST`. Bounded: a driver that never sends one must not wedge us.
    fn drain_to_last(&mut self) -> anyhow::Result<()> {
        self.saw_last = false;
        for _ in 0..DRAIN_ROUNDS {
            self.collect_capture(usize::MAX)?;
            if self.saw_last {
                return Ok(());
            }
            self.drain_output()?;
            let r = self.ops.poll(POLL_STEP_MS).context("poll")?;
            if r.error {
                return Err(poll_error());
            }
        }
        self.collect_capture(usize::MAX)?;
        if !self.saw_last {
            tracing::debug!(
                "v4l2 decoder: no end-of-sequence buffer within {} ms; reconfiguring anyway",
                DRAIN_ROUNDS as i32 * POLL_STEP_MS
            );
        }
        Ok(())
    }

    /// Returns whether any event was handled.
    fn drain_events(&mut self) -> anyhow::Result<bool> {
        let mut any = false;
        while let Some(ev) = self.ops.dqevent().context("DQEVENT")? {
            any = true;
            match ev.kind {
                V4L2_EVENT_SOURCE_CHANGE => {
                    if ev.changes & V4L2_EVENT_SRC_CH_RESOLUTION != 0 {
                        self.reconfigure_capture()?;
                    }
                }
                V4L2_EVENT_EOS => self.finished = true,
                _ => {}
            }
        }
        Ok(any)
    }

    fn drain_output(&mut self) -> anyhow::Result<bool> {
        let mut any = false;
        loop {
            match self.output.dequeue(&mut self.ops).context("DQBUF output")? {
                Dequeue::Buffer(_) => any = true,
                // Nothing ready, or the driver has finished the sequence it
                // was fed; either way there is nothing more to collect here.
                Dequeue::Idle | Dequeue::EndOfSequence => return Ok(any),
            }
        }
    }

    /// Dequeue up to `limit` CAPTURE buffers, queueing their pictures for the
    /// caller. Taking one at a time in the steady state leaves the backlog in
    /// the driver's own buffers, which is what back-pressures a decoder fed
    /// faster than it can decode. Returns whether anything was dequeued.
    fn collect_capture(&mut self, limit: usize) -> anyhow::Result<bool> {
        // The sequence has ended (LAST buffer, EPIPE, or a refused requeue):
        // the driver produces nothing more until the queue is restarted, so
        // stop asking. `drain_to_last` and a reconfigure clear this.
        if self.saw_last {
            return Ok(false);
        }
        let mut any = false;
        for _ in 0..limit {
            let Some(picture) = self.take_capture()? else {
                break;
            };
            any = true;
            if let Some(f) = picture {
                self.ready.push_back(f);
            }
            if self.saw_last {
                break;
            }
        }
        Ok(any)
    }

    /// Copy one decoded picture out of a capture buffer and requeue it.
    /// `None` means nothing was ready; `Some(None)` means the buffer carried
    /// no picture (the driver's end-of-sequence marker).
    fn take_capture(&mut self) -> anyhow::Result<Option<Option<RawFrame>>> {
        let Some(cap) = self.capture.as_mut() else {
            return Ok(None);
        };
        let d = match cap.queue.dequeue(&mut self.ops).context("DQBUF capture")? {
            Dequeue::Buffer(d) => d,
            Dequeue::Idle => return Ok(None),
            // The driver already handed over the sequence's last picture and
            // now refuses further dequeues until the queue is restarted.
            Dequeue::EndOfSequence => {
                self.saw_last = true;
                return Ok(None);
            }
        };
        let idx = d.index as usize;
        let last = d.flags & V4L2_BUF_FLAG_LAST != 0;
        // Anything decoded before the first SOURCE_CHANGE came out of the
        // provisional queue, at the driver's default geometry rather than the
        // stream's: recycle it, never hand it to the caller.
        let frame = if cap.provisional {
            tracing::debug!("v4l2 decoder: dropping a picture from the provisional capture queue");
            Ok(None)
        } else if d.bytesused == 0 {
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
        // capture buffer out of the driver's rotation - except for the
        // end-of-sequence buffer, which the driver will not take back until
        // the queue has been restarted.
        if last {
            self.saw_last = true;
        } else if let Err(e) = cap.queue.queue(&mut self.ops, idx, 0, 0) {
            // EINVAL here means the driver has already re-formatted the
            // CAPTURE queue for a source change it has not yet told us about,
            // so our buffer is now the wrong size for it. That is the end of
            // the old sequence; the pending SOURCE_CHANGE rebuilds the queue.
            //
            // This is deliberately not restricted to a drain: on the hardware
            // it fires *outside* one, while collecting normally, because the
            // driver re-formats the queue before it reports the event. Nothing
            // in the ioctl tells the two cases apart, and if no source change
            // follows, the driver simply produces nothing more and the stall
            // detector fires - so a genuinely malformed QBUF shows up as a
            // stall rather than being silently swallowed.
            if e.raw_os_error() == Some(libc::EINVAL) {
                tracing::warn!("v4l2 decoder: capture buffer {idx} refused with EINVAL; treating it as the end of the sequence");
                self.saw_last = true;
            } else {
                return Err(anyhow::Error::new(e).context("requeue capture buffer"));
            }
        }
        let frame = frame?;
        if frame.is_some() {
            self.unanswered = 0;
            self.last_picture = self.ops.now();
        }
        Ok(Some(frame))
    }

    /// Wait until an OUTPUT slot is free (at most MAX_IN_FLIGHT queued),
    /// servicing events and pictures meanwhile. Progress is measured in poll
    /// rounds, not wall clock, so the fake (whose poll returns at once) sees
    /// the same number of rounds as the hardware: STALL_POLL_MS / POLL_STEP_MS.
    fn wait_for_slot(&mut self, budget: &mut usize) -> anyhow::Result<()> {
        let max_idle = STALL_POLL_MS / POLL_STEP_MS as u64;
        let mut idle = 0u64;
        // The deadlock-breaking recycle below copies a picture, so like
        // `budget` it is spent at most once per decode call.
        let mut fallback = 1usize;
        while self.output.in_flight() >= MAX_IN_FLIGHT {
            let r = self.ops.poll(POLL_STEP_MS).context("poll")?;
            if r.error {
                return Err(poll_error());
            }
            let mut progressed = self.drain_events()?;
            if self.drain_output()? {
                progressed = true;
            }
            if *budget > 0 && self.collect_capture(1)? {
                *budget -= 1;
                progressed = true;
            }
            if progressed {
                idle = 0;
                continue;
            }
            // Nothing moved. The driver may be waiting for a CAPTURE buffer we
            // are holding back to bound this call's latency, so recycle one
            // anyway rather than deadlocking against our own budget.
            if fallback > 0 && self.collect_capture(1)? {
                fallback -= 1;
                idle = 0;
                continue;
            }
            idle += 1;
            if idle >= max_idle {
                bail!("decoder stalled: no progress for {STALL_POLL_MS} ms");
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

fn poll_error() -> anyhow::Error {
    anyhow!("decoder failed: poll reported POLLERR on the device (the driver dropped the stream)")
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
        // Copying a picture out of the driver's (uncached) buffer is the
        // expensive part of a decode call - about 20 ms for 1080p on a Pi 3 -
        // so at most one is copied per call. That still recycles one CAPTURE
        // buffer per access unit, which is exactly the steady-state rate.
        let mut budget = 1usize;
        self.wait_for_slot(&mut budget)?;
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
        if budget > 0 {
            self.collect_capture(budget)?;
        }
        let idle = self.ops.now().saturating_duration_since(self.last_picture);
        if self.ready.is_empty()
            && self.unanswered >= STALL_INPUTS
            && idle >= std::time::Duration::from_millis(STALL_POLL_MS)
        {
            bail!(
                "decoder stalled: {} access units queued with no picture for {} ms",
                self.unanswered,
                idle.as_millis()
            );
        }
        Ok(self.ready.pop_front())
    }

    /// Collect a picture the decoder has already produced, without feeding it
    /// anything. Waits up to one poll step for one to appear.
    ///
    /// The driver is pipelined - a picture comes out a few access units after
    /// the one that produced it - and it batches heavily when it is fed faster
    /// than real time, so a caller that has run out of input needs a way to
    /// pull the tail out rather than growing its latency.
    fn poll_frame(&mut self) -> anyhow::Result<Option<RawFrame>> {
        if let Some(f) = self.ready.pop_front() {
            return Ok(Some(f));
        }
        let r = self.ops.poll(POLL_STEP_MS).context("poll")?;
        if r.error {
            return Err(poll_error());
        }
        self.drain_events()?;
        self.drain_output()?;
        self.collect_capture(1)?;
        Ok(self.ready.pop_front())
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

    fn hd() -> FormatInfo {
        FormatInfo {
            width: 1280,
            height: 720,
            pixelformat: V4L2_PIX_FMT_NV12,
            bytesperline: 1280,
            sizeimage: 1280 * 720 * 3 / 2,
        }
    }

    fn rect(width: u32, height: u32) -> v4l2_rect {
        v4l2_rect {
            left: 0,
            top: 0,
            width,
            height,
        }
    }

    #[test]
    fn startup_streams_both_queues_before_the_first_access_unit() {
        let ops = FakeOps::new();
        let d = V4l2Decoder::with_ops(ops).unwrap();
        let calls = &d.ops.calls;
        assert!(calls.contains(&"query_cap".to_string()));
        assert!(calls.iter().any(|c| c.starts_with("s_fmt(10,")));
        assert!(calls.contains(&format!("subscribe({V4L2_EVENT_SOURCE_CHANGE})")));
        assert!(calls.contains(&format!("subscribe({V4L2_EVENT_EOS})")));
        assert!(calls.contains(&"reqbufs(10,4)".to_string()));
        assert!(calls.contains(&"streamon(10)".to_string()));
        // The M2M pipeline only runs a job when both queues are streaming with
        // buffers queued, so CAPTURE is brought up provisionally at the
        // driver's default size; otherwise the first SOURCE_CHANGE arrives
        // only once the driver's internal input pool has filled.
        assert!(calls.iter().any(|c| c.starts_with("s_fmt(9,")));
        assert!(calls.contains(&"reqbufs(9,6)".to_string()));
        assert_eq!(
            calls.iter().filter(|c| c.starts_with("qbuf(9,")).count(),
            6,
            "all provisional capture buffers queued"
        );
        assert!(calls.contains(&"streamon(9)".to_string()));
        // No SPS has been parsed, so no size is claimed yet.
        assert!(d.frame_size().is_none());
    }

    #[test]
    fn capture_count_honours_the_drivers_minimum() {
        let mut ops = FakeOps::new();
        ops.min_capture_buffers = 8;
        let d = V4l2Decoder::with_ops(ops).unwrap();
        assert!(
            d.ops.calls.contains(&"reqbufs(9,10)".to_string()),
            "MIN_BUFFERS_FOR_CAPTURE + 2: {:?}",
            d.ops.calls
        );
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
    fn first_source_change_rebuilds_capture_and_frames_flow() {
        let mut ops = FakeOps::new();
        ops.push_event(src_change());
        let mut d = V4l2Decoder::with_ops(ops).unwrap();
        let before = d.ops.calls.len();
        // Feeding the keyframe queues it, drains the event, rebuilds capture.
        assert!(d.decode(&KEY, 33_000).unwrap().is_none());
        let c: Vec<_> = d.ops.calls[before..].to_vec();
        assert!(c.contains(&"qbuf(10,0,9,33000)".to_string()));
        // The provisional queue is stopped and freed, then rebuilt for the
        // size the driver now reports.
        let pos = |s: &str| {
            c.iter()
                .position(|x| x.starts_with(s))
                .unwrap_or_else(|| panic!("missing {s}: {c:?}"))
        };
        assert!(pos("streamoff(9)") < pos("reqbufs(9,0)"));
        assert!(pos("reqbufs(9,0)") < pos("s_fmt(9,"));
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
        d.ops.capture_format = hd();
        d.ops.compose = rect(1280, 720);
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
    fn a_resolution_change_drains_to_the_last_buffer_before_stopping_capture() {
        let mut ops = FakeOps::new();
        ops.push_event(src_change());
        let mut d = V4l2Decoder::with_ops(ops).unwrap();
        d.decode(&KEY, 0).unwrap();
        let before = d.ops.calls.len();
        // The driver finishes the old sequence before the new size takes
        // effect: one more 640x360 picture, then an empty buffer flagged LAST.
        d.ops.push_dequeue(
            CAP,
            Dequeued {
                index: 1,
                bytesused: 640 * 368 * 3 / 2,
                timestamp_us: 33_000,
                flags: 0,
            },
        );
        d.ops.push_dequeue(
            CAP,
            Dequeued {
                index: 2,
                bytesused: 0,
                timestamp_us: 0,
                flags: V4L2_BUF_FLAG_LAST,
            },
        );
        d.ops.capture_format = hd();
        d.ops.compose = rect(1280, 720);
        d.ops.push_event(src_change());
        let f = d
            .decode(&KEY, 66_000)
            .unwrap()
            .expect("the old sequence's tail picture is not thrown away");
        assert_eq!((f.width, f.height), (640, 360));
        let after: Vec<_> = d.ops.calls[before..].to_vec();
        let off = after
            .iter()
            .position(|c| c == "streamoff(9)")
            .expect("capture stopped");
        let drained = &after[..off];
        assert!(
            drained.iter().filter(|c| c.as_str() == "dqbuf(9)").count() >= 2,
            "both buffers dequeued before STREAMOFF: {after:?}"
        );
        assert!(
            drained.iter().any(|c| c.starts_with("qbuf(9,1,")),
            "the picture buffer is recycled: {after:?}"
        );
        assert!(
            !drained.iter().any(|c| c.starts_with("qbuf(9,2,")),
            "the LAST buffer is not requeued into a stopping queue: {after:?}"
        );
        assert_eq!(d.frame_size(), Some((1280, 720)));
    }

    #[test]
    fn a_source_change_that_keeps_the_geometry_only_restarts_the_queue() {
        let mut ops = FakeOps::new();
        ops.push_event(src_change());
        let mut d = V4l2Decoder::with_ops(ops).unwrap();
        d.decode(&KEY, 0).unwrap();
        let before = d.ops.calls.len();
        // Same coded size reported again (e.g. a bitstream restart at the same
        // resolution): the buffers stay, the queue is only cycled.
        d.ops.push_dequeue(
            CAP,
            Dequeued {
                index: 0,
                bytesused: 0,
                timestamp_us: 0,
                flags: V4L2_BUF_FLAG_LAST,
            },
        );
        d.ops.push_event(src_change());
        d.decode(&KEY, 33_000).unwrap();
        let after: Vec<_> = d.ops.calls[before..].to_vec();
        assert!(after.contains(&"streamoff(9)".to_string()));
        assert!(after.contains(&"streamon(9)".to_string()));
        assert!(
            !after.iter().any(|c| c.starts_with("reqbufs(9,")),
            "no reallocation for an unchanged format: {after:?}"
        );
        assert_eq!(
            after.iter().filter(|c| c.starts_with("qbuf(9,")).count(),
            6,
            "every buffer is requeued when the queue restarts: {after:?}"
        );
        assert_eq!(d.frame_size(), Some((640, 360)));
    }

    #[test]
    fn poll_frame_hands_back_the_backlog_a_drain_produced() {
        let mut ops = FakeOps::new();
        ops.push_event(src_change());
        let mut d = V4l2Decoder::with_ops(ops).unwrap();
        d.decode(&KEY, 0).unwrap();
        // A source change drains the old sequence, so several pictures can
        // arrive at once. `decode` returns one per call by contract; the rest
        // must be reachable or they are permanent latency.
        for i in 0..4u32 {
            d.ops.push_dequeue(
                CAP,
                Dequeued {
                    index: i,
                    bytesused: 640 * 368 * 3 / 2,
                    timestamp_us: i as u64 * 33_000,
                    flags: 0,
                },
            );
        }
        d.ops.push_dequeue(
            CAP,
            Dequeued {
                index: 4,
                bytesused: 0,
                timestamp_us: 0,
                flags: V4L2_BUF_FLAG_LAST,
            },
        );
        d.ops.capture_format = hd();
        d.ops.compose = rect(1280, 720);
        d.ops.push_event(src_change());
        let first = d.decode(&KEY, 33_000).unwrap().expect("one picture");
        assert_eq!(first.timestamp_us, 0);
        let rest: Vec<u64> = std::iter::from_fn(|| d.poll_frame().unwrap())
            .map(|f| f.timestamp_us)
            .collect();
        assert_eq!(rest, vec![33_000, 66_000, 99_000], "in order, then empty");
        assert!(d.poll_frame().unwrap().is_none());
    }

    #[test]
    fn pictures_from_the_provisional_queue_are_dropped_not_returned() {
        let mut ops = FakeOps::new();
        // No SOURCE_CHANGE yet: the capture queue is still at the driver's
        // default geometry, so anything it produces is not this stream's.
        ops.push_dequeue(
            CAP,
            Dequeued {
                index: 3,
                bytesused: 640 * 368 * 3 / 2,
                timestamp_us: 7,
                flags: 0,
            },
        );
        let mut d = V4l2Decoder::with_ops(ops).unwrap();
        let before = d.ops.calls.len();
        assert!(
            d.decode(&KEY, 0).unwrap().is_none(),
            "no picture is claimed"
        );
        assert!(d.frame_size().is_none());
        assert!(
            d.ops.calls[before..]
                .iter()
                .any(|c| c.starts_with("qbuf(9,3,")),
            "the buffer is still recycled: {:?}",
            &d.ops.calls[before..]
        );
    }

    #[test]
    fn a_capture_requeue_refused_with_einval_ends_the_sequence() {
        let mut ops = FakeOps::new();
        ops.push_event(src_change());
        let mut d = V4l2Decoder::with_ops(ops).unwrap();
        d.decode(&KEY, 0).unwrap();
        // The driver hands back a picture but then refuses to take the buffer
        // again: it has already re-formatted the queue for a source change it
        // has not reported yet.
        d.ops.push_dequeue(
            CAP,
            Dequeued {
                index: 1,
                bytesused: 640 * 368 * 3 / 2,
                timestamp_us: 33_000,
                flags: 0,
            },
        );
        d.ops.fail_errno = Some(("qbuf(9,", libc::EINVAL));
        d.ops.capture_format = hd();
        d.ops.compose = rect(1280, 720);
        d.ops.push_event(src_change());
        // No error surfaces, the picture is kept, and the drain finishes at
        // once instead of polling out its whole budget.
        let before = d.ops.calls.len();
        let f = d.decode(&KEY, 66_000).unwrap().expect("picture kept");
        assert_eq!((f.width, f.height), (640, 360));
        assert_eq!(d.frame_size(), Some((1280, 720)), "capture was rebuilt");
        assert_eq!(
            d.ops.calls[before..]
                .iter()
                .filter(|c| c.starts_with("poll("))
                .count(),
            0,
            "EINVAL ends the drain immediately"
        );
    }

    #[test]
    fn an_einval_requeue_outside_a_drain_also_ends_the_sequence() {
        let mut ops = FakeOps::new();
        ops.push_event(src_change());
        let mut d = V4l2Decoder::with_ops(ops).unwrap();
        d.decode(&KEY, 0).unwrap();
        // The same refusal with no source change pending. On the hardware this
        // is where it actually happens (the driver re-formats before it
        // reports the event), so it is end-of-sequence here too, not an error:
        // with no event to follow, the driver goes quiet and the stall rule
        // catches it.
        d.ops.push_dequeue(
            CAP,
            Dequeued {
                index: 1,
                bytesused: 640 * 368 * 3 / 2,
                timestamp_us: 33_000,
                flags: 0,
            },
        );
        d.ops.push_dequeue(
            CAP,
            Dequeued {
                index: 2,
                bytesused: 640 * 368 * 3 / 2,
                timestamp_us: 66_000,
                flags: 0,
            },
        );
        d.ops.fail_errno = Some(("qbuf(9,", libc::EINVAL));
        let f = d.decode(&DELTA, 66_000).unwrap().expect("picture kept");
        assert_eq!(f.timestamp_us, 33_000);
        // The second scripted buffer is not collected: the sequence is over
        // until the queue is restarted.
        assert!(d.poll_frame().unwrap().is_none());
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
    fn stalls_only_after_sixty_unanswered_inputs_and_two_idle_seconds() {
        let mut ops = FakeOps::new();
        ops.push_event(src_change());
        // Every wait for a slot immediately "completes" an output buffer.
        for _ in 0..400 {
            ops.polls.push_back(PollResult {
                writable: true,
                ..Default::default()
            });
        }
        let mut d = V4l2Decoder::with_ops(ops).unwrap();
        d.decode(&KEY, 0).unwrap();
        let feed = |d: &mut V4l2Decoder<FakeOps>, i: u32| {
            // Return the buffer the next decode will need, alternating slots 0
            // and 1, so a free slot is always available and the stall can only
            // come from the unanswered-input rule.
            d.ops.push_dequeue(
                OUT,
                Dequeued {
                    index: i % 2,
                    bytesused: 0,
                    timestamp_us: 0,
                    flags: 0,
                },
            );
            d.decode(&DELTA, i as u64 * 33_000)
        };
        // A caller catching up after a jitter-buffer burst can push far more
        // than 60 access units in well under two seconds; that is not a stall.
        for i in 1..=STALL_INPUTS * 2 {
            assert!(
                feed(&mut d, i).is_ok(),
                "input {i} within the time budget must not be treated as a stall"
            );
        }
        d.ops
            .advance(std::time::Duration::from_millis(STALL_POLL_MS));
        let e = feed(&mut d, STALL_INPUTS * 2 + 1).unwrap_err();
        assert!(
            e.to_string()
                .contains("access units queued with no picture"),
            "{e:#}"
        );
    }

    #[test]
    fn a_picture_resets_the_stall_detector() {
        let mut ops = FakeOps::new();
        ops.push_event(src_change());
        for _ in 0..400 {
            ops.polls.push_back(PollResult {
                writable: true,
                ..Default::default()
            });
        }
        let mut d = V4l2Decoder::with_ops(ops).unwrap();
        d.decode(&KEY, 0).unwrap();
        for i in 1..=STALL_INPUTS {
            d.ops.push_dequeue(
                OUT,
                Dequeued {
                    index: i % 2,
                    bytesused: 0,
                    timestamp_us: 0,
                    flags: 0,
                },
            );
            d.decode(&DELTA, i as u64 * 33_000).unwrap();
        }
        d.ops
            .advance(std::time::Duration::from_millis(STALL_POLL_MS));
        // One picture, then the same idle time again: still no stall.
        d.ops.push_dequeue(
            CAP,
            Dequeued {
                index: 0,
                bytesused: 640 * 368 * 3 / 2,
                timestamp_us: 0,
                flags: 0,
            },
        );
        d.ops.push_dequeue(
            OUT,
            Dequeued {
                index: 0,
                bytesused: 0,
                timestamp_us: 0,
                flags: 0,
            },
        );
        assert!(d.decode(&DELTA, 1).unwrap().is_some());
        d.ops
            .advance(std::time::Duration::from_millis(STALL_POLL_MS - 1));
        d.ops.push_dequeue(
            OUT,
            Dequeued {
                index: 1,
                bytesused: 0,
                timestamp_us: 0,
                flags: 0,
            },
        );
        assert!(d.decode(&DELTA, 2).is_ok());
    }

    #[test]
    fn polling_gives_up_after_two_seconds_without_progress() {
        let mut ops = FakeOps::new();
        ops.push_event(src_change());
        let mut d = V4l2Decoder::with_ops(ops).unwrap();
        d.decode(&KEY, 0).unwrap();
        d.decode(&DELTA, 1).unwrap();
        let before = d.ops.calls.len();
        // Third input: every poll times out (default PollResult) and nothing dequeues.
        let e = d.decode(&DELTA, 2).unwrap_err();
        assert!(e.to_string().contains("stalled"), "{e:#}");
        let polls = d.ops.calls[before..]
            .iter()
            .filter(|c| c.starts_with("poll("))
            .count();
        assert_eq!(polls as u64, STALL_POLL_MS / 200);
    }

    #[test]
    fn a_poll_error_fails_the_decoder_immediately() {
        let mut ops = FakeOps::new();
        ops.push_event(src_change());
        // POLLERR on an M2M device never clears by itself, so one round of it
        // must be fatal rather than counting as "no progress" for 2 seconds.
        ops.polls.push_back(PollResult {
            error: true,
            ..Default::default()
        });
        let mut d = V4l2Decoder::with_ops(ops).unwrap();
        d.decode(&KEY, 0).unwrap();
        d.decode(&DELTA, 1).unwrap();
        let before = d.ops.calls.len();
        let e = d.decode(&DELTA, 2).unwrap_err();
        assert!(e.to_string().contains("POLLERR"), "{e:#}");
        assert_eq!(
            d.ops.calls[before..]
                .iter()
                .filter(|c| c.starts_with("poll("))
                .count(),
            1,
            "no further polling after POLLERR"
        );
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
