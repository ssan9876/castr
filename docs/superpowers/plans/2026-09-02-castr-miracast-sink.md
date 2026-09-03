# Miracast Wi-Fi Direct sink Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Press Windows+K on a PC with nothing installed, pick the Pi, type the PIN it shows on the TV, and see the desktop mirrored through the same hardware decoder and renderer castr's own protocol uses.

**Architecture:** A new Linux-only crate `castr-miracast` with four layers, each testable on its own: `p2p` drives `wpa_supplicant` over its control socket (Wi-Fi Display advertising, persistent group, WPS PIN); `dhcp` hands the source one address on the group interface; `rtsp` plus `wfd` negotiate the session on port 7236 and carry the Microsoft extensions; `rtp` plus `ts` turn the incoming stream into H.264 access units. The receiver gains a display arbiter so whichever protocol connects first owns the screen, and the sink feeds the existing `V4l2Decoder`, jitter buffer, clock and renderer unchanged.

**Tech Stack:** Rust, `wpa_supplicant` 2.10 over a Unix datagram control socket, hand-written RTSP/RTP/MPEG-TS/DHCP, the existing `castr-codec-v4l2` and `castr-receiver` pipeline. No new crates.

**Spec:** `docs/superpowers/specs/2026-09-02-castr-miracast-sink-design.md` (part two, sections 5 to 9)

## Global Constraints

- Linux-only and receiver-side. The Windows sender, castr's own protocol, and the Windows receiver are untouched. `castr-miracast` compiles to an empty crate on Windows, so `cargo test --workspace` stays green there.
- No new Rust dependencies. RTSP, RTP, MPEG-TS and DHCP are hand-written; `wpa_supplicant` is driven over its control socket.
- Video ceiling is 1280x720p30: advertise it as the only CEA mode and clear every other CEA, VESA and handheld bit, so the source cannot choose more than 2.4 GHz can carry.
- Audio is LPCM 48 kHz stereo 16-bit only. No AAC.
- `wfd_content_protection: none`. HDCP is out of scope; protected video shows black and that is documented, not worked around.
- Pairing is the WPS PIN display method: the Pi shows an eight-digit PIN through the receiver's existing overlay; a peer that has paired before is remembered in `paired.toml` under a `[miracast]` section keyed by P2P device address, separate from castr's fingerprint-keyed peers.
- One display: `DisplayOwner` is `Idle`, `Castr` or `Miracast`; the first to connect owns it, the other is refused (castr `Error { code: 5, message: "display busy" }`, RTSP `503`). Neither preempts the other.
- The DHCP range is `192.168.173.0/29`, deliberately far from the Pi's own LAN (`192.168.88.0/24`), so the source cannot confuse the two default routes.
- Keep-alive is `GET_PARAMETER` every 30 s; 60 s without a reply ends the session. An IDR request is rate-limited to one per 500 ms.
- Every commit: `cargo fmt -p <crate>`, clippy `-D warnings` for the new crate inside the Linux container (`bash scripts/pi/test-linux.sh`), `cargo test -q --workspace` green on Windows, and `bash scripts/pi/build-pi.sh` clean.
- Windows dev shell: `export PATH="$PATH:$HOME/.cargo/bin:/c/Program Files/CMake/bin"` before cargo. The Pi is `dietpi@192.168.88.157`, key auth, passwordless sudo, no SFTP (`cat file | ssh host 'cat > dest'`), no dbus (`sudo systemctl`, `sudo journalctl`), and never `pkill -f` inside an ssh command string.

## Findings this plan is built on (measured 2026-09-02)

- The Pi's `wpa_supplicant` is v2.10 and its build contains `P2P_FIND`, `P2P_GROUP_ADD`, `WFD_SUBELEM_SET` and `WPS_PIN`. `wpa_cli` is at `/sbin/wpa_cli`. The service is installed but `inactive` and `disabled`, and `/run/wpa_supplicant` does not exist, so the sink owns the supplicant lifecycle rather than fighting a system one.
- `wlan0` is `DOWN` and unused; the Pi's network is `eth0`. The radio reports `P2P-GO`, `P2P-client` and `P2P-device` modes, `{P2P-client, P2P-GO} <= 1` alongside `managed <= 2`, and Band 1 only (channels 1, 6, 11).
- No DHCP server of any kind is installed (`dnsmasq`, `dhcpd`, `udhcpd` all absent), so `dhcp.rs` is required, not a convenience.
- Ports 7331 and 7332 are already bound by the castr receiver; 7236 and the RTP port are free.
- On the Windows side `netsh wlan show wirelesscapabilities` reports Wi-Fi Direct Device, GO, Client and P2P Device Discovery all Supported; `WlanSvc` runs and `WFDSConMgrSvc` is Manual, which is normal (it starts on demand).

---

## File structure

```
crates/castr-miracast/
  Cargo.toml          linux-only deps: libc (socket, poll); dev-dep castr-media
  src/lib.rs          cfg gate; pub use Sink, SinkConfig, SinkEvent
  src/wfd.rs          WFD parameter encode/decode: capability strings, the
                      device-information subelement, the Microsoft extensions
  src/rtsp.rs         RTSP/1.0 message parse and format; the M1-M7 state machine
                      as a pure transition function over messages
  src/ts.rs           MPEG-TS demux: PAT, PMT, PID filter, continuity, PES
  src/rtp.rs          RTP parse, sequence reordering, loss detection
  src/dhcp.rs         DHCP DISCOVER/REQUEST parse and OFFER/ACK build
  src/p2p.rs          wpa_supplicant control socket: commands and unsolicited events
  src/session.rs      one sink session: owns the sockets, drives rtsp/rtp/ts,
                      emits SinkEvent, applies the recovery rules
  src/sink.rs         lifecycle: supplicant, group, DHCP, RTSP listener, restart
crates/castr-receiver/
  src/display.rs      NEW: DisplayOwner arbiter
  src/pipeline.rs     wire the arbiter into the castr path; feed sink events
  src/main.rs         --miracast, --miracast-name, --miracast-channel
docs/superpowers/verification/2026-09-02-castr-miracast-sink-e2e.md
```

Tasks 1 to 6 are pure parsing and state machines, testable on any platform with captured fixtures. Task 7 is the only one that talks to `wpa_supplicant`. Task 8 assembles a session and is provable by replaying a recorded stream. Task 9 wires the receiver. Task 10 is the hardware bring-up. Task 11 verifies end to end.

---

### Task 1: Crate scaffold and the WFD capability strings

**Files:**
- Create: `crates/castr-miracast/Cargo.toml`, `src/lib.rs`, `src/wfd.rs`
- Modify: `crates/castr-receiver/Cargo.toml`

**Interfaces:**
- Produces: `wfd::VideoFormats`, `wfd::AudioCodecs`, `wfd::ClientPorts`, `wfd::Capabilities`, `wfd::capabilities_body(&Capabilities) -> String`, `wfd::parse_parameter_body(&str) -> Vec<(String, String)>`, `wfd::device_info_subelement(&DeviceInfo) -> String`, `wfd::DeviceInfo { session_available: bool, rtsp_port: u16, max_throughput_mbps: u16 }`.

- [ ] **Step 1: Create the crate manifest**

```toml
# crates/castr-miracast/Cargo.toml
[package]
name = "castr-miracast"
version.workspace = true
edition.workspace = true
license.workspace = true

[dependencies]
anyhow.workspace = true
tracing.workspace = true
castr-media = { path = "../castr-media" }
castr-proto = { path = "../castr-proto" }

[target.'cfg(target_os = "linux")'.dependencies]
libc = "0.2"
```

- [ ] **Step 2: Write the gated lib root**

```rust
// crates/castr-miracast/src/lib.rs
//! A Miracast (Wi-Fi Display) sink: Wi-Fi Direct group owner, RTSP session,
//! MPEG-TS over RTP, decoded by the same pipeline castr's own protocol uses.
//! Linux only; on other targets this crate is empty so the workspace builds
//! everywhere.
//!
//! The parsing layers (`wfd`, `rtsp`, `ts`, `rtp`, `dhcp`) are pure and are
//! declared on every platform so their tests run in the Windows workspace
//! suite; only the parts that own sockets are Linux-gated.

// Declared as each task creates its file. The parsing and state-machine
// layers end up ungated so their tests run everywhere; only `sink`, which owns
// the supplicant and the sockets, is Linux-only.
pub mod rtsp;
pub mod wfd;
// Task 4: pub mod ts;
// Task 5: pub mod rtp;
// Task 6: pub mod dhcp;
// Task 7: pub mod p2p;
// Task 8: pub mod session;
// Task 10: #[cfg(target_os = "linux")] pub mod sink;
```

Create `rtsp.rs` and `wfd.rs` in this task, and in `lib.rs` declare only those two.
Every later task adds its own `pub mod` line when it creates the file, because a
`mod` declaration whose file does not exist does not compile. The block above
shows where each module ends up: `dhcp`, `p2p`, `rtp`, `ts` and `session` finish
ungated (they are pure, and their tests run in the Windows workspace suite),
while `sink` stays behind `#[cfg(target_os = "linux")]`.

- [ ] **Step 3: Write the failing tests**

At the bottom of `wfd.rs`. The capability strings are from the Wi-Fi Display 1.1 specification's parameter tables; the video-format bitmap has one bit set, CEA index 5, which is 1280x720p30.

```rust
#[cfg(test)]
mod tests {
    use super::*;

    fn caps() -> Capabilities {
        Capabilities {
            video: VideoFormats::only_720p30(),
            audio: AudioCodecs::lpcm_48k_stereo(),
            ports: ClientPorts { rtp_port: 5000 },
            max_bitrate_kbps: 8000,
            latency_management: true,
            format_change: true,
        }
    }

    #[test]
    fn the_video_format_line_advertises_only_720p30() {
        let body = capabilities_body(&caps());
        let line = body
            .lines()
            .find(|l| l.starts_with("wfd_video_formats:"))
            .expect("video formats");
        // native 0x40 = CEA table, index 1; profile/level constant for CBP@3.1;
        // CEA bitmap 0x00000020 is index 5 (1280x720p30); VESA and HH are zero.
        assert_eq!(
            line,
            "wfd_video_formats: 40 00 02 04 00000020 00000000 00000000 00 0000 0000 00 none none"
        );
    }

    #[test]
    fn the_audio_line_advertises_lpcm_only() {
        let body = capabilities_body(&caps());
        assert!(
            body.contains("wfd_audio_codecs: LPCM 00000002 00"),
            "48 kHz stereo bit only: {body}"
        );
    }

    #[test]
    fn content_protection_is_none_and_uibc_is_absent() {
        let body = capabilities_body(&caps());
        assert!(body.contains("wfd_content_protection: none"));
        assert!(!body.contains("wfd_uibc_capability"), "no input back channel");
    }

    #[test]
    fn the_client_port_line_names_our_rtp_port() {
        let body = capabilities_body(&caps());
        assert!(
            body.contains("wfd_client_rtp_ports: RTP/AVP/UDP;unicast 5000 0 mode=play"),
            "{body}"
        );
    }

    #[test]
    fn the_microsoft_extensions_are_advertised() {
        let body = capabilities_body(&caps());
        assert!(body.contains("microsoft_max_bitrate: 8000"), "{body}");
        assert!(body.contains("microsoft_latency_management_capability: supported"));
        assert!(body.contains("microsoft_format_change_support: supported"));
    }

    #[test]
    fn every_line_ends_with_crlf_and_the_body_does_too() {
        let body = capabilities_body(&caps());
        assert!(body.ends_with("\r\n"));
        for line in body.split_terminator("\r\n") {
            assert!(!line.contains('\n'), "stray newline in {line:?}");
        }
    }

    #[test]
    fn a_parameter_body_parses_into_pairs() {
        let body = "wfd_video_formats: 40 00 02 04 00000020 00000000 00000000 00 0000 0000 00 none none\r\n\
                    wfd_presentation_URL: rtsp://192.168.173.1/wfd1.0/streamid=0 none\r\n\
                    wfd_client_rtp_ports: RTP/AVP/UDP;unicast 5000 0 mode=play\r\n";
        let pairs = parse_parameter_body(body);
        assert_eq!(pairs.len(), 3);
        assert_eq!(pairs[0].0, "wfd_video_formats");
        assert_eq!(pairs[1].1, "rtsp://192.168.173.1/wfd1.0/streamid=0 none");
        assert_eq!(pairs[2].0, "wfd_client_rtp_ports");
    }

    #[test]
    fn a_get_parameter_request_body_lists_bare_names() {
        // M3 asks for parameters by name, one per line, with no colon.
        let pairs = parse_parameter_body("wfd_video_formats\r\nwfd_audio_codecs\r\n");
        assert_eq!(pairs.len(), 2);
        assert_eq!(pairs[0], ("wfd_video_formats".to_string(), String::new()));
    }

    #[test]
    fn the_device_information_subelement_says_sink_and_available() {
        let s = device_info_subelement(&DeviceInfo {
            session_available: true,
            rtsp_port: 7236,
            max_throughput_mbps: 10,
        });
        // 6 bytes: device info (2), control port (2), throughput (2).
        // 0x0011 = primary sink (bits 0-1) with session available (bits 4-5).
        assert_eq!(s, "00060011 1c44 000a");
    }

    #[test]
    fn an_unavailable_sink_clears_the_availability_bits() {
        let s = device_info_subelement(&DeviceInfo {
            session_available: false,
            rtsp_port: 7236,
            max_throughput_mbps: 10,
        });
        assert_eq!(s, "00060001 1c44 000a");
    }
}
```

- [ ] **Step 4: Run the tests to watch them fail**

Run: `cargo test -q -p castr-miracast wfd::`
Expected: compile errors, the types do not exist.

- [ ] **Step 5: Implement `wfd.rs`**

