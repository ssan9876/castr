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
        Self {
            cea: 0x0000_0020,
            vesa: 0,
            hh: 0,
            profile: 0x02,
            level: 0x04,
        }
    }
}

/// Audio formats we accept: LPCM 48 kHz 16-bit stereo only (bit 1).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AudioCodecs {
    pub lpcm_modes: u32,
}

impl AudioCodecs {
    pub fn lpcm_48k_stereo() -> Self {
        Self {
            lpcm_modes: 0x0000_0002,
        }
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
    s.push_str(&format!(
        "wfd_audio_codecs: LPCM {:08X} 00\r\n",
        c.audio.lpcm_modes
    ));
    s.push_str("wfd_content_protection: none\r\n");
    s.push_str(&format!(
        "wfd_client_rtp_ports: RTP/AVP/UDP;unicast {} 0 mode=play\r\n",
        c.ports.rtp_port
    ));
    s.push_str(&format!(
        "microsoft_max_bitrate: {}\r\n",
        c.max_bitrate_kbps
    ));
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
pub fn device_info_subelement(d: &DeviceInfo) -> String {
    // Bits 0-1: device type, 01 = primary sink.
    // Bits 4-5: session availability, 01 = available, 00 = not available.
    let mut info: u16 = 0x0001;
    if d.session_available {
        info |= 0x0010;
    }
    // One unbroken lowercase hex string: the supplicant parses this as a
    // hexdump and answers FAIL if it contains spaces, which the hardware
    // bring-up confirmed. Lowercase because that is what `wpa_cli` prints
    // back, so a logged command can be pasted in verbatim.
    format!(
        "0006{:04x}{:04x}{:04x}",
        info, d.rtsp_port, d.max_throughput_mbps
    )
}

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
        assert!(
            !body.contains("wfd_uibc_capability"),
            "no input back channel"
        );
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
        assert_eq!(s, "000600111c44000a");
    }

    #[test]
    fn an_unavailable_sink_clears_the_availability_bits() {
        let s = device_info_subelement(&DeviceInfo {
            session_available: false,
            rtsp_port: 7236,
            max_throughput_mbps: 10,
        });
        assert_eq!(s, "000600011c44000a");
    }
}
