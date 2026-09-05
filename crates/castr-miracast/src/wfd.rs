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

/// The Wi-Fi Alliance display OUI, and the type that marks its information
/// element among the vendor elements a device advertises.
pub const WFD_OUI: [u8; 3] = [0x50, 0x6f, 0x9a];
pub const WFD_OUI_TYPE: u8 = 10;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeviceKind {
    Source,
    PrimarySink,
    SecondarySink,
    DualRole,
}

/// What a device says about itself before anything connects to it: enough to
/// tell a television from a printer, and to know its port, its ceiling and
/// whether it wants content protection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DeviceCaps {
    pub kind: DeviceKind,
    pub session_available: bool,
    pub content_protection: bool,
    pub rtsp_port: u16,
    pub max_throughput_mbps: u16,
}

/// Reads the 6-byte device-information subelement body: a flags field, the
/// session-management port, and a throughput ceiling in Mbit/s.
///
/// The mirror of `device_info_subelement`, which builds the same thing for the
/// sink. One layout, both directions, so the two cannot drift apart.
pub fn parse_device_info(body: &[u8]) -> Option<DeviceCaps> {
    if body.len() < 6 {
        return None;
    }
    let info = u16::from_be_bytes([body[0], body[1]]);
    Some(DeviceCaps {
        kind: match info & 0x0003 {
            0 => DeviceKind::Source,
            1 => DeviceKind::PrimarySink,
            2 => DeviceKind::SecondarySink,
            _ => DeviceKind::DualRole,
        },
        // Bits 4-5: 01 means a session is free to be started.
        session_available: (info >> 4) & 0x0003 == 1,
        content_protection: info & 0x0100 != 0,
        rtsp_port: u16::from_be_bytes([body[2], body[3]]),
        max_throughput_mbps: u16::from_be_bytes([body[4], body[5]]),
    })
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

    #[test]
    fn a_real_televisions_element_parses() {
        // Captured from a Samsung 75" Crystal UHD, 2026-09-04. Bytes from a
        // vendor we did not write are the only interoperability evidence
        // available until a television can actually be cast to.
        let body = [0x01, 0x11, 0x1c, 0x44, 0x00, 0x36];
        let c = parse_device_info(&body).expect("a real element must parse");
        assert_eq!(c.kind, DeviceKind::PrimarySink);
        assert!(c.session_available, "it was advertising a free session");
        assert!(c.content_protection, "this television supports HDCP");
        assert_eq!(c.rtsp_port, 7236);
        assert_eq!(c.max_throughput_mbps, 54);
    }

    #[test]
    fn our_own_sinks_element_parses_as_what_we_built() {
        // The builder and the parser must agree, or the source and the sink
        // disagree about the sink's own advertisement.
        let hex = device_info_subelement(&DeviceInfo {
            session_available: true,
            rtsp_port: 7236,
            max_throughput_mbps: 10,
        });
        let bytes: Vec<u8> = (0..hex.len() / 2)
            .map(|i| u8::from_str_radix(&hex[i * 2..i * 2 + 2], 16).expect("hex"))
            .collect();
        // The first two bytes are the subelement's length, not its body.
        let c = parse_device_info(&bytes[2..]).expect("our own element must parse");
        assert_eq!(c.kind, DeviceKind::PrimarySink);
        assert!(c.session_available);
        assert!(!c.content_protection);
        assert_eq!(c.rtsp_port, 7236);
        assert_eq!(c.max_throughput_mbps, 10);
    }

    #[test]
    fn a_source_is_not_mistaken_for_a_sink() {
        // Device type 00 is a source. Casting to one would never work.
        let body = [0x00, 0x10, 0x1c, 0x44, 0x00, 0x0a];
        assert_eq!(
            parse_device_info(&body).expect("parse").kind,
            DeviceKind::Source
        );
    }

    #[test]
    fn a_truncated_element_is_rejected_rather_than_guessed() {
        assert!(parse_device_info(&[0x01, 0x11, 0x1c]).is_none());
        assert!(parse_device_info(&[]).is_none());
    }

    #[test]
    fn an_unavailable_sink_says_so() {
        // Bits 4-5 clear: a sink already busy with somebody else.
        let body = [0x01, 0x01, 0x1c, 0x44, 0x00, 0x0a];
        let c = parse_device_info(&body).expect("parse");
        assert_eq!(c.kind, DeviceKind::PrimarySink);
        assert!(!c.session_available);
    }
}

/// WPS lives in its own information element, beside the Wi-Fi Display one.
pub const WPS_OUI: [u8; 3] = [0x00, 0x50, 0xf2];
pub const WPS_OUI_TYPE: u8 = 4;