```rust
// crates/castr-miracast/src/wfd.rs
//! Wi-Fi Display parameters: the capability strings the sink sends in RTSP
//! bodies, and the device-information subelement it advertises in beacons.
//!
//! The wire formats here are from the Wi-Fi Display 1.1 specification's
//! parameter tables plus the Microsoft extensions (MS-WFDPE). Every value the
//! sink emits is pinned by a test against a literal string, because a wrong
//! bit in a capability bitmap does not fail loudly: the source simply picks a
//! format we cannot decode, or refuses to connect at all.

/// Video formats we accept. This sink advertises exactly one: 1280x720p30.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VideoFormats {
    /// CEA resolution bitmap; bit 5 is 1280x720p30.
    pub cea: u32,
    pub vesa: u32,
    pub hh: u32,
    /// H.264 profile bitmap: 0x02 is Constrained Baseline.
    pub profile: u8,
    /// Level bitmap: 0x04 is level 3.1, which covers 720p30.
    pub level: u8,
}

impl VideoFormats {
    pub fn only_720p30() -> Self {
        Self { cea: 0x0000_0020, vesa: 0, hh: 0, profile: 0x02, level: 0x04 }
    }
}

/// Audio formats we accept: LPCM 48 kHz 16-bit stereo only (bit 1).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AudioCodecs {
    pub lpcm_modes: u32,
}

impl AudioCodecs {
    pub fn lpcm_48k_stereo() -> Self {
        Self { lpcm_modes: 0x0000_0002 }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ClientPorts {
    pub rtp_port: u16,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Capabilities {
    pub video: VideoFormats,
    pub audio: AudioCodecs,
    pub ports: ClientPorts,
    /// Ceiling we ask the source to respect, in kbit/s (MS-WFDPE).
    pub max_bitrate_kbps: u32,
    pub latency_management: bool,
    pub format_change: bool,
}

/// The body of an M3 response: one `name: value` per line, CRLF terminated.
pub fn capabilities_body(c: &Capabilities) -> String {
    let mut s = String::new();
    // native: 0x40 = CEA table, index 1 (the table our single mode lives in).
    // preferred-display-mode: 00. Then profile, level, the three bitmaps,
    // latency, min-slice, slice-enc, frame-rate-control, then max hres/vres
    // ("none none" = no constraint beyond the bitmaps).
    s.push_str(&format!(
        "wfd_video_formats: 40 00 {:02X} {:02X} {:08X} {:08X} {:08X} 00 0000 0000 00 none none\r\n",
        c.video.profile, c.video.level, c.video.cea, c.video.vesa, c.video.hh
    ));
    s.push_str(&format!("wfd_audio_codecs: LPCM {:08X} 00\r\n", c.audio.lpcm_modes));
    s.push_str("wfd_content_protection: none\r\n");
    s.push_str(&format!(
        "wfd_client_rtp_ports: RTP/AVP/UDP;unicast {} 0 mode=play\r\n",
        c.ports.rtp_port
    ));
    s.push_str(&format!("microsoft_max_bitrate: {}\r\n", c.max_bitrate_kbps));
    if c.latency_management {
        s.push_str("microsoft_latency_management_capability: supported\r\n");
    }
    if c.format_change {
        s.push_str("microsoft_format_change_support: supported\r\n");
    }
    s
}

/// Splits an RTSP parameter body into `(name, value)` pairs. A line with no
/// colon (an M3 request, which asks for parameters by name) yields an empty
/// value rather than being dropped, so the caller can answer it.
pub fn parse_parameter_body(body: &str) -> Vec<(String, String)> {
    body.lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .map(|l| match l.split_once(':') {
            Some((k, v)) => (k.trim().to_string(), v.trim().to_string()),
            None => (l.to_string(), String::new()),
        })
        .collect()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DeviceInfo {
    pub session_available: bool,
    pub rtsp_port: u16,
    pub max_throughput_mbps: u16,
}

/// The WFD IE device-information subelement, in the hex form
/// `wpa_supplicant`'s `WFD_SUBELEM_SET 0 <hex>` expects: a 2-byte length
/// followed by the 6-byte body.
///
/// Device information bits: 0b01 = primary sink, 0x0010 = WSD support,
/// and 0b01 in the session-availability field (bits 4-5) = available.
pub fn device_info_subelement(d: &DeviceInfo) -> String {
    // Bits 0-1: device type, 01 = primary sink.
    // Bits 4-5: session availability, 01 = available, 00 = not available.
    let mut info: u16 = 0x0001;
    if d.session_available {
        info |= 0x0010;
    }
    format!("0006{:04X} {:04X} {:04X}", info, d.rtsp_port, d.max_throughput_mbps)
}
```

- [ ] **Step 6: Run the tests**

Run: `cargo test -q -p castr-miracast wfd::`
Expected: 10 passed. If a literal disagrees with the implementation, fix the implementation: the literals encode the specification's tables, and a wrong bitmap is exactly the failure this task exists to prevent.

- [ ] **Step 7: Wire the receiver dependency**

Append to `crates/castr-receiver/Cargo.toml`, under the existing `[target.'cfg(target_os = "linux")'.dependencies]`:

```toml
castr-miracast = { path = "../castr-miracast" }
```

- [ ] **Step 8: Verify both platforms**

Run: `cargo test -q --workspace` (Windows) and `bash scripts/pi/build-pi.sh`.
Expected: green; the cross-build ends with `built dist/castr-receiver-aarch64`.

- [ ] **Step 9: Commit**

```bash
cargo fmt -p castr-miracast
git add crates/castr-miracast crates/castr-receiver/Cargo.toml Cargo.lock
git commit -m "feat(miracast): crate scaffold and the WFD capability strings"
```

---

### Task 2: RTSP messages

**Files:**
- Create: `crates/castr-miracast/src/rtsp.rs` (replacing the placeholder from Task 1)

**Interfaces:**
- Produces: `rtsp::Message { pub start: StartLine, pub headers: Vec<(String, String)>, pub body: String }`, `rtsp::StartLine::{Request { method: String, uri: String }, Response { status: u16, reason: String }}`, `rtsp::parse(&[u8]) -> Result<Option<(Message, usize)>, ParseError>`, `Message::format(&self) -> String`, `Message::header(&self, name: &str) -> Option<&str>`, `Message::cseq(&self) -> Option<u32>`, `rtsp::response(status: u16, cseq: u32, body: &str) -> Message`, `rtsp::request(method: &str, uri: &str, cseq: u32, body: &str) -> Message`.

- [ ] **Step 1: Write the failing tests**

The fixtures are the real M1 to M7 exchange a Windows source performs, in the order the sink sees them.

```rust
#[cfg(test)]
mod tests {
    use super::*;

    const M1: &[u8] = b"OPTIONS * RTSP/1.0\r\nCSeq: 1\r\nRequire: org.wfa.wfd1.0\r\n\r\n";

    const M3: &[u8] = b"GET_PARAMETER rtsp://localhost/wfd1.0 RTSP/1.0\r\n\
CSeq: 2\r\n\
Content-Type: text/parameters\r\n\
Content-Length: 42\r\n\
\r\n\
wfd_video_formats\r\nwfd_audio_codecs\r\nwfd_client_rtp_ports\r\n";

    #[test]
    fn a_request_without_a_body_parses_whole() {
        let (m, used) = parse(M1).unwrap().unwrap();
        assert_eq!(used, M1.len());
        match &m.start {
            StartLine::Request { method, uri } => {
                assert_eq!(method, "OPTIONS");
                assert_eq!(uri, "*");
            }
            other => panic!("{other:?}"),
        }
        assert_eq!(m.cseq(), Some(1));
        assert_eq!(m.header("require"), Some("org.wfa.wfd1.0"));
        assert!(m.body.is_empty());
    }

    #[test]
    fn header_lookup_ignores_case() {
        let (m, _) = parse(M1).unwrap().unwrap();
        assert_eq!(m.header("CSeq"), m.header("cseq"));
    }

    #[test]
    fn a_body_is_read_by_content_length() {
        let (m, used) = parse(M3).unwrap().unwrap();
        assert_eq!(used, M3.len());
        assert_eq!(m.body.len(), 42);
        assert!(m.body.starts_with("wfd_video_formats"));
    }

    #[test]
    fn an_incomplete_message_asks_for_more() {
        assert!(parse(&M1[..10]).unwrap().is_none());
        // Headers complete, body short.
        let cut = M3.len() - 5;
        assert!(parse(&M3[..cut]).unwrap().is_none());
    }

    #[test]
    fn two_messages_in_one_buffer_are_returned_one_at_a_time() {
        let mut buf = M1.to_vec();
        buf.extend_from_slice(M1);
        let (_, used) = parse(&buf).unwrap().unwrap();
        assert_eq!(used, M1.len());
        let (m2, used2) = parse(&buf[used..]).unwrap().unwrap();
        assert_eq!(used2, M1.len());
        assert_eq!(m2.cseq(), Some(1));
    }

    #[test]
    fn a_response_start_line_parses() {
        let raw = b"RTSP/1.0 200 OK\r\nCSeq: 2\r\n\r\n";
        let (m, _) = parse(raw).unwrap().unwrap();
        match &m.start {
            StartLine::Response { status, reason } => {
                assert_eq!(*status, 200);
                assert_eq!(reason, "OK");
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn a_malformed_start_line_is_an_error_not_a_hang() {
        let raw = b"GARBAGE\r\n\r\n";
        assert!(parse(raw).is_err());
    }

    #[test]
    fn a_response_formats_with_content_length_and_crlf() {
        let m = response(200, 7, "wfd_video_formats: 40\r\n");
        let s = m.format();
        assert!(s.starts_with("RTSP/1.0 200 OK\r\nCSeq: 7\r\n"));
        assert!(s.contains("Content-Type: text/parameters\r\n"));
        assert!(s.contains("Content-Length: 23\r\n"));
        assert!(s.ends_with("\r\n\r\nwfd_video_formats: 40\r\n"));
    }

    #[test]
    fn an_empty_response_carries_no_content_headers() {
        let s = response(200, 3, "").format();
        assert!(!s.contains("Content-Length"), "{s}");
        assert!(s.ends_with("\r\n\r\n"));
    }

    #[test]
    fn a_request_formats_with_its_uri_and_body() {
        let s = request("SET_PARAMETER", "rtsp://192.168.173.2/wfd1.0", 9, "wfd_idr_request\r\n").format();
        assert!(s.starts_with("SET_PARAMETER rtsp://192.168.173.2/wfd1.0 RTSP/1.0\r\n"));
        assert!(s.contains("CSeq: 9\r\n"));
        assert!(s.ends_with("wfd_idr_request\r\n"));
    }

    #[test]
    fn a_formatted_message_parses_back_identically() {
        let m = response(200, 42, "wfd_audio_codecs: LPCM 00000002 00\r\n");
        let text = m.format();
        let (back, used) = parse(text.as_bytes()).unwrap().unwrap();
        assert_eq!(used, text.len());
        assert_eq!(back.cseq(), Some(42));
        assert_eq!(back.body, m.body);
    }
}
```

- [ ] **Step 2: Run to watch them fail**

Run: `cargo test -q -p castr-miracast rtsp::`
Expected: compile errors.

- [ ] **Step 3: Implement `rtsp.rs`**

```rust
// crates/castr-miracast/src/rtsp.rs
//! RTSP/1.0 messages, enough for the Wi-Fi Display exchange. Pure parsing and
//! formatting over byte slices: the caller owns the socket and the buffer, so
//! this module is testable without one.

use std::fmt::Write as _;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StartLine {
    Request { method: String, uri: String },
    Response { status: u16, reason: String },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Message {
    pub start: StartLine,
    pub headers: Vec<(String, String)>,
    pub body: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ParseError {
    MalformedStartLine(String),
    MalformedHeader(String),
    BadContentLength(String),
    NotUtf8,
}

impl std::fmt::Display for ParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ParseError::MalformedStartLine(s) => write!(f, "malformed start line: {s:?}"),
            ParseError::MalformedHeader(s) => write!(f, "malformed header: {s:?}"),
            ParseError::BadContentLength(s) => write!(f, "bad Content-Length: {s:?}"),
            ParseError::NotUtf8 => write!(f, "message is not UTF-8"),
        }
    }
}

impl std::error::Error for ParseError {}

impl Message {
    /// Case-insensitive header lookup, as RTSP requires.
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }

    pub fn cseq(&self) -> Option<u32> {
        self.header("cseq").and_then(|v| v.trim().parse().ok())
    }

    pub fn format(&self) -> String {
        let mut s = String::new();
        match &self.start {
            StartLine::Request { method, uri } => {
                let _ = write!(s, "{method} {uri} RTSP/1.0\r\n");
            }
            StartLine::Response { status, reason } => {
                let _ = write!(s, "RTSP/1.0 {status} {reason}\r\n");
            }
        }
        for (k, v) in &self.headers {
            let _ = write!(s, "{k}: {v}\r\n");
        }
        if !self.body.is_empty() {
            let _ = write!(s, "Content-Type: text/parameters\r\n");
            let _ = write!(s, "Content-Length: {}\r\n", self.body.len());
        }
        s.push_str("\r\n");
        s.push_str(&self.body);
        s
    }
}

/// Parses one message from the front of `buf`.
///
/// `Ok(None)` means the buffer does not yet hold a complete message and the
/// caller should read more. `Ok(Some((msg, used)))` returns the message and
/// how many bytes it consumed, so the caller can drain exactly that much.
pub fn parse(buf: &[u8]) -> Result<Option<(Message, usize)>, ParseError> {
    let Some(head_end) = find_double_crlf(buf) else {
        return Ok(None);
    };
    let head = std::str::from_utf8(&buf[..head_end]).map_err(|_| ParseError::NotUtf8)?;
    let mut lines = head.split("\r\n");
    let start_line = lines.next().unwrap_or_default();
    let start = parse_start_line(start_line)?;
    let mut headers = Vec::new();
    for line in lines {
        if line.is_empty() {
            continue;
        }
        let (k, v) = line
            .split_once(':')
            .ok_or_else(|| ParseError::MalformedHeader(line.to_string()))?;
        headers.push((k.trim().to_string(), v.trim().to_string()));
    }
    let body_len = match headers
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case("content-length"))
    {
        Some((_, v)) => v
            .trim()
            .parse::<usize>()
            .map_err(|_| ParseError::BadContentLength(v.clone()))?,
        None => 0,
    };
    let body_start = head_end + 4;
    if buf.len() < body_start + body_len {
        return Ok(None);
    }
    let body = std::str::from_utf8(&buf[body_start..body_start + body_len])
        .map_err(|_| ParseError::NotUtf8)?
        .to_string();
    Ok(Some((Message { start, headers, body }, body_start + body_len)))
}

fn find_double_crlf(buf: &[u8]) -> Option<usize> {
    buf.windows(4).position(|w| w == b"\r\n\r\n")
}

fn parse_start_line(line: &str) -> Result<StartLine, ParseError> {
    if let Some(rest) = line.strip_prefix("RTSP/1.0 ") {
        let (code, reason) = rest.split_once(' ').unwrap_or((rest, ""));
        let status = code
            .parse()
            .map_err(|_| ParseError::MalformedStartLine(line.to_string()))?;
        return Ok(StartLine::Response { status, reason: reason.to_string() });
    }
    let mut parts = line.split(' ');
    let method = parts.next().unwrap_or_default();
    let uri = parts.next();
    let version = parts.next();
    match (method.is_empty(), uri, version) {
        (false, Some(uri), Some(v)) if v.starts_with("RTSP/") => Ok(StartLine::Request {
            method: method.to_string(),
            uri: uri.to_string(),
        }),
        _ => Err(ParseError::MalformedStartLine(line.to_string())),
    }
}

fn reason_for(status: u16) -> &'static str {
    match status {
        200 => "OK",
        400 => "Bad Request",
        404 => "Not Found",
        454 => "Session Not Found",
        503 => "Service Unavailable",
        _ => "Error",
    }
}

pub fn response(status: u16, cseq: u32, body: &str) -> Message {
    Message {
        start: StartLine::Response { status, reason: reason_for(status).to_string() },
        headers: vec![("CSeq".into(), cseq.to_string())],
        body: body.to_string(),
    }
}

pub fn request(method: &str, uri: &str, cseq: u32, body: &str) -> Message {
    Message {
        start: StartLine::Request { method: method.into(), uri: uri.into() },
        headers: vec![("CSeq".into(), cseq.to_string())],
        body: body.to_string(),
    }
}
```

- [ ] **Step 4: Run the tests**

Run: `cargo test -q -p castr-miracast rtsp::`
Expected: 11 passed.

- [ ] **Step 5: Commit**

```bash
cargo fmt -p castr-miracast
git add crates/castr-miracast/src/rtsp.rs
git commit -m "feat(miracast): RTSP message parsing and formatting"
```

---

### Task 3: The M1-M7 negotiation state machine

**Files:**
- Modify: `crates/castr-miracast/src/rtsp.rs` (append the `session` submodule)

**Interfaces:**
- Consumes: `Message`, `StartLine`, `response`, `request`, `wfd::{Capabilities, capabilities_body, parse_parameter_body}`.
- Produces: `rtsp::Negotiation::new(Capabilities, session_id: String) -> Negotiation`, `Negotiation::state(&self) -> NegState`, `Negotiation::on_message(&mut self, &Message) -> Vec<Action>`, `Negotiation::tick(&mut self, now: Instant) -> Vec<Action>`, `Negotiation::request_idr(&mut self, now: Instant) -> Vec<Action>`, `enum Action { Send(Message), Play, Teardown(&'static str) }`, `enum NegState { Init, Capabilities, Ready, Playing, Done }`, and `Negotiation::chosen_video(&self) -> Option<VideoMode>` with `VideoMode { width: u32, height: u32, fps: u32 }`.

The whole exchange is a pure function of the messages seen, so it is tested without a socket.

- [ ] **Step 1: Write the failing tests**

