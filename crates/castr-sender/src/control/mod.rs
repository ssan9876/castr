//! Stopping and observing a Miracast cast running in another process.
//!
//! A cast binds a loopback port and leaves a record naming it, so
//! `miracast-stop` and `miracast-status` can find it. The decisions are pure
//! and tested — the record's shape and staleness in [`record`], the protocol
//! in [`wire`], what a cast is sending in [`stats`] — and the sockets are the
//! thin shell in [`server`] and [`client`].
//!
//! See `docs/superpowers/specs/2026-09-04-castr-miracast-control-design.md`.

pub mod client;
pub mod record;
pub mod server;
pub mod stats;
pub mod wire;
