use crate::mf::*;
use anyhow::{anyhow, Context};
use castr_media::*;
use windows::core::Interface;
use windows::Win32::Media::MediaFoundation::*;

pub struct MfDecoder {
    mft: IMFTransform,
    /// Coded (padded) surface size, used to compute buffer strides.
    coded_width: u32,
    coded_height: u32,
    /// Visible size reported to callers, taken from the display aperture when the
    /// decoder exposes one (the coded size is macroblock-aligned and can be larger).
    width: u32,
    height: u32,
    output_size: u32,
    provides_samples: bool,
}

// SAFETY: see the identical justification on `MfEncoder`: COM is initialized in the
// multi-threaded apartment by `mf_startup`, so these interfaces are free-threaded.
unsafe impl Send for MfDecoder {}

impl MfDecoder {
    pub fn new() -> anyhow::Result<Self> {
        mf_startup()?;
        let activates = find_transforms(
            MFT_CATEGORY_VIDEO_DECODER,
            &MFVideoFormat_H264,
            &MFVideoFormat_NV12,
            false,
        )?;
        let activate = activates
            .first()
            .ok_or_else(|| anyhow!("no H.264 decoder MFT"))?;
        tracing::info!("using decoder {}", transform_name(activate));
        // SAFETY: `activate` is a valid `IMFActivate` from `find_transforms`;
        // `ActivateObject` returns a new `IMFTransform` reference we own.
        let mft: IMFTransform = unsafe { activate.ActivateObject() }.context("ActivateObject")?;
        // SAFETY: `mft` is a valid transform; `GetAttributes` returns its attribute store.
        if let Ok(attrs) = unsafe { mft.GetAttributes() } {
            // SAFETY: `attrs` is a valid attribute store just obtained above.
            let _ = unsafe { attrs.SetUINT32(&CODECAPI_AVLowLatencyMode, 1) };
        }
        let in_type = video_type(&MFVideoFormat_H264, 1920, 1080, 30, None)?;
        // SAFETY: `mft` is a valid transform; `in_type` is a valid media type.
        unsafe { mft.SetInputType(0, &in_type, 0) }.context("SetInputType H264")?;
        let mut dec = Self {
            mft,
            coded_width: 0,
            coded_height: 0,
            width: 0,
            height: 0,
            output_size: 0,
            provides_samples: false,
        };
        dec.negotiate_output()?;
        // SAFETY: `dec.mft` is valid; these messages carry no pointer payload (param is 0).
        unsafe {
            dec.mft
                .ProcessMessage(MFT_MESSAGE_NOTIFY_BEGIN_STREAMING, 0)?;
            dec.mft
                .ProcessMessage(MFT_MESSAGE_NOTIFY_START_OF_STREAM, 0)?;
        }
        Ok(dec)
    }

    fn negotiate_output(&mut self) -> anyhow::Result<()> {
        let mut i = 0;
        loop {
            // SAFETY: `self.mft` is a valid transform; `i` enumerates its offered
            // output types for stream 0.
            let t = unsafe { self.mft.GetOutputAvailableType(0, i) }
                .context("no NV12 output type offered")?;
            // SAFETY: `t` is a valid media type just retrieved above.
            let sub = unsafe { t.GetGUID(&MF_MT_SUBTYPE) }?;
            if sub == MFVideoFormat_NV12 {
                // SAFETY: `t` is a valid media type offered by the transform itself.
                unsafe { self.mft.SetOutputType(0, &t, 0) }.context("SetOutputType NV12")?;
                // SAFETY: `t` is a valid, now-active media type.
                let size = unsafe { t.GetUINT64(&MF_MT_FRAME_SIZE) }?;
                self.coded_width = (size >> 32) as u32;
                self.coded_height = (size & 0xFFFF_FFFF) as u32;
                let (disp_w, disp_h) =
                    Self::display_aperture(&t).unwrap_or((self.coded_width, self.coded_height));
                self.width = disp_w;
                self.height = disp_h;
                // SAFETY: `self.mft` is a valid, configured transform.
                let info = unsafe { self.mft.GetOutputStreamInfo(0) }?;
                self.output_size = info
                    .cbSize
                    .max(self.coded_width * self.coded_height * 3 / 2);
                self.provides_samples =
                    info.dwFlags & (MFT_OUTPUT_STREAM_PROVIDES_SAMPLES.0 as u32) != 0;
                return Ok(());
            }
            i += 1;
        }
    }

    /// Reads the display aperture (visible region) off a media type, if the decoder
    /// exposes one. The coded (macroblock-aligned) size from `MF_MT_FRAME_SIZE` can be
    /// larger than the true picture size, so callers should prefer this when present.
    fn display_aperture(t: &IMFMediaType) -> Option<(u32, u32)> {
        for attr in [MF_MT_MINIMUM_DISPLAY_APERTURE, MF_MT_GEOMETRIC_APERTURE] {
            let mut area = MFVideoArea::default();
            // SAFETY: `t` is a valid media type; `area` is a correctly sized local
            // buffer for the fixed-size `MFVideoArea` blob these attributes store.
            let buf = unsafe {
                std::slice::from_raw_parts_mut(
                    &mut area as *mut MFVideoArea as *mut u8,
                    std::mem::size_of::<MFVideoArea>(),
                )
            };
            // SAFETY: `t` is a valid media type; `buf` matches the blob size we expect.
            if unsafe { t.GetBlob(&attr, buf, None) }.is_ok()
                && area.Area.cx > 0
                && area.Area.cy > 0
            {
                return Some((area.Area.cx as u32, area.Area.cy as u32));
            }
        }
        None
    }

