//! Media Foundation H.264 codecs. Windows only.
#![cfg(windows)]
pub mod encoder;
pub mod mf;
pub use encoder::MfEncoder;
