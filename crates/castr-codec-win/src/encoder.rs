use crate::mf::*;
use anyhow::{anyhow, bail, Context};
use castr_media::*;
use std::collections::VecDeque;
use std::time::{Duration, Instant};
use windows::core::{Interface, VARIANT};
use windows::Win32::Media::MediaFoundation::*;

pub struct MfEncoder {
    cfg: EncoderConfig,
    mft: IMFTransform,
    events: Option<IMFMediaEventGenerator>,
    codec_api: Option<ICodecAPI>,
    provides_samples: bool,
    output_size: u32,
    need_input_credits: u32,
    frame_index: u64,
    name: &'static str,
    /// Outputs obtained anywhere other than the end of `encode` (e.g. while waiting
    /// for a NeedInput credit, or extras drained after the first `take_output` in a
    /// single `encode` call). Held in FIFO order and drained before any freshly
    /// produced output, so no access unit is ever dropped.
    pending: VecDeque<EncodedFrame>,
}

// SAFETY: `mf_startup` initializes COM with `COINIT_MULTITHREADED`, so the process is
// in the multi-threaded apartment and the COM/MF interfaces held here are free-threaded
// (no apartment marshaling is required to use them from a different thread than the one
// that created them). `castr_media::VideoEncoder` requires `Send`; `MfEncoder` is not
// `Sync` and is never used concurrently from multiple threads at once.
unsafe impl Send for MfEncoder {}

fn set_codec_u32(api: &ICodecAPI, key: &windows::core::GUID, v: u32) -> anyhow::Result<()> {
    // SAFETY: `api` is a valid `ICodecAPI` COM interface; `SetValue` reads the VARIANT
    // by reference and does not retain the pointer.
    unsafe { api.SetValue(key, &VARIANT::from(v)) }
        .with_context(|| format!("ICodecAPI::SetValue {key:?}"))
}

fn set_codec_bool(api: &ICodecAPI, key: &windows::core::GUID, v: bool) -> anyhow::Result<()> {
    // SAFETY: see `set_codec_u32`.
    unsafe { api.SetValue(key, &VARIANT::from(v)) }
        .with_context(|| format!("ICodecAPI::SetValue {key:?}"))
}

impl MfEncoder {
    pub fn new(cfg: EncoderConfig) -> anyhow::Result<Self> {
        mf_startup()?;
        let mut candidates: Vec<(IMFActivate, &'static str)> = find_transforms(
            MFT_CATEGORY_VIDEO_ENCODER,
            &MFVideoFormat_NV12,
            &MFVideoFormat_H264,
            true,
        )?
        .into_iter()
        .map(|a| (a, "mf-hardware"))
        .collect();
        candidates.extend(
            find_transforms(
                MFT_CATEGORY_VIDEO_ENCODER,
                &MFVideoFormat_NV12,
                &MFVideoFormat_H264,
                false,
            )?
            .into_iter()
            .map(|a| (a, "mf-software")),
        );
        let mut last_err = anyhow!("no H.264 encoder MFT found");
        for (activate, name) in candidates {
            let friendly = transform_name(&activate);
            match Self::open(&activate, &cfg, name) {
                Ok(enc) => {
                    tracing::info!("using encoder {friendly} ({name})");
                    return Ok(enc);
                }
                Err(e) => {
                    tracing::warn!("encoder {friendly} rejected: {e:#}");
                    last_err = e;
                }
            }
        }
        Err(last_err)
    }