```rust
#[cfg(test)]
mod negotiation_tests {
    use super::*;
    use crate::wfd::{AudioCodecs, Capabilities, ClientPorts, VideoFormats};
    use std::time::{Duration, Instant};

    fn caps() -> Capabilities {
        Capabilities {
            video: VideoFormats::only_720p30(),
            audio: AudioCodecs::lpcm_48k_stereo(),
            ports: ClientPorts { rtp_port: 5000 },
            max_bitrate_kbps: 8000,
            latency_management: true,
            format_change: true,
        }
    }

    fn neg() -> Negotiation {
        Negotiation::new(caps(), "01234567".into())
    }

    fn req(method: &str, cseq: u32, body: &str) -> Message {
        request(method, "rtsp://localhost/wfd1.0", cseq, body)
    }

    fn sent(actions: &[Action]) -> Vec<String> {
        actions
            .iter()
            .filter_map(|a| match a {
                Action::Send(m) => Some(m.format()),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn m1_options_is_answered_with_our_methods_and_then_we_ask_theirs() {
        let mut n = neg();
        let out = n.on_message(&req("OPTIONS", 1, ""));
        let msgs = sent(&out);
        assert_eq!(msgs.len(), 2, "answer M1 and send M2");
        assert!(msgs[0].starts_with("RTSP/1.0 200 OK\r\nCSeq: 1\r\n"));
        assert!(msgs[0].contains("Public: org.wfa.wfd1.0, SETUP, TEARDOWN, PLAY, PAUSE, GET_PARAMETER, SET_PARAMETER"));
        assert!(msgs[1].starts_with("OPTIONS * RTSP/1.0\r\n"), "{}", msgs[1]);
        assert_eq!(n.state(), NegState::Init);
    }

    #[test]
    fn m3_get_parameter_is_answered_with_our_capabilities() {
        let mut n = neg();
        n.on_message(&req("OPTIONS", 1, ""));
        let out = n.on_message(&req(
            "GET_PARAMETER",
            2,
            "wfd_video_formats\r\nwfd_audio_codecs\r\nwfd_client_rtp_ports\r\n",
        ));
        let msgs = sent(&out);
        assert_eq!(msgs.len(), 1);
        assert!(msgs[0].contains("wfd_video_formats: 40 00 02 04 00000020"));
        assert!(msgs[0].contains("wfd_audio_codecs: LPCM 00000002 00"));
        assert!(msgs[0].contains("microsoft_max_bitrate: 8000"));
        assert_eq!(n.state(), NegState::Capabilities);
    }

    #[test]
    fn m4_set_parameter_records_the_chosen_mode_and_is_acknowledged() {
        let mut n = neg();
        n.on_message(&req("OPTIONS", 1, ""));
        n.on_message(&req("GET_PARAMETER", 2, "wfd_video_formats\r\n"));
        let out = n.on_message(&req(
            "SET_PARAMETER",
            3,
            "wfd_video_formats: 00 00 02 04 00000020 00000000 00000000 00 0000 0000 00 none none\r\n\
             wfd_presentation_URL: rtsp://192.168.173.1/wfd1.0/streamid=0 none\r\n",
        ));
        assert_eq!(sent(&out).len(), 1);
        assert!(sent(&out)[0].starts_with("RTSP/1.0 200 OK\r\nCSeq: 3\r\n"));
        assert_eq!(
            n.chosen_video(),
            Some(VideoMode { width: 1280, height: 720, fps: 30 })
        );
    }

    #[test]
    fn a_source_that_picks_an_unadvertised_mode_is_refused() {
        let mut n = neg();
        n.on_message(&req("OPTIONS", 1, ""));
        n.on_message(&req("GET_PARAMETER", 2, "wfd_video_formats\r\n"));
        // CEA bit 16 (1920x1080p30), which we never advertised.
        let out = n.on_message(&req(
            "SET_PARAMETER",
            3,
            "wfd_video_formats: 00 00 02 04 00010000 00000000 00000000 00 0000 0000 00 none none\r\n",
        ));
        let msgs = sent(&out);
        assert!(msgs[0].starts_with("RTSP/1.0 400"), "{}", msgs[0]);
        assert!(n.chosen_video().is_none());
    }

    #[test]
    fn m5_trigger_setup_makes_us_send_setup_then_play() {
        let mut n = neg();
        n.on_message(&req("OPTIONS", 1, ""));
        n.on_message(&req("GET_PARAMETER", 2, "wfd_video_formats\r\n"));
        n.on_message(&req(
            "SET_PARAMETER",
            3,
            "wfd_video_formats: 00 00 02 04 00000020 00000000 00000000 00 0000 0000 00 none none\r\n\
             wfd_presentation_URL: rtsp://192.168.173.1/wfd1.0/streamid=0 none\r\n",
        ));
        let out = n.on_message(&req("SET_PARAMETER", 4, "wfd_trigger_method: SETUP\r\n"));
        let msgs = sent(&out);
        assert_eq!(msgs.len(), 2, "ack the trigger, then send M6 SETUP");
        assert!(msgs[1].starts_with("SETUP rtsp://192.168.173.1/wfd1.0/streamid=0 RTSP/1.0\r\n"), "{}", msgs[1]);
        assert!(msgs[1].contains("Transport: RTP/AVP/UDP;unicast;client_port=5000\r\n"));
        assert_eq!(n.state(), NegState::Ready);
    }

    #[test]
    fn a_setup_response_with_a_session_makes_us_play_and_start_media() {
        let mut n = neg();
        n.on_message(&req("OPTIONS", 1, ""));
        n.on_message(&req("GET_PARAMETER", 2, "wfd_video_formats\r\n"));
        n.on_message(&req(
            "SET_PARAMETER",
            3,
            "wfd_video_formats: 00 00 02 04 00000020 00000000 00000000 00 0000 0000 00 none none\r\n\
             wfd_presentation_URL: rtsp://192.168.173.1/wfd1.0/streamid=0 none\r\n",
        ));
        n.on_message(&req("SET_PARAMETER", 4, "wfd_trigger_method: SETUP\r\n"));
        let mut ok = response(200, 100, "");
        ok.headers.push(("Session".into(), "abcdef12;timeout=60".into()));
        let out = n.on_message(&ok);
        let msgs = sent(&out);
        assert!(msgs[0].starts_with("PLAY rtsp://192.168.173.1/wfd1.0/streamid=0 RTSP/1.0\r\n"), "{}", msgs[0]);
        assert!(msgs[0].contains("Session: abcdef12\r\n"));
        assert!(out.iter().any(|a| matches!(a, Action::Play)), "media starts");
        assert_eq!(n.state(), NegState::Playing);
    }

    #[test]
    fn keep_alive_goes_out_every_thirty_seconds_and_silence_ends_the_session() {
        let mut n = neg();
        let t0 = Instant::now();
        n.on_message(&req("OPTIONS", 1, ""));
        assert!(sent(&n.tick(t0 + Duration::from_secs(29))).is_empty());
        let out = n.tick(t0 + Duration::from_secs(31));
        assert!(sent(&out)[0].starts_with("GET_PARAMETER "), "{:?}", sent(&out));
        // No reply for 60 s after the last one heard: tear down.
        let out = n.tick(t0 + Duration::from_secs(95));
        assert!(
            out.iter().any(|a| matches!(a, Action::Teardown(_))),
            "{out:?}"
        );
    }

    #[test]
    fn an_idr_request_is_rate_limited_to_one_per_five_hundred_milliseconds() {
        let mut n = neg();
        let t0 = Instant::now();
        n.on_message(&req("OPTIONS", 1, ""));
        assert_eq!(sent(&n.request_idr(t0)).len(), 1);
        assert!(sent(&n.request_idr(t0 + Duration::from_millis(200))).is_empty());
        let out = n.request_idr(t0 + Duration::from_millis(600));
        assert_eq!(sent(&out).len(), 1);
        assert!(sent(&out)[0].contains("wfd_idr_request"), "{:?}", sent(&out));
    }

    #[test]
    fn teardown_from_the_source_ends_the_session() {
        let mut n = neg();
        n.on_message(&req("OPTIONS", 1, ""));
        let out = n.on_message(&req("TEARDOWN", 9, ""));
        assert!(sent(&out)[0].starts_with("RTSP/1.0 200 OK\r\nCSeq: 9\r\n"));
        assert!(out.iter().any(|a| matches!(a, Action::Teardown(_))));
        assert_eq!(n.state(), NegState::Done);
    }

    #[test]
    fn an_unknown_method_is_refused_without_ending_the_session() {
        let mut n = neg();
        n.on_message(&req("OPTIONS", 1, ""));
        let out = n.on_message(&req("ANNOUNCE", 5, ""));
        assert!(sent(&out)[0].starts_with("RTSP/1.0 400"));
        assert!(!out.iter().any(|a| matches!(a, Action::Teardown(_))));
    }
}
```

- [ ] **Step 2: Run to watch them fail**

Run: `cargo test -q -p castr-miracast negotiation`
Expected: compile errors.

- [ ] **Step 3: Implement the negotiation, appended to `rtsp.rs`**

```rust
// ---- negotiation ----

use crate::wfd::{capabilities_body, parse_parameter_body, Capabilities};
use std::time::{Duration, Instant};

/// How often we send a keep-alive, and how long we tolerate silence.
const KEEPALIVE_EVERY: Duration = Duration::from_secs(30);
const KEEPALIVE_TIMEOUT: Duration = Duration::from_secs(60);
/// One IDR request per this interval, however often the decoder asks.
const IDR_MIN_INTERVAL: Duration = Duration::from_millis(500);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VideoMode {
    pub width: u32,
    pub height: u32,
    pub fps: u32,
}

/// CEA table index to resolution. Only the modes we might see are listed; the
/// sink advertises index 5 alone, so anything else is a source error.
fn cea_mode(bit: u32) -> Option<VideoMode> {
    Some(match bit {
        5 => VideoMode { width: 1280, height: 720, fps: 30 },
        _ => return None,
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NegState {
    Init,
    Capabilities,
    Ready,
    Playing,
    Done,
}

#[derive(Debug)]
pub enum Action {
    Send(Message),
    /// Media may start flowing; the caller opens its RTP socket.
    Play,
    Teardown(&'static str),
}

pub struct Negotiation {
    caps: Capabilities,
    session_id: String,
    state: NegState,
    next_cseq: u32,
    presentation_url: String,
    chosen: Option<VideoMode>,
    peer_session: Option<String>,
    last_heard: Option<Instant>,
    last_keepalive: Option<Instant>,
    last_idr: Option<Instant>,
}

impl Negotiation {
    pub fn new(caps: Capabilities, session_id: String) -> Self {
        Self {
            caps,
            session_id,
            state: NegState::Init,
            next_cseq: 100,
            presentation_url: String::new(),
            chosen: None,
            peer_session: None,
            last_heard: None,
            last_keepalive: None,
            last_idr: None,
        }
    }

    pub fn state(&self) -> NegState {
        self.state
    }

    pub fn chosen_video(&self) -> Option<VideoMode> {
        self.chosen
    }

    fn cseq(&mut self) -> u32 {
        let c = self.next_cseq;
        self.next_cseq += 1;
        c
    }

    pub fn on_message(&mut self, m: &Message) -> Vec<Action> {
        self.last_heard = Some(Instant::now());
        match &m.start {
            StartLine::Request { method, .. } => self.on_request(method.as_str(), m),
            StartLine::Response { status, .. } => self.on_response(*status, m),
        }
    }

    fn on_request(&mut self, method: &str, m: &Message) -> Vec<Action> {
        let cseq = m.cseq().unwrap_or(0);
        match method {
            "OPTIONS" => {
                let mut ok = response(200, cseq, "");
                ok.headers.push((
                    "Public".into(),
                    "org.wfa.wfd1.0, SETUP, TEARDOWN, PLAY, PAUSE, GET_PARAMETER, SET_PARAMETER"
                        .into(),
                ));
                // M2: ask what the source supports, as the exchange requires.
                let c = self.cseq();
                let mut m2 = request("OPTIONS", "*", c, "");
                m2.headers.push(("Require".into(), "org.wfa.wfd1.0".into()));
                vec![Action::Send(ok), Action::Send(m2)]
            }
            "GET_PARAMETER" => {
                // M3, or a keep-alive with an empty body.
                if m.body.trim().is_empty() {
                    return vec![Action::Send(response(200, cseq, ""))];
                }
                self.state = NegState::Capabilities;
                vec![Action::Send(response(200, cseq, &capabilities_body(&self.caps)))]
            }
            "SET_PARAMETER" => self.on_set_parameter(cseq, m),
            "TEARDOWN" => {
                self.state = NegState::Done;
                vec![
                    Action::Send(response(200, cseq, "")),
                    Action::Teardown("source sent TEARDOWN"),
                ]
            }
            _ => vec![Action::Send(response(400, cseq, ""))],
        }
    }

    fn on_set_parameter(&mut self, cseq: u32, m: &Message) -> Vec<Action> {
        let params = parse_parameter_body(&m.body);
        let mut out = Vec::new();
        let mut trigger = None;
        for (name, value) in &params {
            match name.as_str() {
                "wfd_video_formats" => match Self::parse_chosen_video(value) {
                    Some(mode) => self.chosen = Some(mode),
                    None => {
                        // The source picked something we never advertised.
                        return vec![Action::Send(response(400, cseq, ""))];
                    }
                },
                "wfd_presentation_URL" => {
                    self.presentation_url =
                        value.split_whitespace().next().unwrap_or_default().to_string();
                }
                "wfd_trigger_method" => trigger = Some(value.trim().to_string()),
                _ => {}
            }
        }
        out.push(Action::Send(response(200, cseq, "")));
        if trigger.as_deref() == Some("SETUP") {
            let c = self.cseq();
            let mut setup = request("SETUP", &self.presentation_url, c, "");
            setup.headers.push((
                "Transport".into(),
                format!(
                    "RTP/AVP/UDP;unicast;client_port={}",
                    self.caps.ports.rtp_port
                ),
            ));
            self.state = NegState::Ready;
            out.push(Action::Send(setup));
        }
        out
    }

    /// The chosen-format line repeats the capability layout with exactly one
    /// bit set in one table. Accept it only if it is a CEA mode we advertised.
    fn parse_chosen_video(value: &str) -> Option<VideoMode> {
        let f: Vec<&str> = value.split_whitespace().collect();
        if f.len() < 7 {
            return None;
        }
        let cea = u32::from_str_radix(f[4], 16).ok()?;
        let vesa = u32::from_str_radix(f[5], 16).ok()?;
        let hh = u32::from_str_radix(f[6], 16).ok()?;
        if vesa != 0 || hh != 0 || cea.count_ones() != 1 {
            return None;
        }
        cea_mode(cea.trailing_zeros())
    }

    fn on_response(&mut self, status: u16, m: &Message) -> Vec<Action> {
        if status != 200 {
            return vec![Action::Teardown("source refused a request")];
        }
        if self.state == NegState::Ready && self.peer_session.is_none() {
            // M6 answered: take the session id and send M7 PLAY.
            let session = m
                .header("session")
                .map(|s| s.split(';').next().unwrap_or(s).trim().to_string());
            let Some(session) = session else {
                return vec![Action::Teardown("SETUP response carried no Session")];
            };
            self.peer_session = Some(session.clone());
            let c = self.cseq();
            let mut play = request("PLAY", &self.presentation_url, c, "");
            play.headers.push(("Session".into(), session));
            self.state = NegState::Playing;
            return vec![Action::Send(play), Action::Play];
        }
        Vec::new()
    }

    /// Time-driven work: keep-alive out, and give up on a silent source.
    pub fn tick(&mut self, now: Instant) -> Vec<Action> {
        if matches!(self.state, NegState::Done) {
            return Vec::new();
        }
        let mut out = Vec::new();
        let since_heard = self.last_heard.map(|t| now.duration_since(t));
        if since_heard.is_some_and(|d| d > KEEPALIVE_TIMEOUT) {
            self.state = NegState::Done;
            out.push(Action::Teardown("no keep-alive reply for 60 s"));
            return out;
        }
        let due = self
            .last_keepalive
            .map(|t| now.duration_since(t) >= KEEPALIVE_EVERY)
            .unwrap_or_else(|| since_heard.is_some_and(|d| d >= KEEPALIVE_EVERY));
        if due {
            self.last_keepalive = Some(now);
            let c = self.cseq();
            let mut ka = request("GET_PARAMETER", &self.uri(), c, "");
            if let Some(s) = &self.peer_session {
                ka.headers.push(("Session".into(), s.clone()));
            }
            out.push(Action::Send(ka));
        }
        out
    }

    /// Asks the source for a fresh keyframe, at most once per 500 ms.
    pub fn request_idr(&mut self, now: Instant) -> Vec<Action> {
        if self
            .last_idr
            .is_some_and(|t| now.duration_since(t) < IDR_MIN_INTERVAL)
        {
            return Vec::new();
        }
        self.last_idr = Some(now);
        let c = self.cseq();
        let mut m = request("SET_PARAMETER", &self.uri(), c, "wfd_idr_request\r\n");
        if let Some(s) = &self.peer_session {
            m.headers.push(("Session".into(), s.clone()));
        }
        vec![Action::Send(m)]
    }

    fn uri(&self) -> String {
        if self.presentation_url.is_empty() {
            format!("rtsp://localhost/wfd1.0/{}", self.session_id)
        } else {
            self.presentation_url.clone()
        }
    }
}
```

