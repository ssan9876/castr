//! Media Foundation H.264 codecs. Windows only.
#![cfg(windows)]
pub mod decoder;
pub mod encoder;
pub mod mf;
pub use decoder::MfDecoder;
pub use encoder::MfEncoder;