    fn open(
        activate: &IMFActivate,
        cfg: &EncoderConfig,
        name: &'static str,
    ) -> anyhow::Result<Self> {
        // SAFETY: `activate` is a valid `IMFActivate` from `find_transforms`;
        // `ActivateObject` returns a new `IMFTransform` reference we own.
        let mft: IMFTransform = unsafe { activate.ActivateObject() }.context("ActivateObject")?;
        // SAFETY: `mft` is a valid transform; `GetAttributes` returns its (possibly
        // absent) attribute store.
        let attrs = unsafe { mft.GetAttributes() }.ok();
        let is_async = attrs
            .as_ref()
            // SAFETY: `a` is a valid attribute store obtained above.
            .map(|a| unsafe { a.GetUINT32(&MF_TRANSFORM_ASYNC) }.unwrap_or(0) == 1)
            .unwrap_or(false);
        if is_async {
            // SAFETY: `attrs` is `Some` in this branch (checked by `is_async`'s map).
            unsafe {
                attrs
                    .as_ref()
                    .unwrap()
                    .SetUINT32(&MF_TRANSFORM_ASYNC_UNLOCK, 1)
            }
            .context("async unlock")?;
        }
        let codec_api: Option<ICodecAPI> = mft.cast().ok();
        if let Some(api) = &codec_api {
            let _ = set_codec_bool(api, &CODECAPI_AVLowLatencyMode, true);
            let _ = set_codec_u32(api, &CODECAPI_AVEncMPVDefaultBPictureCount, 0);
            Self::apply_mode(api, cfg);
        }
        let out_type = video_type(
            &MFVideoFormat_H264,
            cfg.width,
            cfg.height,
            cfg.fps,
            Some(cfg.bitrate_bps),
        )?;
        // SAFETY: `mft` is a valid transform; `out_type` is a valid media type.
        unsafe { mft.SetOutputType(0, &out_type, 0) }.context("SetOutputType H264")?;
        let in_type = video_type(&MFVideoFormat_NV12, cfg.width, cfg.height, cfg.fps, None)?;
        // SAFETY: same as above, for the input type.
        unsafe { mft.SetInputType(0, &in_type, 0) }.context("SetInputType NV12")?;
        // SAFETY: `mft` is a valid, configured transform.
        let info = unsafe { mft.GetOutputStreamInfo(0) }.context("GetOutputStreamInfo")?;
        let provides_samples = info.dwFlags
            & (MFT_OUTPUT_STREAM_PROVIDES_SAMPLES.0 as u32
                | MFT_OUTPUT_STREAM_CAN_PROVIDE_SAMPLES.0 as u32)
            != 0;
        // SAFETY: `mft` is valid; these messages carry no pointer payload (param is 0).
        unsafe {
            mft.ProcessMessage(MFT_MESSAGE_NOTIFY_BEGIN_STREAMING, 0)?;
            mft.ProcessMessage(MFT_MESSAGE_NOTIFY_START_OF_STREAM, 0)?;
        }
        let events = if is_async {
            Some(
                mft.cast::<IMFMediaEventGenerator>()
                    .context("IMFMediaEventGenerator")?,
            )
        } else {
            None
        };
        Ok(Self {
            cfg: *cfg,
            mft,
            events,
            codec_api,
            provides_samples,
            output_size: info.cbSize.max(1 << 20),
            need_input_credits: 0,
            frame_index: 0,
            name,
            pending: VecDeque::new(),
        })
    }

    fn apply_mode(api: &ICodecAPI, cfg: &EncoderConfig) {
        let (rc, gop) = match cfg.mode {
            Mode::Game => (eAVEncCommonRateControlMode_CBR.0 as u32, cfg.fps * 10),
            Mode::Quality => (
                eAVEncCommonRateControlMode_UnconstrainedVBR.0 as u32,
                cfg.fps * 2,
            ),
        };
        let _ = set_codec_u32(api, &CODECAPI_AVEncCommonRateControlMode, rc);
        let _ = set_codec_u32(api, &CODECAPI_AVEncMPVGOPSize, gop);
        let _ = set_codec_u32(api, &CODECAPI_AVEncCommonMeanBitRate, cfg.bitrate_bps);
    }

