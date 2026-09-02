pub use castr_proto::Mode;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PixelFormat {
    Bgra,
    I420,
    Nv12,
}

/// A raw picture. `data` holds all planes contiguously. For I420: Y (w*h), U (w/2*h/2), V.
/// For NV12: Y, then interleaved UV. For BGRA: `stride` bytes per row.
#[derive(Debug, Clone)]
pub struct RawFrame {
    pub format: PixelFormat,
    pub width: u32,
    pub height: u32,
    pub stride: u32,
    pub data: Vec<u8>,
    pub timestamp_us: u64,
}

#[derive(Debug, Clone)]
pub struct EncodedFrame {
    pub data: Vec<u8>,
    pub keyframe: bool,
    pub timestamp_us: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EncoderConfig {
    pub width: u32,
    pub height: u32,
    pub fps: u32,
    pub bitrate_bps: u32,
    pub mode: Mode,
}

pub trait VideoEncoder: Send {
    fn encode(&mut self, frame: &RawFrame) -> anyhow::Result<Option<EncodedFrame>>;
    fn request_keyframe(&mut self);
    fn set_bitrate(&mut self, bitrate_bps: u32) -> anyhow::Result<()>;
    fn set_mode(&mut self, mode: Mode) -> anyhow::Result<()>;
    fn input_format(&self) -> PixelFormat;
    fn name(&self) -> &'static str;
}

pub trait VideoDecoder: Send {
    /// Feed one complete access unit (Annex B). May return zero or one frame.
    fn decode(&mut self, data: &[u8], timestamp_us: u64) -> anyhow::Result<Option<RawFrame>>;
    fn name(&self) -> &'static str;
}
