//! Forming a Wi-Fi Direct group with a Miracast display, on Windows.
//!
//! The radio half of casting to an ordinary display: find it, pair with it,
//! bring the group up, and hand back an address the media path can use. What
//! happens after that belongs to `castr-miracast`'s source modules, which do
//! not care whether the address came from a radio or from an ordinary network.
//!
//! WinRT needs real hardware, so nothing in `radio` can be unit-tested. Every
//! *decision* therefore lives in `select` and `failure`, which are pure and
//! build everywhere; `radio` is only the calls.

pub mod failure;
pub mod select;

#[cfg(windows)]
pub mod radio;