    /// Async MFTs: pump events, counting NeedInput credits, returning true when HaveOutput is seen.
    fn pump_events(&mut self, wait: Duration) -> anyhow::Result<bool> {
        let Some(gen) = &self.events else {
            return Ok(true);
        };
        let deadline = Instant::now() + wait;
        loop {
            // SAFETY: `gen` is a valid `IMFMediaEventGenerator` obtained from the transform.
            match unsafe { gen.GetEvent(MF_EVENT_FLAG_NO_WAIT) } {
                Ok(ev) => {
                    // SAFETY: `ev` is a valid event object just returned by `GetEvent`.
                    let t = unsafe { ev.GetType() }?;
                    if t == METransformNeedInput.0 as u32 {
                        self.need_input_credits += 1;
                    } else if t == METransformHaveOutput.0 as u32 {
                        return Ok(true);
                    }
                }
                Err(_) => {
                    if Instant::now() >= deadline {
                        return Ok(false);
                    }
                    std::thread::sleep(Duration::from_millis(1));
                }
            }
        }
    }

    fn wait_need_input(&mut self) -> anyhow::Result<()> {
        if self.events.is_none() {
            return Ok(());
        }
        let deadline = Instant::now() + Duration::from_millis(200);
        while self.need_input_credits == 0 {
            if self.pump_events(Duration::from_millis(5))? {
                // Output arrived before we could feed input; queue it (FIFO) rather than
                // discard it, so this access unit is still returned to the caller.
                if let Some(f) = self.take_output()? {
                    self.pending.push_back(f);
                }
            }
            if Instant::now() >= deadline {
                bail!("encoder never requested input");
            }
        }
        self.need_input_credits -= 1;
        Ok(())
    }

    fn take_output(&mut self) -> anyhow::Result<Option<EncodedFrame>> {
        let mut out = MFT_OUTPUT_DATA_BUFFER {
            dwStreamID: 0,
            pSample: std::mem::ManuallyDrop::new(None),
            dwStatus: 0,
            pEvents: std::mem::ManuallyDrop::new(None),
        };
        if !self.provides_samples {
            let sample = make_sample(&vec![0u8; self.output_size as usize], 0, 0)?;
            // SAFETY: `sample` was just created with one buffer at index 0.
            unsafe { sample.GetBufferByIndex(0)?.SetCurrentLength(0)? };
            out.pSample = std::mem::ManuallyDrop::new(Some(sample));
        }
        let mut status = 0u32;
        // SAFETY: `self.mft` is a valid, configured transform; `out` is a single
        // well-formed `MFT_OUTPUT_DATA_BUFFER` for stream 0, matching the API contract.
        let hr = unsafe {
            self.mft
                .ProcessOutput(0, std::slice::from_mut(&mut out), &mut status)
        };
        // SAFETY: `out.pSample`/`out.pEvents` were initialized above (or left `None`)
        // and are taken exactly once here to hand ownership back to safe code.
        let sample = unsafe { std::mem::ManuallyDrop::take(&mut out.pSample) };
        if let Some(ev) = unsafe { std::mem::ManuallyDrop::take(&mut out.pEvents) } {
            drop(ev);
        }
        match hr {
            Ok(()) => {
                let sample = sample.ok_or_else(|| anyhow!("ProcessOutput returned no sample"))?;
                let data = read_sample(&sample)?;
                if data.is_empty() {
                    return Ok(None);
                }
                // SAFETY: `sample` is a valid output sample.
                let keyframe =
                    unsafe { sample.GetUINT32(&MFSampleExtension_CleanPoint) }.unwrap_or(0) == 1;
                // SAFETY: `sample` is a valid output sample.
                let time_us = unsafe { sample.GetSampleTime() }.unwrap_or(0).max(0) as u64 / 10;
                Ok(Some(EncodedFrame {
                    data,
                    keyframe,
                    timestamp_us: time_us,
                }))
            }
            Err(e) if e.code() == MF_E_TRANSFORM_NEED_MORE_INPUT => Ok(None),
            Err(e) if e.code() == MF_E_TRANSFORM_STREAM_CHANGE => {
                // SAFETY: `self.mft` is a valid transform.
                let t = unsafe { self.mft.GetOutputAvailableType(0, 0) }?;
                // SAFETY: `t` is a valid media type just retrieved from the transform.
                unsafe { self.mft.SetOutputType(0, &t, 0) }?;
                Ok(None)
            }
            Err(e) => Err(e).context("ProcessOutput"),
        }
    }
}