- [ ] **Step 4: Run the tests**

Run: `cargo test -q -p castr-miracast`
Expected: the 10 negotiation tests pass alongside the earlier suites.

- [ ] **Step 5: Commit**

```bash
cargo fmt -p castr-miracast
git add crates/castr-miracast/src/rtsp.rs
git commit -m "feat(miracast): the M1-M7 negotiation state machine"
```

---

### Task 4: MPEG-TS demux

**Files:**
- Create: `crates/castr-miracast/src/ts.rs`
- Modify: `crates/castr-miracast/src/lib.rs` (add `pub mod ts;`, ungated: it is pure)

**Interfaces:**
- Produces: `ts::Demux::new() -> Demux`, `Demux::push(&mut self, packet: &[u8]) -> Vec<Unit>`, `enum Unit { Video { data: Vec<u8>, pts_us: Option<u64> }, Audio { data: Vec<u8>, pts_us: Option<u64> } }`, `Demux::stats(&self) -> DemuxStats { pub continuity_errors: u64, pub video_pid: Option<u16>, pub audio_pid: Option<u16> }`.

Add `pub mod ts;` to `lib.rs` in place of the Task 4 placeholder comment, ungated,
since the demux is pure and its tests should run on Windows too.

- [ ] **Step 1: Write the failing tests**

The fixtures are built by helper functions so the test reads as the structure of a transport stream rather than a wall of hex.

```rust
#[cfg(test)]
mod tests {
    use super::*;

    const VIDEO_PID: u16 = 0x1011;
    const AUDIO_PID: u16 = 0x1100;

    /// One 188-byte TS packet: sync byte, flags, PID, continuity, payload.
    fn ts_packet(pid: u16, start: bool, cc: u8, payload: &[u8]) -> Vec<u8> {
        let mut p = vec![0u8; 188];
        p[0] = 0x47;
        p[1] = ((start as u8) << 6) | ((pid >> 8) as u8 & 0x1f);
        p[2] = (pid & 0xff) as u8;
        p[3] = 0x10 | (cc & 0x0f); // payload only
        let n = payload.len().min(184);
        p[4..4 + n].copy_from_slice(&payload[..n]);
        p
    }

    /// PAT naming one program whose PMT lives on `pmt_pid`.
    fn pat(pmt_pid: u16) -> Vec<u8> {
        let mut section = vec![0x00, 0xb0, 0x0d, 0x00, 0x01, 0xc1, 0x00, 0x00];
        section.extend_from_slice(&[0x00, 0x01]); // program number 1
        section.extend_from_slice(&[(0xe0 | (pmt_pid >> 8) as u8), (pmt_pid & 0xff) as u8]);
        section.extend_from_slice(&[0, 0, 0, 0]); // CRC, unchecked by the demux
        let mut payload = vec![0x00]; // pointer field
        payload.extend_from_slice(&section);
        ts_packet(0x0000, true, 0, &payload)
    }

    /// PMT naming an H.264 video stream and an LPCM audio stream.
    fn pmt(pmt_pid: u16) -> Vec<u8> {
        let mut es = Vec::new();
        es.extend_from_slice(&[0x1b, (0xe0 | (VIDEO_PID >> 8) as u8), (VIDEO_PID & 0xff) as u8, 0xf0, 0x00]);
        es.extend_from_slice(&[0x83, (0xe0 | (AUDIO_PID >> 8) as u8), (AUDIO_PID & 0xff) as u8, 0xf0, 0x00]);
        let section_len = 9 + es.len() + 4;
        let mut section = vec![
            0x02,
            0xb0 | ((section_len >> 8) as u8 & 0x0f),
            (section_len & 0xff) as u8,
            0x00, 0x01, 0xc1, 0x00, 0x00,
            0xe0 | (VIDEO_PID >> 8) as u8, (VIDEO_PID & 0xff) as u8,
            0xf0, 0x00,
        ];
        section.extend_from_slice(&es);
        section.extend_from_slice(&[0, 0, 0, 0]);
        let mut payload = vec![0x00];
        payload.extend_from_slice(&section);
        ts_packet(pmt_pid, true, 0, &payload)
    }

    /// A PES header carrying `pts` (90 kHz) and `payload`.
    fn pes(stream_id: u8, pts_90k: Option<u64>, payload: &[u8]) -> Vec<u8> {
        let mut p = vec![0x00, 0x00, 0x01, stream_id];
        let (flags, hdr_len, pts_bytes) = match pts_90k {
            Some(pts) => {
                let mut b = Vec::new();
                b.push(0x21 | (((pts >> 30) & 0x07) as u8) << 1);
                b.push(((pts >> 22) & 0xff) as u8);
                b.push((((pts >> 15) & 0x7f) as u8) << 1 | 1);
                b.push(((pts >> 7) & 0xff) as u8);
                b.push(((pts & 0x7f) as u8) << 1 | 1);
                (0x80u8, 5u8, b)
            }
            None => (0x00, 0, Vec::new()),
        };
        let len = 3 + hdr_len as usize + payload.len();
        p.push((len >> 8) as u8);
        p.push((len & 0xff) as u8);
        p.push(0x80);
        p.push(flags);
        p.push(hdr_len);
        p.extend_from_slice(&pts_bytes);
        p.extend_from_slice(payload);
        p
    }

    fn feed(d: &mut Demux, packets: Vec<Vec<u8>>) -> Vec<Unit> {
        packets.into_iter().flat_map(|p| d.push(&p)).collect()
    }

    #[test]
    fn the_tables_teach_it_which_pids_carry_what() {
        let mut d = Demux::new();
        feed(&mut d, vec![pat(0x1000), pmt(0x1000)]);
        assert_eq!(d.stats().video_pid, Some(VIDEO_PID));
        assert_eq!(d.stats().audio_pid, Some(AUDIO_PID));
    }

    #[test]
    fn a_video_access_unit_comes_out_with_its_timestamp() {
        let mut d = Demux::new();
        let au = [0u8, 0, 0, 1, 0x65, 1, 2, 3];
        let mut packets = vec![pat(0x1000), pmt(0x1000)];
        packets.push(ts_packet(VIDEO_PID, true, 0, &pes(0xe0, Some(90_000), &au)));
        // A second PES start flushes the first.
        packets.push(ts_packet(VIDEO_PID, true, 1, &pes(0xe0, Some(93_000), &au)));
        let units = feed(&mut d, packets);
        match &units[0] {
            Unit::Video { data, pts_us } => {
                assert_eq!(data, &au);
                assert_eq!(*pts_us, Some(1_000_000), "90 kHz to microseconds");
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn a_unit_split_across_packets_is_reassembled() {
        let mut d = Demux::new();
        let big: Vec<u8> = (0..400u32).map(|i| (i % 251) as u8).collect();
        let payload = pes(0xe0, Some(0), &big);
        let mut packets = vec![pat(0x1000), pmt(0x1000)];
        packets.push(ts_packet(VIDEO_PID, true, 0, &payload[..184]));
        packets.push(ts_packet(VIDEO_PID, false, 1, &payload[184..368]));
        packets.push(ts_packet(VIDEO_PID, false, 2, &payload[368..]));
        packets.push(ts_packet(VIDEO_PID, true, 3, &pes(0xe0, Some(3000), &[9])));
        let units = feed(&mut d, packets);
        match &units[0] {
            Unit::Video { data, .. } => assert_eq!(data, &big),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn audio_is_separated_from_video() {
        let mut d = Demux::new();
        let mut packets = vec![pat(0x1000), pmt(0x1000)];
        packets.push(ts_packet(AUDIO_PID, true, 0, &pes(0xbd, Some(90_000), &[1, 2, 3, 4])));
        packets.push(ts_packet(AUDIO_PID, true, 1, &pes(0xbd, Some(91_000), &[5])));
        let units = feed(&mut d, packets);
        assert!(matches!(&units[0], Unit::Audio { data, .. } if data == &[1, 2, 3, 4]));
    }

    #[test]
    fn a_continuity_break_is_counted_and_drops_the_damaged_unit() {
        let mut d = Demux::new();
        let payload = pes(0xe0, Some(0), &[7u8; 300]);
        let mut packets = vec![pat(0x1000), pmt(0x1000)];
        packets.push(ts_packet(VIDEO_PID, true, 0, &payload[..184]));
        // cc jumps from 0 to 5: a packet was lost.
        packets.push(ts_packet(VIDEO_PID, false, 5, &payload[184..]));
        packets.push(ts_packet(VIDEO_PID, true, 6, &pes(0xe0, Some(3000), &[1])));
        let units = feed(&mut d, packets);
        assert_eq!(d.stats().continuity_errors, 1);
        assert!(
            !units.iter().any(|u| matches!(u, Unit::Video { data, .. } if data.len() > 200)),
            "the damaged unit is not emitted"
        );
    }

    #[test]
    fn packets_before_the_tables_are_ignored_without_panicking() {
        let mut d = Demux::new();
        let units = d.push(&ts_packet(VIDEO_PID, true, 0, &pes(0xe0, Some(0), &[1, 2, 3])));
        assert!(units.is_empty());
    }

    #[test]
    fn a_packet_with_no_sync_byte_is_rejected() {
        let mut d = Demux::new();
        let mut bad = ts_packet(VIDEO_PID, true, 0, &[1, 2, 3]);
        bad[0] = 0x00;
        assert!(d.push(&bad).is_empty());
    }

    #[test]
    fn an_adaptation_field_is_skipped() {
        let mut d = Demux::new();
        feed(&mut d, vec![pat(0x1000), pmt(0x1000)]);
        let inner = pes(0xe0, Some(0), &[4, 5, 6]);
        let mut p = vec![0u8; 188];
        p[0] = 0x47;
        p[1] = 0x40 | ((VIDEO_PID >> 8) as u8 & 0x1f);
        p[2] = (VIDEO_PID & 0xff) as u8;
        p[3] = 0x30; // adaptation field and payload
        p[4] = 7; // adaptation length
        p[12..12 + inner.len()].copy_from_slice(&inner);
        d.push(&p);
        let flush = d.push(&ts_packet(VIDEO_PID, true, 1, &pes(0xe0, Some(9000), &[1])));
        assert!(matches!(&flush[0], Unit::Video { data, .. } if data == &[4, 5, 6]));
    }
}
```

- [ ] **Step 2: Run to watch them fail**

Run: `cargo test -q -p castr-miracast ts::`
Expected: compile errors.

- [ ] **Step 3: Implement `ts.rs`**

```rust
// crates/castr-miracast/src/ts.rs
//! MPEG-TS demux: enough of the format to pull H.264 access units and LPCM
//! audio out of a Wi-Fi Display stream.
//!
//! Everything here is driven by whole 188-byte packets the caller supplies, so
//! it is testable with synthetic streams and has no I/O of its own. Sections
//! are read from the first packet that carries them; a PAT or PMT split across
//! packets is rare in a Miracast stream (both are far smaller than 184 bytes)
//! and is ignored rather than mis-parsed.

pub const PACKET_LEN: usize = 188;
const SYNC: u8 = 0x47;
/// Stream types we care about: H.264 video and LPCM audio.
const STREAM_TYPE_H264: u8 = 0x1b;
const STREAM_TYPE_LPCM: u8 = 0x83;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Unit {
    Video { data: Vec<u8>, pts_us: Option<u64> },
    Audio { data: Vec<u8>, pts_us: Option<u64> },
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct DemuxStats {
    pub continuity_errors: u64,
    pub video_pid: Option<u16>,
    pub audio_pid: Option<u16>,
}

#[derive(Default)]
struct Assembly {
    data: Vec<u8>,
    pts_us: Option<u64>,
    damaged: bool,
    open: bool,
}

#[derive(Default)]
pub struct Demux {
    pmt_pid: Option<u16>,
    video_pid: Option<u16>,
    audio_pid: Option<u16>,
    video: Assembly,
    audio: Assembly,
    last_cc: std::collections::HashMap<u16, u8>,
    continuity_errors: u64,
}

impl Demux {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn stats(&self) -> DemuxStats {
        DemuxStats {
            continuity_errors: self.continuity_errors,
            video_pid: self.video_pid,
            audio_pid: self.audio_pid,
        }
    }

    /// Feeds one transport packet. Returns any access units it completed.
    pub fn push(&mut self, packet: &[u8]) -> Vec<Unit> {
        if packet.len() != PACKET_LEN || packet[0] != SYNC {
            return Vec::new();
        }
        let pid = (((packet[1] & 0x1f) as u16) << 8) | packet[2] as u16;
        let start = packet[1] & 0x40 != 0;
        let cc = packet[3] & 0x0f;
        let has_payload = packet[3] & 0x10 != 0;
        let has_adaptation = packet[3] & 0x20 != 0;
        if !has_payload {
            return Vec::new();
        }
        let mut off = 4;
        if has_adaptation {
            let len = packet[4] as usize;
            off = 5 + len;
            if off >= PACKET_LEN {
                return Vec::new();
            }
        }
        let payload = &packet[off..];

        // Continuity: the counter increments per packet on a PID. A jump means
        // a packet was lost, so whatever is being assembled is damaged.
        let expected = self.last_cc.get(&pid).map(|c| (c + 1) & 0x0f);
        self.last_cc.insert(pid, cc);
        if let Some(exp) = expected {
            if cc != exp {
                self.continuity_errors += 1;
                if Some(pid) == self.video_pid {
                    self.video.damaged = true;
                } else if Some(pid) == self.audio_pid {
                    self.audio.damaged = true;
                }
            }
        }

        if pid == 0 {
            self.read_pat(payload, start);
            return Vec::new();
        }
        if Some(pid) == self.pmt_pid {
            self.read_pmt(payload, start);
            return Vec::new();
        }
        let is_video = Some(pid) == self.video_pid;
        let is_audio = Some(pid) == self.audio_pid;
        if !is_video && !is_audio {
            return Vec::new();
        }
        let mut out = Vec::new();
        let asm = if is_video { &mut self.video } else { &mut self.audio };
        if start {
            // A new PES starts: emit whatever came before, if it is intact.
            if asm.open && !asm.damaged && !asm.data.is_empty() {
                let data = std::mem::take(&mut asm.data);
                let pts = asm.pts_us;
                out.push(if is_video {
                    Unit::Video { data, pts_us: pts }
                } else {
                    Unit::Audio { data, pts_us: pts }
                });
            } else {
                asm.data.clear();
            }
            asm.damaged = false;
            asm.open = true;
            match parse_pes(payload) {
                Some((pts_us, body)) => {
                    asm.pts_us = pts_us;
                    asm.data.extend_from_slice(body);
                }
                None => {
                    asm.open = false;
                    asm.damaged = true;
                }
            }
        } else if asm.open {
            asm.data.extend_from_slice(payload);
        }
        out
    }

    fn read_pat(&mut self, payload: &[u8], start: bool) {
        if !start || payload.is_empty() {
            return;
        }
        let ptr = payload[0] as usize;
        let s = &payload[1 + ptr..];
        // table_id(1) length(2) tsid(2) ver(1) sec(1) last(1) then entries.
        if s.len() < 12 || s[0] != 0x00 {
            return;
        }
        let program = &s[8..12];
        let pid = (((program[2] & 0x1f) as u16) << 8) | program[3] as u16;
        if pid != 0 {
            self.pmt_pid = Some(pid);
        }
    }

    fn read_pmt(&mut self, payload: &[u8], start: bool) {
        if !start || payload.is_empty() {
            return;
        }
        let ptr = payload[0] as usize;
        let s = &payload[1 + ptr..];
        if s.len() < 12 || s[0] != 0x02 {
            return;
        }
        let section_len = ((((s[1] & 0x0f) as usize) << 8) | s[2] as usize) + 3;
        if s.len() < section_len.min(s.len()) || section_len < 16 {
            return;
        }
        let info_len = (((s[10] & 0x0f) as usize) << 8) | s[11] as usize;
        let mut i = 12 + info_len;
        let end = section_len.saturating_sub(4).min(s.len());
        while i + 5 <= end {
            let stream_type = s[i];
            let pid = (((s[i + 1] & 0x1f) as u16) << 8) | s[i + 2] as u16;
            let es_info = (((s[i + 3] & 0x0f) as usize) << 8) | s[i + 4] as usize;
            match stream_type {
                STREAM_TYPE_H264 => self.video_pid = Some(pid),
                STREAM_TYPE_LPCM => self.audio_pid = Some(pid),
                _ => {}
            }
            i += 5 + es_info;
        }
    }
}

/// Returns the presentation timestamp in microseconds and the payload after
/// the PES header.
fn parse_pes(p: &[u8]) -> Option<(Option<u64>, &[u8])> {
    if p.len() < 9 || p[0] != 0 || p[1] != 0 || p[2] != 1 {
        return None;
    }
    let flags = p[7];
    let header_len = p[8] as usize;
    let body_start = 9 + header_len;
    if p.len() < body_start {
        return None;
    }
    let pts = if flags & 0x80 != 0 && header_len >= 5 {
        let b = &p[9..14];
        let v = (((b[0] as u64) >> 1) & 0x07) << 30
            | (b[1] as u64) << 22
            | (((b[2] as u64) >> 1) & 0x7f) << 15
            | (b[3] as u64) << 7
            | ((b[4] as u64) >> 1);
        // 90 kHz ticks to microseconds.
        Some(v * 1_000_000 / 90_000)
    } else {
        None
    };
    Some((pts, &p[body_start..]))
}
```