/// How a device is willing to be paired with.
///
/// A source that only knows one ceremony can only pair with displays that
/// happen to offer it. Reading this is what lets the right one be chosen -
/// and it is read before connecting, from the same beacon the Wi-Fi Display
/// element arrives in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ConfigMethods(pub u16);

impl ConfigMethods {
    /// The device shows a PIN for someone to type in elsewhere.
    pub const DISPLAY: u16 = 0x0008;
    /// The device accepts a button press instead of a PIN.
    pub const PUSH_BUTTON: u16 = 0x0080;
    /// The device has a keypad, so it can accept a PIN typed into it.
    pub const KEYPAD: u16 = 0x0100;
    pub const PHYSICAL_PUSH_BUTTON: u16 = 0x0280;
    pub const VIRTUAL_PUSH_BUTTON: u16 = 0x0480;
    pub const VIRTUAL_DISPLAY: u16 = 0x2008;

    /// Whether every bit of `bits` is set. Composite methods such as
    /// `PHYSICAL_PUSH_BUTTON` are two bits, so a plain intersection would say
    /// yes when only half of one is present.
    pub fn has(self, bits: u16) -> bool {
        self.0 & bits == bits
    }

    pub fn push_button(self) -> bool {
        self.has(Self::PUSH_BUTTON)
    }

    /// The device can put a PIN on its own screen for us to type.
    pub fn shows_a_pin(self) -> bool {
        self.has(Self::DISPLAY) || self.has(Self::VIRTUAL_DISPLAY)
    }

    pub fn describe(self) -> String {
        let mut names = Vec::new();
        for (bits, name) in [
            (Self::DISPLAY, "display"),
            (Self::PUSH_BUTTON, "push-button"),
            (Self::KEYPAD, "keypad"),
        ] {
            if self.has(bits) {
                names.push(name);
            }
        }
        if names.is_empty() {
            return format!("no pairing method (0x{:04x})", self.0);
        }
        names.join(", ")
    }
}

/// Reads the Config Methods attribute out of a WPS element's value.
///
/// WPS attributes are a two-byte id, a two-byte length and a body, big-endian
/// throughout; Config Methods is id `0x1008` and is itself two bytes.
pub fn parse_config_methods(value: &[u8]) -> Option<ConfigMethods> {
    let mut i = 0usize;
    while i + 4 <= value.len() {
        let id = u16::from_be_bytes([value[i], value[i + 1]]);
        let len = u16::from_be_bytes([value[i + 2], value[i + 3]]) as usize;
        let end = i.checked_add(4)?.checked_add(len)?;
        if end > value.len() {
            return None;
        }
        if id == 0x1008 && len >= 2 {
            return Some(ConfigMethods(u16::from_be_bytes([
                value[i + 4],
                value[i + 5],
            ])));
        }
        i = end;
    }
    None
}

#[cfg(test)]
mod config_method_tests {
    use super::*;

    /// Read from the real devices in range on 2026-09-05. Genuine bytes from
    /// four vendors we did not write, which is the only interop evidence there
    /// is short of casting to each one.
    const ADAPTER: u16 = 0x2288; // MR-A202 wireless display adapter
    const SAMSUNG: u16 = 0x4388; // 75" Crystal UHD
    const LG: u16 = 0x3388; // webOS TV UN7000PUB
    const FIRE_TV: u16 = 0x4108; // Fire TV Stick
    const PRINTER: u16 = 0x0000; // Epson WF-2960

    fn tlv(id: u16, body: &[u8]) -> Vec<u8> {
        let mut v = id.to_be_bytes().to_vec();
        v.extend_from_slice(&(body.len() as u16).to_be_bytes());
        v.extend_from_slice(body);
        v
    }

    #[test]
    fn the_attribute_is_found_among_others() {
        let mut value = tlv(0x1044, &[0x02]); // WPS state, in the way
        value.extend(tlv(0x1008, &ADAPTER.to_be_bytes()));
        value.extend(tlv(0x1047, &[0u8; 16])); // UUID, after it
        assert_eq!(parse_config_methods(&value), Some(ConfigMethods(ADAPTER)));
    }

    #[test]
    fn an_element_without_the_attribute_yields_nothing() {
        let value = tlv(0x1044, &[0x02]);
        assert_eq!(parse_config_methods(&value), None);
    }

