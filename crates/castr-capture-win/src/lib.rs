//! Windows-only capture: Desktop Duplication video and WASAPI loopback audio.
#![cfg(windows)]
pub mod dxgi;
pub mod wasapi;
pub use dxgi::DesktopCapture;
pub use wasapi::LoopbackCapture;
