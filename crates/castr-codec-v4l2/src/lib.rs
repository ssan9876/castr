//! Hardware H.264 decode on Raspberry Pi through the V4L2 memory-to-memory
//! decoder (`bcm2835-codec`, `/dev/video10`). Linux only; on other targets this
//! crate is empty so the workspace still builds everywhere.

// Pure Rust, no OS dependency: their tests run in the Windows workspace suite too.
pub mod annexb;
pub mod sys;

#[cfg(target_os = "linux")]
pub mod decoder;
#[cfg(target_os = "linux")]
pub mod ops;
#[cfg(target_os = "linux")]
pub mod queue;
#[cfg(all(target_os = "linux", test))]
pub(crate) mod fake;

#[cfg(target_os = "linux")]
pub use decoder::V4l2Decoder;