- [ ] **Step 4: Run the tests**

Run: `cargo test -q -p castr-miracast ts::`
Expected: 8 passed.

- [ ] **Step 5: Commit**

```bash
cargo fmt -p castr-miracast
git add crates/castr-miracast/src/ts.rs crates/castr-miracast/src/lib.rs
git commit -m "feat(miracast): MPEG-TS demux with PAT, PMT and PES assembly"
```

---

### Task 5: RTP receive and reordering

**Files:**
- Create: `crates/castr-miracast/src/rtp.rs`
- Modify: `crates/castr-miracast/src/lib.rs` (replace the Task 5 placeholder with `pub mod rtp;`, ungated)

**Interfaces:**
- Produces: `rtp::Packet { pub sequence: u16, pub timestamp: u32, pub payload_type: u8, pub payload: Vec<u8> }`, `rtp::parse(&[u8]) -> Option<Packet>`, `rtp::Reorder::new(window: usize) -> Reorder`, `Reorder::push(&mut self, Packet) -> Vec<Packet>`, `Reorder::flush(&mut self) -> Vec<Packet>`, `Reorder::lost(&self) -> u64`.

- [ ] **Step 1: Write the failing tests**

```rust
#[cfg(test)]
mod tests {
    use super::*;

    fn raw(seq: u16, ts: u32, pt: u8, payload: &[u8]) -> Vec<u8> {
        let mut v = vec![0x80, pt];
        v.extend_from_slice(&seq.to_be_bytes());
        v.extend_from_slice(&ts.to_be_bytes());
        v.extend_from_slice(&0x1234_5678u32.to_be_bytes()); // SSRC
        v.extend_from_slice(payload);
        v
    }

    fn pkt(seq: u16) -> Packet {
        Packet { sequence: seq, timestamp: seq as u32 * 100, payload_type: 33, payload: vec![seq as u8] }
    }

    #[test]
    fn a_packet_parses_into_its_fields() {
        let p = parse(&raw(7, 900, 33, &[1, 2, 3])).expect("parse");
        assert_eq!(p.sequence, 7);
        assert_eq!(p.timestamp, 900);
        assert_eq!(p.payload_type, 33);
        assert_eq!(p.payload, vec![1, 2, 3]);
    }

    #[test]
    fn a_csrc_count_shifts_the_payload() {
        let mut v = raw(1, 0, 33, &[9, 9]);
        v[0] = 0x81; // one CSRC
        v.splice(12..12, [0, 0, 0, 5]);
        let p = parse(&v).expect("parse");
        assert_eq!(p.payload, vec![9, 9]);
    }

    #[test]
    fn a_short_or_wrong_version_packet_is_rejected() {
        assert!(parse(&[0x80, 33, 0]).is_none());
        let mut v = raw(1, 0, 33, &[1]);
        v[0] = 0x40; // version 1
        assert!(parse(&v).is_none());
    }

    #[test]
    fn packets_in_order_pass_straight_through() {
        let mut r = Reorder::new(8);
        let out: Vec<u16> = (1..=4).flat_map(|s| r.push(pkt(s))).map(|p| p.sequence).collect();
        assert_eq!(out, vec![1, 2, 3, 4]);
        assert_eq!(r.lost(), 0);
    }

    #[test]
    fn a_swapped_pair_is_reordered() {
        let mut r = Reorder::new(8);
        let mut got: Vec<u16> = r.push(pkt(1)).into_iter().map(|p| p.sequence).collect();
        got.extend(r.push(pkt(3)).into_iter().map(|p| p.sequence));
        got.extend(r.push(pkt(2)).into_iter().map(|p| p.sequence));
        got.extend(r.flush().into_iter().map(|p| p.sequence));
        assert_eq!(got, vec![1, 2, 3]);
        assert_eq!(r.lost(), 0);
    }

    #[test]
    fn a_gap_wider_than_the_window_is_given_up_on_and_counted() {
        let mut r = Reorder::new(4);
        r.push(pkt(1));
        let mut got = Vec::new();
        for s in 3..=8 {
            got.extend(r.push(pkt(s)).into_iter().map(|p| p.sequence));
        }
        assert!(got.contains(&3), "later packets are released: {got:?}");
        assert_eq!(r.lost(), 1, "packet 2 is counted lost");
    }

    #[test]
    fn a_duplicate_is_dropped() {
        let mut r = Reorder::new(8);
        r.push(pkt(1));
        r.push(pkt(2));
        assert!(r.push(pkt(2)).is_empty());
        assert_eq!(r.flush().len(), 0);
    }

    #[test]
    fn the_sequence_number_wraps_without_declaring_loss() {
        let mut r = Reorder::new(8);
        let mut got = Vec::new();
        for s in [65534u16, 65535, 0, 1] {
            got.extend(r.push(pkt(s)).into_iter().map(|p| p.sequence));
        }
        got.extend(r.flush().into_iter().map(|p| p.sequence));
        assert_eq!(got, vec![65534, 65535, 0, 1]);
        assert_eq!(r.lost(), 0);
    }
}
```

- [ ] **Step 2: Run to watch them fail**

Run: `cargo test -q -p castr-miracast rtp::`
Expected: compile errors.

- [ ] **Step 3: Implement `rtp.rs`**

```rust
// crates/castr-miracast/src/rtp.rs
//! RTP parsing and a small reordering window.
//!
//! Wi-Fi Display carries MPEG-TS in RTP payload type 33. The network can
//! deliver packets out of order, so a short window is held before handing them
//! on; anything that never arrives inside the window is counted as lost, which
//! is what drives the sink's keyframe requests.

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Packet {
    pub sequence: u16,
    pub timestamp: u32,
    pub payload_type: u8,
    pub payload: Vec<u8>,
}

pub fn parse(buf: &[u8]) -> Option<Packet> {
    if buf.len() < 12 || buf[0] >> 6 != 2 {
        return None;
    }
    let csrc = (buf[0] & 0x0f) as usize;
    let extension = buf[0] & 0x10 != 0;
    let mut off = 12 + csrc * 4;
    if extension {
        if buf.len() < off + 4 {
            return None;
        }
        let words = u16::from_be_bytes([buf[off + 2], buf[off + 3]]) as usize;
        off += 4 + words * 4;
    }
    if buf.len() < off {
        return None;
    }
    Some(Packet {
        sequence: u16::from_be_bytes([buf[2], buf[3]]),
        timestamp: u32::from_be_bytes([buf[4], buf[5], buf[6], buf[7]]),
        payload_type: buf[1] & 0x7f,
        payload: buf[off..].to_vec(),
    })
}

/// True when `a` is at or after `b` in sequence-number space, tolerating wrap.
fn seq_ge(a: u16, b: u16) -> bool {
    a.wrapping_sub(b) < 0x8000
}

pub struct Reorder {
    window: usize,
    held: Vec<Packet>,
    next: Option<u16>,
    lost: u64,
}

impl Reorder {
    pub fn new(window: usize) -> Self {
        Self { window, held: Vec::new(), next: None, lost: 0 }
    }

    pub fn lost(&self) -> u64 {
        self.lost
    }

    pub fn push(&mut self, p: Packet) -> Vec<Packet> {
        let seq = p.sequence;
        if let Some(next) = self.next {
            // Already delivered, or a duplicate of something held.
            if !seq_ge(seq, next) || self.held.iter().any(|h| h.sequence == seq) {
                return Vec::new();
            }
        }
        let pos = self
            .held
            .iter()
            .position(|h| seq_ge(h.sequence, seq))
            .unwrap_or(self.held.len());
        self.held.insert(pos, p);
        let mut out = Vec::new();
        loop {
            // Release the head while it is the packet we are waiting for.
            let Some(head) = self.held.first() else { break };
            match self.next {
                None => {
                    self.next = Some(head.sequence);
                }
                Some(next) if head.sequence == next => {}
                Some(_) if self.held.len() > self.window => {
                    // Give up on the missing one and resync to what we have.
                    let missing = head.sequence.wrapping_sub(self.next.unwrap_or(head.sequence));
                    self.lost += missing.max(1) as u64;
                    self.next = Some(head.sequence);
                }
                Some(_) => break,
            }
            let head = self.held.remove(0);
            self.next = Some(head.sequence.wrapping_add(1));
            out.push(head);
        }
        out
    }

    /// Releases everything still held, in order, at end of stream.
    pub fn flush(&mut self) -> Vec<Packet> {
        let mut out = std::mem::take(&mut self.held);
        out.sort_by(|a, b| {
            if seq_ge(b.sequence, a.sequence) {
                std::cmp::Ordering::Less
            } else {
                std::cmp::Ordering::Greater
            }
        });
        if let Some(last) = out.last() {
            self.next = Some(last.sequence.wrapping_add(1));
        }
        out
    }
}
```

- [ ] **Step 4: Run the tests**

Run: `cargo test -q -p castr-miracast rtp::`
Expected: 8 passed.

- [ ] **Step 5: Commit**

```bash
cargo fmt -p castr-miracast
git add crates/castr-miracast/src/rtp.rs crates/castr-miracast/src/lib.rs
git commit -m "feat(miracast): RTP parsing with a reordering window"
```

---

### Task 6: DHCP responder

**Files:**
- Create: `crates/castr-miracast/src/dhcp.rs`
- Modify: `crates/castr-miracast/src/lib.rs` (replace the Task 6 placeholder with `pub mod dhcp;`, ungated)

**Interfaces:**
- Produces: `dhcp::Request { pub kind: Kind, pub xid: u32, pub mac: [u8; 6], pub requested_ip: Option<Ipv4Addr> }`, `enum Kind { Discover, Request, Other }`, `dhcp::parse(&[u8]) -> Option<Request>`, `dhcp::Lease { pub server: Ipv4Addr, pub client: Ipv4Addr, pub netmask: Ipv4Addr, pub lease_secs: u32 }`, `dhcp::reply(&Request, &Lease) -> Option<Vec<u8>>`, `dhcp::DEFAULT_LEASE`.

- [ ] **Step 1: Write the failing tests**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use std::net::Ipv4Addr;

    const MAC: [u8; 6] = [0xaa, 0xbb, 0xcc, 0xdd, 0xee, 0xff];

    fn packet(msg_type: u8, xid: u32, requested: Option<[u8; 4]>) -> Vec<u8> {
        let mut p = vec![0u8; 240];
        p[0] = 1; // BOOTREQUEST
        p[1] = 1; // ethernet
        p[2] = 6; // hardware length
        p[4..8].copy_from_slice(&xid.to_be_bytes());
        p[28..34].copy_from_slice(&MAC);
        p[236..240].copy_from_slice(&[99, 130, 83, 99]); // magic cookie
        p.extend_from_slice(&[53, 1, msg_type]);
        if let Some(ip) = requested {
            p.push(50);
            p.push(4);
            p.extend_from_slice(&ip);
        }
        p.push(255);
        p
    }

    #[test]
    fn a_discover_parses_with_its_transaction_and_mac() {
        let r = parse(&packet(1, 0xdeadbeef, None)).expect("parse");
        assert_eq!(r.kind, Kind::Discover);
        assert_eq!(r.xid, 0xdeadbeef);
        assert_eq!(r.mac, MAC);
        assert_eq!(r.requested_ip, None);
    }

    #[test]
    fn a_request_carries_the_address_it_wants() {
        let r = parse(&packet(3, 1, Some([192, 168, 173, 2]))).expect("parse");
        assert_eq!(r.kind, Kind::Request);
        assert_eq!(r.requested_ip, Some(Ipv4Addr::new(192, 168, 173, 2)));
    }

    #[test]
    fn a_packet_without_the_magic_cookie_is_rejected() {
        let mut p = packet(1, 1, None);
        p[236] = 0;
        assert!(parse(&p).is_none());
    }

    #[test]
    fn a_truncated_packet_is_rejected_without_panicking() {
        assert!(parse(&[1, 1, 6, 0]).is_none());
        let p = packet(1, 1, None);
        assert!(parse(&p[..100]).is_none());
    }

    #[test]
    fn a_discover_is_answered_with_an_offer_naming_our_addresses() {
        let r = parse(&packet(1, 7, None)).unwrap();
        let out = reply(&r, &DEFAULT_LEASE).expect("offer");
        assert_eq!(out[0], 2, "BOOTREPLY");
        assert_eq!(&out[4..8], &7u32.to_be_bytes(), "same transaction");
        assert_eq!(&out[16..20], &[192, 168, 173, 2], "your-address");
        assert_eq!(&out[28..34], &MAC);
        assert!(has_option(&out, 53, &[2]), "OFFER");
        assert!(has_option(&out, 54, &[192, 168, 173, 1]), "server identifier");
        assert!(has_option(&out, 1, &[255, 255, 255, 248]), "/29 netmask");
        assert!(has_option(&out, 3, &[192, 168, 173, 1]), "router");
    }

    #[test]
    fn a_request_is_answered_with_an_ack() {
        let r = parse(&packet(3, 8, Some([192, 168, 173, 2]))).unwrap();
        let out = reply(&r, &DEFAULT_LEASE).expect("ack");
        assert!(has_option(&out, 53, &[5]), "ACK");
        assert!(has_option(&out, 51, &3600u32.to_be_bytes()), "lease time");
    }

    #[test]
    fn a_request_for_someone_elses_address_is_declined() {
        let r = parse(&packet(3, 9, Some([10, 0, 0, 5]))).unwrap();
        let out = reply(&r, &DEFAULT_LEASE).expect("nak");
        assert!(has_option(&out, 53, &[6]), "NAK");
    }

    #[test]
    fn any_other_message_type_is_ignored() {
        let r = parse(&packet(7, 10, None)).unwrap(); // RELEASE
        assert_eq!(r.kind, Kind::Other);
        assert!(reply(&r, &DEFAULT_LEASE).is_none());
    }

    /// Finds a DHCP option in a reply and compares its value.
    fn has_option(buf: &[u8], code: u8, value: &[u8]) -> bool {
        let mut i = 240;
        while i + 2 <= buf.len() {
            let c = buf[i];
            if c == 255 {
                return false;
            }
            let len = buf[i + 1] as usize;
            if c == code {
                return &buf[i + 2..i + 2 + len] == value;
            }
            i += 2 + len;
        }
        false
    }
}
```

- [ ] **Step 2: Run to watch them fail**

Run: `cargo test -q -p castr-miracast dhcp::`
Expected: compile errors.

- [ ] **Step 3: Implement `dhcp.rs`**

```rust
// crates/castr-miracast/src/dhcp.rs
//! A DHCP server with exactly one address to give away.
//!
//! Miracast leaves addressing to DHCP, and the Pi has no DHCP server
//! installed, so the sink answers on the group interface itself. The range is
//! deliberately far from the Pi's own LAN so the source cannot confuse the two
//! default routes.