    #[test]
    fn a_truncated_element_is_survived_rather_than_panicking() {
        for cut in 0..12 {
            let full = tlv(0x1008, &ADAPTER.to_be_bytes());
            let _ = parse_config_methods(&full[..cut.min(full.len())]);
        }
        // A length that runs past the end must not be believed.
        let lying = vec![0x10, 0x08, 0xff, 0xff, 0x22];
        assert_eq!(parse_config_methods(&lying), None);
    }

    #[test]
    fn the_adapter_offers_a_button_and_a_screen_but_no_keypad() {
        let m = ConfigMethods(ADAPTER);
        assert!(m.push_button());
        assert!(m.shows_a_pin());
        assert!(!m.has(ConfigMethods::KEYPAD));
    }

    #[test]
    fn the_televisions_offer_a_button() {
        assert!(ConfigMethods(SAMSUNG).push_button());
        assert!(ConfigMethods(LG).push_button());
    }

    #[test]
    fn the_fire_tv_offers_no_button() {
        // The one display here that a push-button-only source could not pair
        // with at all.
        let m = ConfigMethods(FIRE_TV);
        assert!(!m.push_button());
        assert!(m.shows_a_pin());
    }

    #[test]
    fn a_printer_offers_nothing() {
        let m = ConfigMethods(PRINTER);
        assert!(!m.push_button());
        assert!(!m.shows_a_pin());
        assert!(m.describe().contains("no pairing method"));
    }

    #[test]
    fn a_composite_method_needs_both_its_bits() {
        // 0x0200 alone is half of PHYSICAL_PUSH_BUTTON; on its own it must not
        // read as one.
        assert!(!ConfigMethods(0x0200).has(ConfigMethods::PHYSICAL_PUSH_BUTTON));
        assert!(ConfigMethods(0x0280).has(ConfigMethods::PHYSICAL_PUSH_BUTTON));
    }

    #[test]
    fn methods_are_described_in_words() {
        assert_eq!(ConfigMethods(ADAPTER).describe(), "display, push-button");
        assert_eq!(ConfigMethods(FIRE_TV).describe(), "display, keypad");
    }
}

/// Reads a mid-session bitrate request out of an RTSP body.
///
/// A sink watching its own loss asks the source to send less: our own sink
/// does exactly this, logging "loss is up, asking the source for 2000 kbps".
/// A source that reads this only once, from the initial capabilities, and
/// ignores every later one has no way to ride out a link that is degrading -
/// it keeps sending the same rate until the session drops.
///
/// Returns `None` when the body carries no such request, which is the ordinary
/// case for every other parameter a display might set.
pub fn parse_max_bitrate_kbps(body: &str) -> Option<u32> {
    for line in body.lines() {
        // A line with no colon is skipped, not fatal: `wfd_idr_request` is a
        // bare flag and arrives in the same body as this request.
        let Some((name, value)) = line.split_once(':') else {
            continue;
        };
        if name.trim().eq_ignore_ascii_case("microsoft_max_bitrate") {
            return value.trim().parse::<u32>().ok();
        }
    }
    None
}

#[cfg(test)]
mod bitrate_request_tests {
    use super::*;

    #[test]
    fn a_request_is_read() {
        assert_eq!(
            parse_max_bitrate_kbps("microsoft_max_bitrate: 2000\r\n"),
            Some(2000)
        );
    }

    #[test]
    fn it_is_found_beside_other_parameters() {
        let body = "wfd_video_formats: 00 00 02 04 00000080\r\n\
                    microsoft_max_bitrate: 4000\r\n";
        assert_eq!(parse_max_bitrate_kbps(body), Some(4000));
    }

    #[test]
    fn the_name_is_matched_without_regard_to_case() {
        assert_eq!(
            parse_max_bitrate_kbps("Microsoft_Max_Bitrate: 1500\r\n"),
            Some(1500)
        );
    }

    #[test]
    fn a_bare_flag_on_an_earlier_line_does_not_stop_the_search() {
        // Exactly the body a display sends when it wants both at once.
        let body = "wfd_idr_request\r\nmicrosoft_max_bitrate: 4000\r\n";
        assert_eq!(parse_max_bitrate_kbps(body), Some(4000));
    }

    #[test]
    fn a_body_without_one_asks_for_nothing() {
        assert_eq!(parse_max_bitrate_kbps("wfd_idr_request\r\n"), None);
        assert_eq!(parse_max_bitrate_kbps(""), None);
    }

    #[test]
    fn a_value_that_is_not_a_number_is_ignored_rather_than_guessed() {
        assert_eq!(parse_max_bitrate_kbps("microsoft_max_bitrate: lots\r\n"), None);
        assert_eq!(parse_max_bitrate_kbps("microsoft_max_bitrate:\r\n"), None);
    }
}
