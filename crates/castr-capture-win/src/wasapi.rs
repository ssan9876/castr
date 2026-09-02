use anyhow::Context;
use windows::Win32::Media::Audio::*;
use windows::Win32::System::Com::*;

const REFTIMES_PER_MS: i64 = 10_000;

pub struct LoopbackCapture {
    _client: IAudioClient,
    capture: IAudioCaptureClient,
}

impl LoopbackCapture {
    /// Opens the default render device in shared loopback mode, requesting 48 kHz stereo
    /// 16-bit with Windows' automatic format conversion. Call on the thread that will call
    /// `drain`, since COM is initialized per-thread here.
    pub fn new() -> anyhow::Result<Self> {
        // SAFETY: CoInitializeEx is safe to call per-thread; we check the returned HRESULT
        // and only treat RPC_E_CHANGED_MODE (already initialized differently) as non-fatal.
        unsafe {
            let hr = CoInitializeEx(None, COINIT_MULTITHREADED);
            if hr.is_err() && hr != windows::Win32::Foundation::RPC_E_CHANGED_MODE {
                return Err(anyhow::anyhow!("CoInitializeEx: {hr:?}"));
            }
        }
        // SAFETY: CoCreateInstance with a valid CLSID/IID pair; COM was initialized above.
        let enumerator: IMMDeviceEnumerator =
            unsafe { CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL) }
                .context("MMDeviceEnumerator")?;
        // SAFETY: enumerator is a valid COM interface obtained above.
        let device = unsafe { enumerator.GetDefaultAudioEndpoint(eRender, eConsole) }
            .context("default render endpoint")?;
        // SAFETY: device is a valid COM interface obtained above.
        let client: IAudioClient =
            unsafe { device.Activate(CLSCTX_ALL, None) }.context("IAudioClient")?;
        let format = WAVEFORMATEX {
            wFormatTag: WAVE_FORMAT_PCM as u16,
            nChannels: 2,
            nSamplesPerSec: 48_000,
            nAvgBytesPerSec: 48_000 * 4,
            nBlockAlign: 4,
            wBitsPerSample: 16,
            cbSize: 0,
        };
        let flags = AUDCLNT_STREAMFLAGS_LOOPBACK
            | AUDCLNT_STREAMFLAGS_AUTOCONVERTPCM
            | AUDCLNT_STREAMFLAGS_SRC_DEFAULT_QUALITY;
        // SAFETY: client is a valid COM interface; format is a valid WAVEFORMATEX describing
        // 48 kHz stereo 16-bit PCM.
        unsafe {
            client
                .Initialize(
                    AUDCLNT_SHAREMODE_SHARED,
                    flags,
                    50 * REFTIMES_PER_MS,
                    0,
                    &format,
                    None,
                )
                .context(
                    "IAudioClient::Initialize (48 kHz s16 stereo loopback with auto-convert)",
                )?;
        }
        // SAFETY: client is a valid, initialized COM interface.
        let capture: IAudioCaptureClient =
            unsafe { client.GetService() }.context("IAudioCaptureClient")?;
        // SAFETY: client is a valid, initialized COM interface.
        unsafe { client.Start() }.context("IAudioClient::Start")?;
        Ok(Self {
            _client: client,
            capture,
        })
    }

    /// Drains whatever is available now into `out` as interleaved i16 stereo at 48 kHz.
    /// Non-blocking. Silence packets append zeros.
    pub fn drain(&mut self, out: &mut Vec<i16>) -> anyhow::Result<()> {
        loop {
            // SAFETY: capture is a valid, started COM interface.
            let packet =
                unsafe { self.capture.GetNextPacketSize() }.context("GetNextPacketSize")?;
            if packet == 0 {
                return Ok(());
            }
            let mut data: *mut u8 = std::ptr::null_mut();
            let mut frames = 0u32;
            let mut flags = 0u32;
            // SAFETY: capture is a valid COM interface; data/frames/flags are valid out-pointers
            // per the IAudioCaptureClient::GetBuffer contract.
            unsafe {
                self.capture
                    .GetBuffer(&mut data, &mut frames, &mut flags, None, None)
            }
            .context("GetBuffer")?;
            let samples = (frames * 2) as usize;
            if flags & (AUDCLNT_BUFFERFLAGS_SILENT.0 as u32) != 0 || data.is_null() {
                out.extend(std::iter::repeat_n(0i16, samples));
            } else {
                // SAFETY: data points to `frames` interleaved stereo i16 samples, valid until
                // ReleaseBuffer is called below, per the GetBuffer contract.
                let slice = unsafe { std::slice::from_raw_parts(data as *const i16, samples) };
                out.extend_from_slice(slice);
            }
            // SAFETY: capture is a valid COM interface; frames matches the count from GetBuffer.
            unsafe { self.capture.ReleaseBuffer(frames) }.context("ReleaseBuffer")?;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Needs an audio render device. Run: cargo test -p castr-capture-win -- --ignored
    #[test]
    #[ignore]
    fn drains_audio_or_silence_for_200ms() {
        let mut cap = LoopbackCapture::new().unwrap();
        let mut out = Vec::new();
        for _ in 0..40 {
            cap.drain(&mut out).unwrap();
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        assert!(out.len() % 2 == 0);
        assert!(
            out.len() >= 48 * 2 * 150,
            "expected at least 150 ms of stereo samples, got {}",
            out.len() / 96
        );
    }
}