use std::net::Ipv4Addr;

const MAGIC: [u8; 4] = [99, 130, 83, 99];
const OPT_SUBNET: u8 = 1;
const OPT_ROUTER: u8 = 3;
const OPT_LEASE_TIME: u8 = 51;
const OPT_MESSAGE_TYPE: u8 = 53;
const OPT_SERVER_ID: u8 = 54;
const OPT_REQUESTED_IP: u8 = 50;
const OPT_END: u8 = 255;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Discover,
    Request,
    Other,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Request {
    pub kind: Kind,
    pub xid: u32,
    pub mac: [u8; 6],
    pub requested_ip: Option<Ipv4Addr>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Lease {
    pub server: Ipv4Addr,
    pub client: Ipv4Addr,
    pub netmask: Ipv4Addr,
    pub lease_secs: u32,
}

/// The single lease this sink hands out: a /29 far from the usual home ranges.
pub const DEFAULT_LEASE: Lease = Lease {
    server: Ipv4Addr::new(192, 168, 173, 1),
    client: Ipv4Addr::new(192, 168, 173, 2),
    netmask: Ipv4Addr::new(255, 255, 255, 248),
    lease_secs: 3600,
};

pub fn parse(buf: &[u8]) -> Option<Request> {
    if buf.len() < 240 || buf[0] != 1 || buf[236..240] != MAGIC {
        return None;
    }
    let xid = u32::from_be_bytes([buf[4], buf[5], buf[6], buf[7]]);
    let mut mac = [0u8; 6];
    mac.copy_from_slice(&buf[28..34]);
    let mut kind = Kind::Other;
    let mut requested_ip = None;
    let mut i = 240;
    while i + 2 <= buf.len() {
        let code = buf[i];
        if code == OPT_END {
            break;
        }
        if code == 0 {
            i += 1;
            continue;
        }
        let len = buf[i + 1] as usize;
        let value = buf.get(i + 2..i + 2 + len)?;
        match code {
            OPT_MESSAGE_TYPE if len == 1 => {
                kind = match value[0] {
                    1 => Kind::Discover,
                    3 => Kind::Request,
                    _ => Kind::Other,
                }
            }
            OPT_REQUESTED_IP if len == 4 => {
                requested_ip = Some(Ipv4Addr::new(value[0], value[1], value[2], value[3]))
            }
            _ => {}
        }
        i += 2 + len;
    }
    Some(Request { kind, xid, mac, requested_ip })
}

/// Builds the reply for a request, or `None` when there is nothing to say.
pub fn reply(r: &Request, lease: &Lease) -> Option<Vec<u8>> {
    let message_type = match r.kind {
        Kind::Discover => 2, // OFFER
        Kind::Request => {
            match r.requested_ip {
                // Asking for what we offered, or for nothing in particular.
                None => 5,
                Some(ip) if ip == lease.client => 5, // ACK
                Some(_) => 6,                        // NAK
            }
        }
        Kind::Other => return None,
    };
    let mut p = vec![0u8; 240];
    p[0] = 2; // BOOTREPLY
    p[1] = 1;
    p[2] = 6;
    p[4..8].copy_from_slice(&r.xid.to_be_bytes());
    if message_type != 6 {
        p[16..20].copy_from_slice(&lease.client.octets());
    }
    p[20..24].copy_from_slice(&lease.server.octets());
    p[28..34].copy_from_slice(&r.mac);
    p[236..240].copy_from_slice(&MAGIC);
    push_option(&mut p, OPT_MESSAGE_TYPE, &[message_type]);
    push_option(&mut p, OPT_SERVER_ID, &lease.server.octets());
    if message_type != 6 {
        push_option(&mut p, OPT_SUBNET, &lease.netmask.octets());
        push_option(&mut p, OPT_ROUTER, &lease.server.octets());
        push_option(&mut p, OPT_LEASE_TIME, &lease.lease_secs.to_be_bytes());
    }
    p.push(OPT_END);
    Some(p)
}

fn push_option(p: &mut Vec<u8>, code: u8, value: &[u8]) {
    p.push(code);
    p.push(value.len() as u8);
    p.extend_from_slice(value);
}
```

- [ ] **Step 4: Run the tests**

Run: `cargo test -q -p castr-miracast dhcp::`
Expected: 8 passed. Then `cargo test -q --workspace` on Windows: all five pure modules' tests run there.

- [ ] **Step 5: Commit**

```bash
cargo fmt -p castr-miracast
git add crates/castr-miracast/src/dhcp.rs crates/castr-miracast/src/lib.rs
git commit -m "feat(miracast): a one-address DHCP responder for the group interface"
```

---

### Task 7: The wpa_supplicant control channel

**Files:**
- Create: `crates/castr-miracast/src/p2p.rs`
- Modify: `crates/castr-miracast/src/lib.rs`

**Interfaces:**
- Produces: `p2p::Command` (a pure builder: `Command::wifi_display_enable() -> String`, `Command::subelement(index: u8, hex: &str) -> String`, `Command::group_add_persistent(freq_mhz: u32) -> String`, `Command::wps_pin(pin: &str) -> String`, `Command::group_remove(iface: &str) -> String`), `p2p::Event`, `p2p::parse_event(&str) -> Option<Event>`, and the Linux-only `p2p::Control::open(ctrl_dir: &Path, iface: &str) -> io::Result<Control>` with `Control::request(&mut self, cmd: &str) -> io::Result<String>`, `Control::attach(&mut self) -> io::Result<()>`, `Control::poll_event(&mut self, timeout: Duration) -> io::Result<Option<Event>>`.

The command strings and the event parser are pure and tested everywhere; only `Control` is Linux-gated.

- [ ] **Step 1: Write the failing tests**

Event fixtures are the real unsolicited messages `wpa_supplicant` 2.10 emits.

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_command_strings_match_the_control_interface() {
        assert_eq!(Command::wifi_display_enable(), "SET wifi_display 1");
        assert_eq!(
            Command::subelement(0, "00060011 1c44 000a"),
            "WFD_SUBELEM_SET 0 00060011 1c44 000a"
        );
        assert_eq!(
            Command::group_add_persistent(2437),
            "P2P_GROUP_ADD persistent freq=2437"
        );
        assert_eq!(Command::wps_pin("12345670"), "WPS_PIN any 12345670");
        assert_eq!(Command::group_remove("p2p-wlan0-0"), "P2P_GROUP_REMOVE p2p-wlan0-0");
    }

    #[test]
    fn a_group_started_event_yields_the_interface_and_role() {
        let e = parse_event(
            "<3>P2P-GROUP-STARTED p2p-wlan0-0 GO ssid=\"DIRECT-xy\" freq=2437 passphrase=\"secret\" go_dev_addr=02:11:22:33:44:55",
        )
        .expect("event");
        assert_eq!(
            e,
            Event::GroupStarted { interface: "p2p-wlan0-0".into(), go: true, freq_mhz: 2437 }
        );
    }

    #[test]
    fn a_client_joining_yields_its_address() {
        let e = parse_event("<3>AP-STA-CONNECTED 02:aa:bb:cc:dd:ee p2p_dev_addr=02:11:22:33:44:55")
            .expect("event");
        assert_eq!(e, Event::ClientConnected { mac: "02:aa:bb:cc:dd:ee".into() });
    }

    #[test]
    fn a_client_leaving_is_recognised() {
        let e = parse_event("<3>AP-STA-DISCONNECTED 02:aa:bb:cc:dd:ee").expect("event");
        assert_eq!(e, Event::ClientDisconnected { mac: "02:aa:bb:cc:dd:ee".into() });
    }

    #[test]
    fn a_provision_discovery_request_asks_us_for_a_pin() {
        let e = parse_event("<3>P2P-PROV-DISC-PBC-REQ 02:11:22:33:44:55 p2p_dev_addr=02:11:22:33:44:55 name='PC'")
            .expect("event");
        assert_eq!(e, Event::ProvisionRequest { peer: "02:11:22:33:44:55".into() });
    }

    #[test]
    fn group_removal_and_wps_success_are_recognised() {
        assert_eq!(
            parse_event("<3>P2P-GROUP-REMOVED p2p-wlan0-0 GO reason=REQUESTED"),
            Some(Event::GroupRemoved { interface: "p2p-wlan0-0".into() })
        );
        assert_eq!(
            parse_event("<3>WPS-SUCCESS"),
            Some(Event::WpsSuccess)
        );
        assert_eq!(parse_event("<3>WPS-FAIL msg=8 config_error=0"), Some(Event::WpsFail));
    }

    #[test]
    fn an_unknown_event_is_none_not_a_panic() {
        assert!(parse_event("<3>CTRL-EVENT-SCAN-STARTED ").is_none());
        assert!(parse_event("").is_none());
        assert!(parse_event("<3>").is_none());
    }

    #[test]
    fn a_priority_prefix_is_optional() {
        assert!(parse_event("AP-STA-DISCONNECTED 02:aa:bb:cc:dd:ee").is_some());
    }
}
```

- [ ] **Step 2: Run to watch them fail**

Run: `cargo test -q -p castr-miracast p2p::`
Expected: compile errors.

- [ ] **Step 3: Implement `p2p.rs`**

```rust
// crates/castr-miracast/src/p2p.rs
//! The wpa_supplicant control channel.
//!
//! `wpa_supplicant` speaks a line protocol over a Unix datagram socket: send a
//! command, read one reply; after `ATTACH`, unsolicited events arrive on the
//! same socket. The command builders and the event parser are pure so they are
//! tested everywhere; only `Control`, which owns the socket, is Linux-only.

/// Commands we send. Built as strings so they can be asserted in tests and
/// logged verbatim when something goes wrong on the hardware.
pub struct Command;

impl Command {
    pub fn wifi_display_enable() -> String {
        "SET wifi_display 1".into()
    }
    /// Sets one WFD information-element subelement; index 0 is device info.
    pub fn subelement(index: u8, hex: &str) -> String {
        format!("WFD_SUBELEM_SET {index} {hex}")
    }
    /// Creates a persistent group with us as group owner on a fixed channel.
    pub fn group_add_persistent(freq_mhz: u32) -> String {
        format!("P2P_GROUP_ADD persistent freq={freq_mhz}")
    }
    /// Authorises an enrolment with the PIN we display.
    pub fn wps_pin(pin: &str) -> String {
        format!("WPS_PIN any {pin}")
    }
    pub fn group_remove(iface: &str) -> String {
        format!("P2P_GROUP_REMOVE {iface}")
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    GroupStarted { interface: String, go: bool, freq_mhz: u32 },
    GroupRemoved { interface: String },
    ClientConnected { mac: String },
    ClientDisconnected { mac: String },
    ProvisionRequest { peer: String },
    WpsSuccess,
    WpsFail,
}

/// Parses one unsolicited event line. Unknown events yield `None` rather than
/// an error: the supplicant emits many we do not care about.
pub fn parse_event(line: &str) -> Option<Event> {
    // Strip the "<3>" priority prefix if present.
    let body = match line.split_once('>') {
        Some((p, rest)) if p.starts_with('<') => rest,
        _ => line,
    };
    let mut parts = body.split_whitespace();
    let name = parts.next()?;
    match name {
        "P2P-GROUP-STARTED" => {
            let interface = parts.next()?.to_string();
            let role = parts.next().unwrap_or("");
            let freq_mhz = body
                .split_whitespace()
                .find_map(|t| t.strip_prefix("freq="))
                .and_then(|v| v.parse().ok())
                .unwrap_or(0);
            Some(Event::GroupStarted { interface, go: role == "GO", freq_mhz })
        }
        "P2P-GROUP-REMOVED" => Some(Event::GroupRemoved { interface: parts.next()?.to_string() }),
        "AP-STA-CONNECTED" => Some(Event::ClientConnected { mac: parts.next()?.to_string() }),
        "AP-STA-DISCONNECTED" => Some(Event::ClientDisconnected { mac: parts.next()?.to_string() }),
        "P2P-PROV-DISC-PBC-REQ" | "P2P-PROV-DISC-SHOW-PIN" | "P2P-PROV-DISC-ENTER-PIN" => {
            Some(Event::ProvisionRequest { peer: parts.next()?.to_string() })
        }
        "WPS-SUCCESS" => Some(Event::WpsSuccess),
        "WPS-FAIL" | "WPS-TIMEOUT" => Some(Event::WpsFail),
        _ => None,
    }
}

#[cfg(target_os = "linux")]
mod control {
    use super::{parse_event, Event};
    use std::io;
    use std::os::fd::{AsRawFd, OwnedFd};
    use std::path::Path;
    use std::time::{Duration, Instant};

    /// A connected control socket. Commands and events share it, so a reply
    /// read may turn up an event first; `request` skips events and hands them
    /// to the caller on the next `poll_event`.
    pub struct Control {
        fd: OwnedFd,
        pending: Vec<Event>,
    }

    impl Control {
        /// Connects to `<ctrl_dir>/<iface>`, binding our own abstract socket.
        pub fn open(ctrl_dir: &Path, iface: &str) -> io::Result<Self> {
            let path = ctrl_dir.join(iface);
            // SAFETY: creating a Unix datagram socket with the standard flags.
            let raw = unsafe { libc::socket(libc::AF_UNIX, libc::SOCK_DGRAM | libc::SOCK_CLOEXEC, 0) };
            if raw < 0 {
                return Err(io::Error::last_os_error());
            }
            // SAFETY: `raw` is a fresh fd we own from here on.
            let fd = unsafe { <OwnedFd as std::os::fd::FromRawFd>::from_raw_fd(raw) };
            let local = format!("\0castr-miracast-{}", std::process::id());
            bind_unix(&fd, local.as_bytes())?;
            connect_unix(&fd, path.as_os_str())?;
            Ok(Self { fd, pending: Vec::new() })
        }

        /// Sends a command and returns its reply, skipping any event lines
        /// that arrive first (they are queued for `poll_event`).
        pub fn request(&mut self, cmd: &str) -> io::Result<String> {
            send(&self.fd, cmd.as_bytes())?;
            let deadline = Instant::now() + Duration::from_secs(5);
            loop {
                let Some(line) = recv_line(&self.fd, deadline)? else {
                    return Err(io::Error::new(io::ErrorKind::TimedOut, format!("{cmd}: no reply")));
                };
                if line.starts_with('<') {
                    if let Some(e) = parse_event(&line) {
                        self.pending.push(e);
                    }
                    continue;
                }
                return Ok(line);
            }
        }

        /// Subscribes to unsolicited events.
        pub fn attach(&mut self) -> io::Result<()> {
            let reply = self.request("ATTACH")?;
            if reply.trim() == "OK" {
                Ok(())
            } else {
                Err(io::Error::other(format!("ATTACH: {reply}")))
            }
        }

        pub fn poll_event(&mut self, timeout: Duration) -> io::Result<Option<Event>> {
            if !self.pending.is_empty() {
                return Ok(Some(self.pending.remove(0)));
            }
            let deadline = Instant::now() + timeout;
            while let Some(line) = recv_line(&self.fd, deadline)? {
                if let Some(e) = parse_event(&line) {
                    return Ok(Some(e));
                }
            }
            Ok(None)
        }
    }

    fn sockaddr_un(path: &[u8]) -> io::Result<(libc::sockaddr_un, libc::socklen_t)> {
        let mut addr: libc::sockaddr_un = unsafe { std::mem::zeroed() };
        addr.sun_family = libc::AF_UNIX as libc::sa_family_t;
        if path.len() >= addr.sun_path.len() {
            return Err(io::Error::other("control socket path too long"));
        }
        for (i, b) in path.iter().enumerate() {
            addr.sun_path[i] = *b as libc::c_char;
        }
        let len = (std::mem::size_of::<libc::sa_family_t>() + path.len()) as libc::socklen_t;
        Ok((addr, len))
    }

    fn bind_unix(fd: &OwnedFd, path: &[u8]) -> io::Result<()> {
        let (addr, len) = sockaddr_un(path)?;
        // SAFETY: `addr` is a correctly sized sockaddr_un for `len` bytes.
        let r = unsafe { libc::bind(fd.as_raw_fd(), &addr as *const _ as *const libc::sockaddr, len) };
        if r < 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }

    fn connect_unix(fd: &OwnedFd, path: &std::ffi::OsStr) -> io::Result<()> {
        use std::os::unix::ffi::OsStrExt;
        let (addr, len) = sockaddr_un(path.as_bytes())?;
        // SAFETY: as above.
        let r = unsafe { libc::connect(fd.as_raw_fd(), &addr as *const _ as *const libc::sockaddr, len) };
        if r < 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }

    fn send(fd: &OwnedFd, data: &[u8]) -> io::Result<()> {
        // SAFETY: writing `data.len()` bytes from a valid slice to our socket.
        let n = unsafe { libc::send(fd.as_raw_fd(), data.as_ptr() as *const libc::c_void, data.len(), 0) };
        if n < 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }

    /// Reads one datagram, waiting until `deadline`. `None` on timeout.
    fn recv_line(fd: &OwnedFd, deadline: Instant) -> io::Result<Option<String>> {
        let mut pfd = libc::pollfd { fd: fd.as_raw_fd(), events: libc::POLLIN, revents: 0 };
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Ok(None);
        }
        // SAFETY: one valid pollfd for the socket we own.
        let r = unsafe { libc::poll(&mut pfd, 1, remaining.as_millis().min(i32::MAX as u128) as i32) };
        if r < 0 {
            let e = io::Error::last_os_error();
            if e.kind() == io::ErrorKind::Interrupted {
                return Ok(None);
            }
            return Err(e);
        }
        if r == 0 {
            return Ok(None);
        }
        let mut buf = [0u8; 4096];
        // SAFETY: reading at most `buf.len()` bytes into a live buffer.
        let n = unsafe { libc::recv(fd.as_raw_fd(), buf.as_mut_ptr() as *mut libc::c_void, buf.len(), 0) };
        if n < 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(Some(String::from_utf8_lossy(&buf[..n as usize]).trim_end().to_string()))
    }
}

#[cfg(target_os = "linux")]
pub use control::Control;
```

Replace the Task 7 placeholder in `lib.rs` with `pub mod p2p;`, ungated, since
only its inner `control` submodule is Linux-gated.

- [ ] **Step 4: Run the tests**

Run: `cargo test -q -p castr-miracast p2p::` on Windows (the pure half) and `bash scripts/pi/test-linux.sh` (which also compiles `Control` and runs clippy with `-D warnings`).
Expected: 7 passed on both.

- [ ] **Step 5: Commit**

```bash
cargo fmt -p castr-miracast
git add crates/castr-miracast/src/p2p.rs crates/castr-miracast/src/lib.rs
git commit -m "feat(miracast): wpa_supplicant control commands, events and socket"
```

---

### Task 8: The session, and replaying a recorded stream

**Files:**
- Create: `crates/castr-miracast/src/session.rs`
- Create: `crates/castr-miracast/tests/replay.rs`
- Modify: `crates/castr-miracast/src/lib.rs`

**Interfaces:**
- Consumes: `rtsp::{Negotiation, Action, Message, parse}`, `ts::{Demux, Unit}`, `rtp`, `wfd::Capabilities`.
- Produces: `session::Session::new(caps: Capabilities, session_id: String) -> Session` (declared ungated in `lib.rs`; it has no I/O), `Session::on_rtsp_bytes(&mut self, &[u8]) -> Vec<SinkEvent>`, `Session::on_rtp_datagram(&mut self, &[u8]) -> Vec<SinkEvent>`, `Session::tick(&mut self, now: Instant) -> Vec<SinkEvent>`, `Session::note_decode_error(&mut self, now: Instant) -> Vec<SinkEvent>`, `enum SinkEvent { SendRtsp(String), Video { data: Vec<u8>, pts_us: Option<u64> }, Audio { data: Vec<u8>, pts_us: Option<u64> }, Started(VideoMode), Ended(&'static str) }`.

The session is the last pure layer: bytes in, events out, no sockets. That makes the whole media path testable by replaying a recorded stream.

- [ ] **Step 1: Write the failing tests**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::wfd::{AudioCodecs, Capabilities, ClientPorts, VideoFormats};
    use std::time::{Duration, Instant};

    fn caps() -> Capabilities {
        Capabilities {
            video: VideoFormats::only_720p30(),
            audio: AudioCodecs::lpcm_48k_stereo(),
            ports: ClientPorts { rtp_port: 5000 },
            max_bitrate_kbps: 8000,
            latency_management: true,
            format_change: true,
        }
    }

    fn sess() -> Session {
        Session::new(caps(), "01234567".into())
    }

    fn sent(events: &[SinkEvent]) -> Vec<String> {
        events
            .iter()
            .filter_map(|e| match e {
                SinkEvent::SendRtsp(s) => Some(s.clone()),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn rtsp_bytes_arriving_in_pieces_are_answered_once_complete() {
        let mut s = sess();
        let m1 = b"OPTIONS * RTSP/1.0\r\nCSeq: 1\r\nRequire: org.wfa.wfd1.0\r\n\r\n";
        assert!(s.on_rtsp_bytes(&m1[..12]).is_empty(), "partial message waits");
        let out = s.on_rtsp_bytes(&m1[12..]);
        assert_eq!(sent(&out).len(), 2, "the M1 answer and our M2");
    }

    #[test]
    fn two_messages_in_one_read_are_both_handled() {
        let mut s = sess();
        let mut buf = b"OPTIONS * RTSP/1.0\r\nCSeq: 1\r\n\r\n".to_vec();
        buf.extend_from_slice(b"GET_PARAMETER rtsp://x RTSP/1.0\r\nCSeq: 2\r\nContent-Length: 19\r\n\r\nwfd_video_formats\r\n");
        let out = s.on_rtsp_bytes(&buf);
        let msgs = sent(&out);
        assert_eq!(msgs.len(), 3, "M1 answer, M2, M3 answer");
        assert!(msgs[2].contains("wfd_video_formats: 40 00 02 04 00000020"));
    }

    #[test]
    fn a_video_unit_reaches_the_caller_as_an_event() {
        let mut s = sess();
        // Drive the negotiation to Playing so media is accepted.
        drive_to_playing(&mut s);
        for packet in crate::test_support::stream_with_one_access_unit() {
            let mut rtp = vec![0x80, 33, 0, 1, 0, 0, 0, 0, 0, 0, 0, 0];
            rtp.extend_from_slice(&packet);
            let out = s.on_rtp_datagram(&rtp);
            if let Some(SinkEvent::Video { data, .. }) = out.into_iter().find(|e| matches!(e, SinkEvent::Video { .. })) {
                assert!(data.starts_with(&[0, 0, 0, 1]), "Annex B access unit");
                return;
            }
        }
        panic!("no video unit was emitted");
    }

    #[test]
    fn a_decode_error_asks_the_source_for_a_keyframe_at_most_twice_a_second() {
        let mut s = sess();
        drive_to_playing(&mut s);
        let t0 = Instant::now();
        let first = s.note_decode_error(t0);
        assert!(sent(&first)[0].contains("wfd_idr_request"), "{:?}", sent(&first));
        assert!(sent(&s.note_decode_error(t0 + Duration::from_millis(100))).is_empty());
        assert!(!sent(&s.note_decode_error(t0 + Duration::from_millis(700))).is_empty());
    }

    #[test]
    fn silence_ends_the_session_with_a_reason() {
        let mut s = sess();
        drive_to_playing(&mut s);
        let out = s.tick(Instant::now() + Duration::from_secs(120));
        assert!(
            out.iter().any(|e| matches!(e, SinkEvent::Ended(_))),
            "{out:?}"
        );
    }

    #[test]
    fn media_before_play_is_ignored() {
        let mut s = sess();
        let mut rtp = vec![0x80, 33, 0, 1, 0, 0, 0, 0, 0, 0, 0, 0];
        rtp.extend_from_slice(&[0x47; 188]);
        assert!(s.on_rtp_datagram(&rtp).is_empty());
    }

    fn drive_to_playing(s: &mut Session) {
        s.on_rtsp_bytes(b"OPTIONS * RTSP/1.0\r\nCSeq: 1\r\n\r\n");
        s.on_rtsp_bytes(b"GET_PARAMETER rtsp://x RTSP/1.0\r\nCSeq: 2\r\nContent-Length: 19\r\n\r\nwfd_video_formats\r\n");
        let body = "wfd_video_formats: 00 00 02 04 00000020 00000000 00000000 00 0000 0000 00 none none\r\nwfd_presentation_URL: rtsp://192.168.173.1/wfd1.0/streamid=0 none\r\n";
        let m4 = format!(
            "SET_PARAMETER rtsp://x RTSP/1.0\r\nCSeq: 3\r\nContent-Length: {}\r\n\r\n{}",
            body.len(),
            body
        );
        s.on_rtsp_bytes(m4.as_bytes());
        let trig = "wfd_trigger_method: SETUP\r\n";
        let m5 = format!(
            "SET_PARAMETER rtsp://x RTSP/1.0\r\nCSeq: 4\r\nContent-Length: {}\r\n\r\n{}",
            trig.len(),
            trig
        );
        s.on_rtsp_bytes(m5.as_bytes());
        s.on_rtsp_bytes(b"RTSP/1.0 200 OK\r\nCSeq: 100\r\nSession: abcdef12;timeout=60\r\n\r\n");
    }
}
```

`crate::test_support::stream_with_one_access_unit()` is a `#[cfg(test)] pub(crate)` helper added in this task to `lib.rs`, building the same PAT, PMT and PES packets Task 4's tests use; factor those builders out of `ts.rs`'s test module into `test_support` and have both call them, so there is one definition.

- [ ] **Step 2: Run to watch them fail**

Run: `cargo test -q -p castr-miracast session::`
Expected: compile errors.

- [ ] **Step 3: Implement `session.rs`**

```rust
// crates/castr-miracast/src/session.rs
//! One sink session: RTSP negotiation, RTP reception, MPEG-TS demux, and the
//! recovery rules, expressed as bytes in and events out. No sockets here, so
//! the whole media path can be replayed from a recording in a test.

use crate::rtp;
use crate::rtsp::{self, Action, Negotiation, VideoMode};
use crate::ts::{self, Unit};
use crate::wfd::Capabilities;
use std::time::Instant;

/// RTP payload type for MPEG-TS, which is what Wi-Fi Display carries.
const PT_MP2T: u8 = 33;
/// Packets held while waiting for a late one.
const REORDER_WINDOW: usize = 32;

#[derive(Debug)]
pub enum SinkEvent {
    /// Formatted RTSP text the caller must write to the control socket.
    SendRtsp(String),
    Video { data: Vec<u8>, pts_us: Option<u64> },
    Audio { data: Vec<u8>, pts_us: Option<u64> },
    Started(VideoMode),
    Ended(&'static str),
}

pub struct Session {
    negotiation: Negotiation,
    rtsp_buf: Vec<u8>,
    reorder: rtp::Reorder,
    demux: ts::Demux,
    playing: bool,
    /// Loss seen since the last keyframe request, so a gap can trigger one.
    lost_at_last_check: u64,
}

impl Session {
    pub fn new(caps: Capabilities, session_id: String) -> Self {
        Self {
            negotiation: Negotiation::new(caps, session_id),
            rtsp_buf: Vec::new(),
            reorder: rtp::Reorder::new(REORDER_WINDOW),
            demux: ts::Demux::new(),
            playing: false,
            lost_at_last_check: 0,
        }
    }

    /// Feeds bytes read from the RTSP socket; returns what to send back.
    pub fn on_rtsp_bytes(&mut self, bytes: &[u8]) -> Vec<SinkEvent> {
        self.rtsp_buf.extend_from_slice(bytes);
        let mut out = Vec::new();
        loop {
            match rtsp::parse(&self.rtsp_buf) {
                Ok(Some((msg, used))) => {
                    self.rtsp_buf.drain(..used);
                    let actions = self.negotiation.on_message(&msg);
                    self.apply(actions, &mut out);
                }
                Ok(None) => break,
                Err(e) => {
                    self.rtsp_buf.clear();
                    tracing::warn!("miracast: bad RTSP message: {e}");
                    out.push(SinkEvent::Ended("malformed RTSP"));
                    break;
                }
            }
        }
        out
    }

    /// Feeds one UDP datagram from the RTP socket.
    pub fn on_rtp_datagram(&mut self, datagram: &[u8]) -> Vec<SinkEvent> {
        if !self.playing {
            return Vec::new();
        }
        let Some(packet) = rtp::parse(datagram) else {
            return Vec::new();
        };
        if packet.payload_type != PT_MP2T {
            return Vec::new();
        }
        let mut out = Vec::new();
        for p in self.reorder.push(packet) {
            for chunk in p.payload.chunks(ts::PACKET_LEN) {
                for unit in self.demux.push(chunk) {
                    out.push(match unit {
                        Unit::Video { data, pts_us } => SinkEvent::Video { data, pts_us },
                        Unit::Audio { data, pts_us } => SinkEvent::Audio { data, pts_us },
                    });
                }
            }
        }
        // A lost packet or a continuity break damaged a frame: ask for an IDR
        // rather than waiting for the source's next scheduled keyframe.
        let lost = self.reorder.lost() + self.demux.stats().continuity_errors;
        if lost > self.lost_at_last_check {
            self.lost_at_last_check = lost;
            let actions = self.negotiation.request_idr(Instant::now());
            self.apply(actions, &mut out);
        }
        out
    }

    /// Time-driven work: keep-alives and the silence timeout.
    pub fn tick(&mut self, now: Instant) -> Vec<SinkEvent> {
        let actions = self.negotiation.tick(now);
        let mut out = Vec::new();
        self.apply(actions, &mut out);
        out
    }

    /// The decoder lost its reference; ask the source for a keyframe.
    pub fn note_decode_error(&mut self, now: Instant) -> Vec<SinkEvent> {
        let actions = self.negotiation.request_idr(now);
        let mut out = Vec::new();
        self.apply(actions, &mut out);
        out
    }

    fn apply(&mut self, actions: Vec<Action>, out: &mut Vec<SinkEvent>) {
        for a in actions {
            match a {
                Action::Send(m) => out.push(SinkEvent::SendRtsp(m.format())),
                Action::Play => {
                    self.playing = true;
                    if let Some(mode) = self.negotiation.chosen_video() {
                        out.push(SinkEvent::Started(mode));
                    }
                }
                Action::Teardown(reason) => {
                    self.playing = false;
                    out.push(SinkEvent::Ended(reason));
                }
            }
        }
    }
}
```

- [ ] **Step 4: Write the replay test**

`crates/castr-miracast/tests/replay.rs` proves the whole media path with one synthetic recording, which is what makes Task 10's hardware bring-up debuggable: if the replay passes and the hardware does not, the fault is in the radio layer.

```rust
//! Replays a synthetic Wi-Fi Display stream through the session and checks
//! that the access units come out whole and in order.
#![cfg(target_os = "linux")]

use castr_miracast::session::{Session, SinkEvent};
use castr_miracast::wfd::{AudioCodecs, Capabilities, ClientPorts, VideoFormats};

#[test]
fn a_recorded_stream_replays_into_ordered_access_units() {
    let caps = Capabilities {
        video: VideoFormats::only_720p30(),
        audio: AudioCodecs::lpcm_48k_stereo(),
        ports: ClientPorts { rtp_port: 5000 },
        max_bitrate_kbps: 8000,
        latency_management: true,
        format_change: true,
    };
    let mut s = Session::new(caps, "01234567".into());
    for msg in castr_miracast::test_support::negotiation_to_playing() {
        s.on_rtsp_bytes(msg.as_bytes());
    }
    let mut video = Vec::new();
    for (i, datagram) in castr_miracast::test_support::recorded_stream(24).into_iter().enumerate() {
        let _ = i;
        for e in s.on_rtp_datagram(&datagram) {
            if let SinkEvent::Video { data, pts_us } = e {
                video.push((data, pts_us));
            }
        }
    }
    assert!(video.len() >= 20, "expected the units back, got {}", video.len());
    assert!(video.iter().all(|(d, _)| d.starts_with(&[0, 0, 0, 1])));
    let ts: Vec<u64> = video.iter().filter_map(|(_, p)| *p).collect();
    assert!(ts.windows(2).all(|w| w[0] < w[1]), "timestamps ascend: {ts:?}");
}
```

`test_support::negotiation_to_playing()` and `test_support::recorded_stream(n)` are `pub` (not `cfg(test)`) helpers in `lib.rs` behind `#[doc(hidden)]`, since an integration test is a separate crate and cannot see `cfg(test)` items. `recorded_stream(n)` builds `n` access units of increasing timestamp, wraps them in PES and TS packets, and packs seven TS packets per RTP datagram, which is what a real source does.

- [ ] **Step 5: Run everything**

Run: `bash scripts/pi/test-linux.sh` (clippy `-D warnings` plus all tests including the replay) and `cargo test -q --workspace` on Windows.
Expected: green on both; the replay test runs only on Linux.

- [ ] **Step 6: Commit**

```bash
cargo fmt -p castr-miracast
git add crates/castr-miracast/src crates/castr-miracast/tests
git commit -m "feat(miracast): the sink session, provable by replaying a recorded stream"
```

---

### Task 9: The display arbiter and receiver wiring

**Files:**
- Create: `crates/castr-receiver/src/display.rs`
- Modify: `crates/castr-receiver/src/pipeline.rs`, `crates/castr-receiver/src/main.rs`

**Interfaces:**
- Produces: `display::DisplayArbiter::new() -> DisplayArbiter`, `DisplayArbiter::try_acquire(&self, who: Owner) -> bool`, `DisplayArbiter::release(&self, who: Owner)`, `DisplayArbiter::owner(&self) -> Owner`, `enum Owner { Idle, Castr, Miracast }`; `pipeline::ReceiverConfig` gains `pub miracast: MiracastChoice` and `pub miracast_name: Option<String>`, `pub miracast_channel: Option<u32>`; `enum MiracastChoice { On, Off, Auto }`.

- [ ] **Step 1: Write the failing tests**

In `display.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    #[test]
    fn an_idle_display_is_granted_to_whoever_asks_first() {
        let a = DisplayArbiter::new();
        assert_eq!(a.owner(), Owner::Idle);
        assert!(a.try_acquire(Owner::Castr));
        assert_eq!(a.owner(), Owner::Castr);
    }

    #[test]
    fn the_second_protocol_is_refused_until_the_first_releases() {
        let a = DisplayArbiter::new();
        assert!(a.try_acquire(Owner::Miracast));
        assert!(!a.try_acquire(Owner::Castr), "castr is refused");
        a.release(Owner::Miracast);
        assert_eq!(a.owner(), Owner::Idle);
        assert!(a.try_acquire(Owner::Castr));
    }

    #[test]
    fn acquiring_twice_from_the_same_owner_succeeds_and_release_is_idempotent() {
        let a = DisplayArbiter::new();
        assert!(a.try_acquire(Owner::Castr));
        assert!(a.try_acquire(Owner::Castr), "reentrant for the same owner");
        a.release(Owner::Castr);
        a.release(Owner::Castr);
        assert_eq!(a.owner(), Owner::Idle);
    }

    #[test]
    fn releasing_from_the_wrong_owner_does_nothing() {
        let a = DisplayArbiter::new();
        a.try_acquire(Owner::Castr);
        a.release(Owner::Miracast);
        assert_eq!(a.owner(), Owner::Castr, "a stale release cannot steal the display");
    }

    #[test]
    fn it_is_shareable_across_threads() {
        let a = Arc::new(DisplayArbiter::new());
        let b = a.clone();
        let t = std::thread::spawn(move || b.try_acquire(Owner::Miracast));
        let first = t.join().unwrap();
        let second = a.try_acquire(Owner::Castr);
        assert!(first ^ second, "exactly one of the two holds it");
    }
}
```

- [ ] **Step 2: Run to watch them fail**

Run: `cargo test -q -p castr-receiver display::`
Expected: compile errors.

- [ ] **Step 3: Implement `display.rs`**

```rust
// crates/castr-receiver/src/display.rs
//! Which protocol owns the screen.
//!
//! The Pi speaks two protocols and has one display. Whoever connects first
//! owns it until they disconnect; the other is refused with a clear message.
//! Neither can preempt the other, so a guest presenting cannot be knocked off
//! by a background reconnect, and vice versa.

use std::sync::Mutex;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Owner {
    Idle,
    Castr,
    Miracast,
}

pub struct DisplayArbiter {
    owner: Mutex<Owner>,
}

impl Default for DisplayArbiter {
    fn default() -> Self {
        Self::new()
    }
}

impl DisplayArbiter {
    pub fn new() -> Self {
        Self { owner: Mutex::new(Owner::Idle) }
    }

    pub fn owner(&self) -> Owner {
        *self.owner.lock().unwrap()
    }

    /// Grants the display when it is free, or already held by `who`.
    pub fn try_acquire(&self, who: Owner) -> bool {
        let mut o = self.owner.lock().unwrap();
        if *o == Owner::Idle || *o == who {
            *o = who;
            true
        } else {
            false
        }
    }

    /// Releases the display, but only for the owner that holds it.
    pub fn release(&self, who: Owner) {
        let mut o = self.owner.lock().unwrap();
        if *o == who {
            *o = Owner::Idle;
        }
    }
}
```

- [ ] **Step 4: Wire it into the castr path**

In `pipeline.rs`: add `mod display;` to `main.rs`, create one `Arc<DisplayArbiter>` in `run()`, put it in `NetConfig`, and in the connection handler, immediately before the receiver sends `HelloAck`, refuse when the display is held:

```rust
    if !cfg.display.try_acquire(display::Owner::Castr) {
        link.send_control(&ControlMessage::Error {
            code: 5,
            message: "display busy".into(),
        })
        .await?;
        tracing::info!("refusing castr sender: display owned by {:?}", cfg.display.owner());
        return Ok(());
    }
```

and release it on every exit from the session (the existing "session ended" path plus the error paths), with `cfg.display.release(display::Owner::Castr);`.

- [ ] **Step 5: Add the CLI flags**

In `main.rs`:

```rust
    /// Accept Miracast sources (Linux only)
    #[arg(long, value_enum, default_value_t = MiracastChoice::Auto)]
    miracast: MiracastChoice,
    /// Name shown in the Windows cast list (defaults to the hostname)
    #[arg(long)]
    miracast_name: Option<String>,
    /// 2.4 GHz channel for the Wi-Fi Direct group
    #[arg(long, value_parser = ["1", "6", "11", "auto"], default_value = "auto")]
    miracast_channel: String,
```

with `MiracastChoice` mirrored into `ReceiverConfig` and the channel parsed to `Option<u32>` (`"auto"` becomes `None`).

- [ ] **Step 6: Verify**

Run: `cargo test -q -p castr-receiver display::` (5 passed), `cargo test -q --workspace`, `cargo clippy --workspace --tests`, and `bash scripts/pi/build-pi.sh`.

- [ ] **Step 7: Commit**

```bash
cargo fmt -p castr-receiver
git add crates/castr-receiver/src
git commit -m "feat(receiver): display arbiter and the miracast CLI flags"
```

---

### Task 10: Sink lifecycle and hardware bring-up

**Files:**
- Create: `crates/castr-miracast/src/sink.rs`
- Create: `scripts/pi/wpa_supplicant-p2p.conf`
- Modify: `crates/castr-miracast/src/lib.rs`, `crates/castr-receiver/src/pipeline.rs`, `scripts/pi/setup.sh`

**Interfaces:**
- Consumes: `p2p::{Control, Command, Event}`, `dhcp`, `session::{Session, SinkEvent}`, `wfd::{Capabilities, DeviceInfo, device_info_subelement}`.
- Produces: `sink::SinkConfig { pub name: String, pub channel: Option<u32>, pub rtsp_port: u16, pub rtp_port: u16, pub paired_path: PathBuf }`, `sink::Sink::start(SinkConfig, Arc<DisplayArbiterHandle>) -> anyhow::Result<Sink>`, `Sink::events(&self) -> Receiver<SinkOut>`, `enum SinkOut { Pin(String), Video { data: Vec<u8>, pts_us: Option<u64> }, Audio { data: Vec<u8>, pts_us: Option<u64> }, Started, Ended(String) }`, `Sink::note_decode_error(&self)`, `Sink::stop(self)`.

`DisplayArbiterHandle` is a small trait (`fn try_acquire(&self) -> bool; fn release(&self);`) so `castr-miracast` does not depend on the receiver crate.

- [ ] **Step 1: The supplicant configuration**

```
# scripts/pi/wpa_supplicant-p2p.conf
# Wi-Fi Direct only: no station networks, no auto-connect. The sink owns this
# supplicant instance and drives it over the control socket below.
ctrl_interface=/run/wpa_supplicant_castr
update_config=0
device_name=castr
device_type=7-0050F204-1
# 0x2588: WPS display method, which is how the PIN reaches the TV.
config_methods=display
p2p_go_intent=15
p2p_go_ht40=1
```

`setup.sh` installs this to `/etc/castr/wpa_supplicant-p2p.conf` and creates `/run/wpa_supplicant_castr` at boot (a `tmpfiles.d` entry), and adds `wpa_supplicant` to the runtime packages. The receiver's systemd unit gains `AmbientCapabilities=CAP_NET_ADMIN CAP_NET_RAW` so the sink can start the supplicant and bind port 67 for DHCP.

- [ ] **Step 2: Implement `sink.rs`**

The lifecycle, in one thread:

1. Start `wpa_supplicant -i wlan0 -c /etc/castr/wpa_supplicant-p2p.conf -B` if it is not already running, then `Control::open("/run/wpa_supplicant_castr", "wlan0")` and `attach()`.
2. `SET wifi_display 1`, then `WFD_SUBELEM_SET 0 <device_info_subelement>` with `session_available: true`.
3. Pick the channel: the configured one, or the least busy of 1, 6 and 11 from `SCAN` results, defaulting to 6 when the scan yields nothing.
4. `P2P_GROUP_ADD persistent freq=<f>`; wait for `Event::GroupStarted`, recording the group interface name.
5. Bring the group interface up with `192.168.173.1/29` and start the DHCP responder on port 67 bound to that interface.
6. On `Event::ProvisionRequest`: if the peer is in `paired.toml`'s `[miracast]` section, authorise silently; otherwise generate an eight-digit PIN (seven digits plus the WPS checksum digit), emit `SinkOut::Pin`, and send `WPS_PIN any <pin>`. On `Event::WpsSuccess`, record the peer.
7. Listen on TCP 7236, accept one connection, and run the `Session`, forwarding `SinkEvent`s as `SinkOut`s and writing `SendRtsp` bytes back.
8. On session end: release the display, remove the group, re-advertise, and go back to step 4 within two seconds.

Every step logs what it sent and what came back, because this is the layer that will need debugging against real hardware.

- [ ] **Step 3: Wire the sink into the receiver**

In `pipeline.rs`, when `miracast` is `On`, or `Auto` and `/sys/class/net/wlan0` exists: start the sink on its own thread, feed `SinkOut::Video` into the same jitter buffer the castr path uses (the decoder is shared, so `DisplayOwner` guarantees only one protocol feeds it at a time), `SinkOut::Audio` into the audio output, `SinkOut::Pin` into the overlay, and call `sink.note_decode_error()` from the decode thread's error arm when the Miracast path owns the display. When `Auto` finds no `wlan0`, log the reason and carry on.

- [ ] **Step 4: Bring it up on the Pi**

Deploy, then work through the sequence by hand first, recording each command and its reply in the task report:

```bash
ssh dietpi@192.168.88.157 'sudo rfkill unblock wifi; sudo ip link set wlan0 up; \
  sudo wpa_supplicant -i wlan0 -c /etc/castr/wpa_supplicant-p2p.conf -B; \
  sudo /sbin/wpa_cli -p /run/wpa_supplicant_castr -i wlan0 status'
```

Then `SET wifi_display 1`, `WFD_SUBELEM_SET 0 ...`, `P2P_GROUP_ADD persistent freq=2437`, and confirm with `iw dev` that a `p2p-wlan0-0` interface exists in AP mode. From Windows, open the Cast panel (Windows+K) and confirm the Pi appears. Do not proceed to the service until the manual sequence works; if it does not, report exactly which command failed and its reply.

- [ ] **Step 5: Verify and commit**

`bash scripts/pi/test-linux.sh`, `bash scripts/pi/build-pi.sh`, `cargo test -q --workspace` on Windows, then deploy and confirm the receiver starts the sink and logs the group interface.

```bash
git add crates/castr-miracast/src/sink.rs crates/castr-miracast/src/lib.rs \
        crates/castr-receiver/src/pipeline.rs scripts/pi
git commit -m "feat(miracast): sink lifecycle, supplicant config and receiver wiring"
```

---

### Task 11: End-to-end verification

**Files:**
- Create: `docs/superpowers/verification/2026-09-02-castr-miracast-sink-e2e.md`
- Modify: `README.md`

- [ ] **Step 1: Cast from Windows**

With the Pi's service running, press Windows+K on the PC, choose the Pi, and enter the PIN shown on the TV. Record: the journal from `sudo journalctl -u castr-receiver` covering discovery, the PIN, the RTSP exchange (every method and status), the chosen video mode, and the first `perf:` line; and a frame dump proving the desktop is on screen, captured against the synthetic test pattern rather than personal content (`powershell -NoProfile -ExecutionPolicy Bypass -File "C:/Users/SETHSA~1/AppData/Local/Temp/claude/D--miracast/2b4241f0-4dcd-4e6e-89b5-6550c719ac5e/scratchpad/testpattern.ps1"`).

- [ ] **Step 2: Measure and stress**

Record: glass-to-glass latency by filming the PC and TV together with a stopwatch on screen; a ten-minute soak with every disconnect logged; a reconnect after teardown, confirming no PIN is asked the second time; a run with Bluetooth disabled on the PC and one with it active, since that adapter shares an antenna and the health check flags it; and a castr cast attempted while Miracast owns the display, confirming it is refused with "display busy" rather than stealing the screen.

- [ ] **Step 3: Write the document and the README section**

The verification document takes the same shape as the Pi hardening one: commands, observed output, a PASS or FAIL against each spec 8 criterion, and a section naming anything that fell short. The README gains a "Casting from Windows without installing anything" section: press Windows+K, pick the Pi, type the PIN, with the 720p30 ceiling, the no-HDCP limitation and the one-protocol-at-a-time rule stated plainly.

- [ ] **Step 4: Commit**

```bash
git add docs/superpowers/verification/2026-09-02-castr-miracast-sink-e2e.md README.md
git commit -m "docs: Miracast sink end-to-end verification and README"
```