impl VideoEncoder for MfEncoder {
    fn encode(&mut self, frame: &RawFrame) -> anyhow::Result<Option<EncodedFrame>> {
        anyhow::ensure!(frame.format == PixelFormat::Nv12, "MfEncoder expects NV12");
        anyhow::ensure!(
            frame.width == self.cfg.width && frame.height == self.cfg.height,
            "frame size mismatch"
        );
        let duration_us = 1_000_000 / self.cfg.fps as u64;
        let sample = make_sample(&frame.data, frame.timestamp_us, duration_us)?;
        self.wait_need_input()?;
        // SAFETY: `self.mft` is a valid, configured transform; `sample` is a valid
        // input sample carrying the encoder's expected NV12 payload for stream 0.
        unsafe { self.mft.ProcessInput(0, &sample, 0) }.context("ProcessInput")?;
        self.frame_index += 1;
        if self.events.is_some() {
            let wait = if self.frame_index <= 3 {
                Duration::from_millis(200)
            } else {
                Duration::from_millis(100)
            };
            if !self.pump_events(wait)? {
                // No output arrived from this input, but an earlier one may still be queued.
                return Ok(self.pending.pop_front());
            }
        }
        // A single input can unblock more than one queued output (e.g. after
        // MF_E_NOTACCEPTING would otherwise have hit us on the next ProcessInput).
        // Drain everything the transform is willing to hand back right now, pushing
        // every one obtained in this call onto the back of `pending` in the order
        // they were produced (oldest first) so delivery order is never rotated; the
        // caller always gets the oldest queued output overall, not just the oldest
        // from this call. For an async MFT, the event contract only permits one
        // `ProcessOutput` per `METransformHaveOutput`, so only keep going while
        // another such event is already queued (checked without waiting); a
        // synchronous MFT has no event queue and `take_output` itself terminates the
        // loop by returning `None` once it reports `MF_E_TRANSFORM_NEED_MORE_INPUT`.
        if let Some(fresh) = self.take_output()? {
            self.pending.push_back(fresh);
        }
        loop {
            if self.events.is_some() && !self.pump_events(Duration::ZERO)? {
                break;
            }
            match self.take_output()? {
                Some(extra) => self.pending.push_back(extra),
                None => break,
            }
        }
        Ok(self.pending.pop_front())
    }

    fn request_keyframe(&mut self) {
        if let Some(api) = &self.codec_api {
            let _ = set_codec_u32(api, &CODECAPI_AVEncVideoForceKeyFrame, 1);
        }
    }

    fn set_bitrate(&mut self, bitrate_bps: u32) -> anyhow::Result<()> {
        self.cfg.bitrate_bps = bitrate_bps;
        let api = self
            .codec_api
            .as_ref()
            .ok_or_else(|| anyhow!("encoder exposes no ICodecAPI"))?;
        set_codec_u32(api, &CODECAPI_AVEncCommonMeanBitRate, bitrate_bps)
    }

    fn set_mode(&mut self, mode: Mode) -> anyhow::Result<()> {
        self.cfg.mode = mode;
        if let Some(api) = &self.codec_api {
            Self::apply_mode(api, &self.cfg);
        }
        self.request_keyframe();
        Ok(())
    }

    fn input_format(&self) -> PixelFormat {
        PixelFormat::Nv12
    }
    fn name(&self) -> &'static str {
        self.name
    }
}