    fn read_nv12(&self, sample: &IMFSample) -> anyhow::Result<Vec<u8>> {
        // SAFETY: `sample` is a valid output sample with at least one buffer.
        let buffer = unsafe { sample.GetBufferByIndex(0) }?;
        let (cw, ch) = (self.coded_width as usize, self.coded_height as usize);
        let (w, h) = (self.width as usize, self.height as usize);
        let mut out = vec![0u8; w * h * 3 / 2];
        if let Ok(b2d) = buffer.cast::<IMF2DBuffer>() {
            let mut scanline0 = std::ptr::null_mut();
            let mut pitch = 0i32;
            // SAFETY: `b2d` is a valid 2D buffer; `Lock2D` hands back a pointer to
            // the top scanline and the row pitch, valid until `Unlock2D`.
            unsafe { b2d.Lock2D(&mut scanline0, &mut pitch) }.context("Lock2D")?;
            let pitch = pitch.unsigned_abs() as usize;
            // Luma: `h` rows of `w` visible columns, addressed within the coded
            // (padded) surface using its pitch.
            for row in 0..h {
                // SAFETY: `scanline0` is valid for `pitch` bytes per row for `ch`
                // luma rows plus `ch / 2` chroma rows, per `Lock2D`'s contract; `row < h <= ch`.
                let src = unsafe { std::slice::from_raw_parts(scanline0.add(row * pitch), w) };
                out[row * w..row * w + w].copy_from_slice(src);
            }
            // Chroma (NV12 interleaved UV) starts at the coded luma height, not the
            // visible one.
            for row in 0..h / 2 {
                // SAFETY: same reasoning as the luma loop, offset past the coded
                // luma plane at row `ch + row`.
                let src =
                    unsafe { std::slice::from_raw_parts(scanline0.add((ch + row) * pitch), w) };
                let dst_off = w * h + row * w;
                out[dst_off..dst_off + w].copy_from_slice(src);
            }
            // SAFETY: matches the `Lock2D` call above.
            unsafe { b2d.Unlock2D() }?;
        } else {
            let data = read_sample(sample)?;
            if cw == w && ch == h {
                let n = out.len().min(data.len());
                out[..n].copy_from_slice(&data[..n]);
            } else {
                let coded_len = cw * ch * 3 / 2;
                if data.len() >= coded_len {
                    for row in 0..h {
                        out[row * w..row * w + w].copy_from_slice(&data[row * cw..row * cw + w]);
                    }
                    for row in 0..h / 2 {
                        let src_off = cw * ch + row * cw;
                        let dst_off = w * h + row * w;
                        out[dst_off..dst_off + w].copy_from_slice(&data[src_off..src_off + w]);
                    }
                }
            }
        }
        Ok(out)
    }
}

impl VideoDecoder for MfDecoder {
    fn decode(&mut self, data: &[u8], timestamp_us: u64) -> anyhow::Result<Option<RawFrame>> {
        let sample = make_sample(data, timestamp_us, 33_333)?;
        // SAFETY: `self.mft` is a valid, configured transform; `sample` is a valid
        // input sample carrying an Annex B access unit for stream 0.
        unsafe { self.mft.ProcessInput(0, &sample, 0) }.context("ProcessInput")?;
        loop {
            let mut out = MFT_OUTPUT_DATA_BUFFER {
                dwStreamID: 0,
                pSample: std::mem::ManuallyDrop::new(None),
                dwStatus: 0,
                pEvents: std::mem::ManuallyDrop::new(None),
            };
            if !self.provides_samples {
                let s = make_sample(&vec![0u8; self.output_size as usize], 0, 0)?;
                // SAFETY: `s` was just created with one buffer at index 0.
                unsafe { s.GetBufferByIndex(0)?.SetCurrentLength(0)? };
                out.pSample = std::mem::ManuallyDrop::new(Some(s));
            }
            let mut status = 0u32;
            // SAFETY: `self.mft` is a valid, configured transform; `out` is a single
            // well-formed `MFT_OUTPUT_DATA_BUFFER` for stream 0.
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
                    let sample = sample.ok_or_else(|| anyhow!("no output sample"))?;
                    // SAFETY: `sample` is a valid output sample.
                    let ts = unsafe { sample.GetSampleTime() }
                        .map(|t| t.max(0) as u64 / 10)
                        .unwrap_or(timestamp_us);
                    let data = self.read_nv12(&sample)?;
                    return Ok(Some(RawFrame {
                        format: PixelFormat::Nv12,
                        width: self.width,
                        height: self.height,
                        stride: self.width,
                        data,
                        timestamp_us: ts,
                    }));
                }
                Err(e) if e.code() == MF_E_TRANSFORM_NEED_MORE_INPUT => return Ok(None),
                Err(e) if e.code() == MF_E_TRANSFORM_STREAM_CHANGE => {
                    self.negotiate_output()?;
                    continue;
                }
                Err(e) => return Err(e).context("ProcessOutput"),
            }
        }
    }

    fn name(&self) -> &'static str {
        "mf-h264"
    }
}
