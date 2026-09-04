//! What a sink says it can accept, and what we choose from it.
//!
//! This is where interoperability is won or lost, and it is pure decision logic
//! over parsed text - so once a real display's reply has been captured it
//! becomes a fixture, and the decision stays testable forever after.
//!
//! Two rules matter more than the parsing. A parameter we do not recognise is
//! ignored rather than fatal, because every real display sends some. And having
//! nothing in common is reported with both sides' offers, because that is the
//! failure most likely to meet an unfamiliar display and the one hardest to
//! diagnose from the outside.

use crate::rtsp::VideoMode;
use crate::wfd::parse_parameter_body;
use castr_media::codec::Mode;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SinkCaps {
    pub cea: u32,
    pub vesa: u32,
    pub hh: u32,
    pub profile: u8,
    pub level: u8,
    pub lpcm_modes: u32,
    pub rtp_port: u16,
    pub max_bitrate_kbps: Option<u32>,
    /// Present only when the display asks for content protection we cannot
    /// give it; `none` is recorded as absent.
    pub content_protection: Option<String>,
}

#[derive(Debug, PartialEq, Eq)]
pub enum CapsError {
    NoVideoFormats,
    MalformedVideoFormats(String),
}

impl std::fmt::Display for CapsError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CapsError::NoVideoFormats => write!(f, "the display advertised no video formats"),
            CapsError::MalformedVideoFormats(s) => {
                write!(f, "could not read wfd_video_formats: {s:?}")
            }
        }
    }
}

impl std::error::Error for CapsError {}

/// CEA table index to resolution.
///
/// `rtsp::cea_mode` covers only the single mode our own sink offers; a source
/// meets displays that offer many, so the fuller table lives here. Unify the
/// two when the sink learns more modes.
pub fn cea_mode(bit: u32) -> Option<VideoMode> {
    let (width, height, fps) = match bit {
        0 => (640, 480, 60),
        1 => (720, 480, 60),
        2 => (720, 480, 60),
        3 => (720, 576, 50),
        4 => (720, 576, 50),
        5 => (1280, 720, 30),
        6 => (1280, 720, 60),
        7 => (1920, 1080, 30),
        8 => (1920, 1080, 60),
        9 => (1920, 1080, 30),
        10 => (1280, 720, 25),
        11 => (1280, 720, 50),
        12 => (1920, 1080, 25),
        13 => (1920, 1080, 50),
        14 => (1920, 1080, 25),
        15 => (1280, 720, 24),
        16 => (1920, 1080, 24),
        _ => return None,
    };
    Some(VideoMode { width, height, fps })
}

/// Reads an M3 response body. Unrecognised parameters are skipped; only a
/// missing or unreadable `wfd_video_formats` is fatal, because without it there
/// is nothing to choose from.
pub fn parse(body: &str) -> Result<SinkCaps, CapsError> {
    let params = parse_parameter_body(body);
    let get = |name: &str| {
        params
            .iter()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.as_str())
    };

    let video = get("wfd_video_formats").ok_or(CapsError::NoVideoFormats)?;
    let f: Vec<&str> = video.split_whitespace().collect();
    // native, preferred-display-mode, profile, level, then the three bitmaps.
    if f.len() < 7 {
        return Err(CapsError::MalformedVideoFormats(video.to_string()));
    }
    let bad = || CapsError::MalformedVideoFormats(video.to_string());
    let hex32 = |s: &str| u32::from_str_radix(s, 16).ok();
    let hex8 = |s: &str| u8::from_str_radix(s, 16).ok();

    Ok(SinkCaps {
        profile: hex8(f[2]).ok_or_else(bad)?,
        level: hex8(f[3]).ok_or_else(bad)?,
        cea: hex32(f[4]).ok_or_else(bad)?,
        vesa: hex32(f[5]).ok_or_else(bad)?,
        hh: hex32(f[6]).ok_or_else(bad)?,
        lpcm_modes: get("wfd_audio_codecs")
            .and_then(|v| v.split_whitespace().nth(1))
            .and_then(hex32)
            .unwrap_or(0),
        rtp_port: get("wfd_client_rtp_ports")
            .and_then(|v| v.split_whitespace().nth(1))
            .and_then(|p| p.parse().ok())
            .unwrap_or(5000),
        max_bitrate_kbps: get("microsoft_max_bitrate").and_then(|v| v.trim().parse().ok()),
        content_protection: get("wfd_content_protection")
            .map(str::to_string)
            .filter(|v| v.trim() != "none"),
    })
}

#[derive(Debug)]
pub struct NoCommonFormat {
    pub sink_offered: Vec<VideoMode>,
    pub we_offered: Vec<VideoMode>,
}

impl std::fmt::Display for NoCommonFormat {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let show = |ms: &[VideoMode]| {
            if ms.is_empty() {
                return "nothing".to_string();
            }
            ms.iter()
                .map(|m| format!("{}x{}p{}", m.width, m.height, m.fps))
                .collect::<Vec<_>>()
                .join(", ")
        };
        write!(
            f,
            "no video format in common: the display offered {}; we offered {}",
            show(&self.sink_offered),
            show(&self.we_offered)
        )
    }
}

impl std::error::Error for NoCommonFormat {}

/// Every mode a sink's CEA bitmap advertises, in table order.
pub fn sink_modes(c: &SinkCaps) -> Vec<VideoMode> {
    (0..32)
        .filter(|b| c.cea & (1 << b) != 0)
        .filter_map(cea_mode)
        .collect()
}

/// What one mode costs, roughly, in Mbit/s.
///
/// A formula rather than a table of magic numbers: pixels times frames, scaled
/// so that 1080p60 lands near 20 Mbit/s, which is about what H.264 needs for a
/// desktop at that size. It only has to be good enough to keep us from
/// proposing a mode a display has already said it cannot carry.
pub fn needs_mbps(m: VideoMode) -> u16 {
    let bits = m.width as u64 * m.height as u64 * m.fps as u64;
    ((bits / 6_000_000).max(1)) as u16
}

/// Every mode we are willing to encode, in the order we would rather have
/// them, dropping any the display cannot carry.
///
/// The ordering is what the Game/Quality toggle already means everywhere else
/// in castr: Quality prefers a bigger picture, which keeps text legible, and
/// Game prefers a faster one, which keeps motion smooth.
pub fn our_modes(mode: Mode, ceiling_mbps: Option<u16>) -> Vec<VideoMode> {
    let mut all = vec![
        VideoMode { width: 1920, height: 1080, fps: 60 },
        VideoMode { width: 1920, height: 1080, fps: 30 },
        VideoMode { width: 1280, height: 720, fps: 60 },
        VideoMode { width: 1280, height: 720, fps: 30 },
        VideoMode { width: 640, height: 480, fps: 60 },
    ];
    match mode {
        Mode::Quality => all.sort_by(|a, b| {
            b.height.cmp(&a.height).then(b.fps.cmp(&a.fps))
        }),
        Mode::Game => all.sort_by(|a, b| b.fps.cmp(&a.fps).then(b.height.cmp(&a.height))),
    }
    if let Some(ceiling) = ceiling_mbps {
        all.retain(|m| needs_mbps(*m) <= ceiling);
    }
    all
}

/// Our modes in preference order; the first the display also offers wins.
pub fn choose(sink: &SinkCaps, ours: &[VideoMode]) -> Result<VideoMode, NoCommonFormat> {
    let offered = sink_modes(sink);
    ours.iter()
        .find(|m| offered.contains(m))
        .copied()
        .ok_or_else(|| NoCommonFormat {
            sink_offered: offered,
            we_offered: ours.to_vec(),
        })
}

/// The CEA bit standing for a mode, for the body that selects it.
pub fn mode_bit(mode: VideoMode) -> Option<u32> {
    (0..32).find(|b| cea_mode(*b) == Some(mode)).map(|b| 1u32 << b)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::wfd::{capabilities_body, AudioCodecs, Capabilities, ClientPorts, VideoFormats};

    /// Exactly what our own sink sends, so both halves are tested together.
    fn our_sink_body() -> String {
        capabilities_body(&Capabilities {
            video: VideoFormats::only_720p30(),
            audio: AudioCodecs::lpcm_48k_stereo(),
            ports: ClientPorts { rtp_port: 5000 },
            max_bitrate_kbps: 20_000,
            latency_management: true,
            format_change: true,
        })
    }

    const P720P30: VideoMode = VideoMode {
        width: 1280,
        height: 720,
        fps: 30,
    };
    const P1080P60: VideoMode = VideoMode {
        width: 1920,
        height: 1080,
        fps: 60,
    };

    #[test]
    fn our_own_sink_capabilities_parse() {
        let c = parse(&our_sink_body()).expect("our own sink must parse");
        assert_eq!(c.cea, 0x0000_0020, "bit 5 is 1280x720p30");
        assert_eq!(c.profile, 0x02);
        assert_eq!(c.level, 0x04);
        assert_eq!(c.rtp_port, 5000);
        assert_eq!(c.lpcm_modes, 0x0000_0002);
        assert_eq!(c.max_bitrate_kbps, Some(20_000));
        assert_eq!(c.content_protection, None, "'none' is not a requirement");
    }

    #[test]
    fn the_only_common_mode_is_chosen() {
        let c = parse(&our_sink_body()).unwrap();
        assert_eq!(choose(&c, &[P1080P60, P720P30]).unwrap(), P720P30);
    }

    #[test]
    fn preference_order_is_ours_not_the_displays() {
        // A display offering everything must still get what we asked for first.
        let mut c = parse(&our_sink_body()).unwrap();
        c.cea = 0xffff_ffff;
        assert_eq!(choose(&c, &[P1080P60, P720P30]).unwrap(), P1080P60);
        assert_eq!(choose(&c, &[P720P30, P1080P60]).unwrap(), P720P30);
    }

    #[test]
    fn an_unknown_parameter_is_ignored_not_fatal() {
        // A real television sends vendor parameters we have never seen, and
        // refusing them would refuse the television.
        let body = format!("{}some_vendor_extension: whatever\r\n", our_sink_body());
        assert!(parse(&body).is_ok());
    }

    #[test]
    fn no_common_format_says_what_each_side_offered() {
        let c = parse(&our_sink_body()).unwrap();
        let text = choose(&c, &[P1080P60]).unwrap_err().to_string();
        assert!(text.contains("1280x720"), "must name the display's offer: {text}");
        assert!(text.contains("1920x1080"), "must name ours: {text}");
    }

    #[test]
    fn a_body_with_no_video_formats_is_an_error_not_a_guess() {
        assert_eq!(
            parse("wfd_audio_codecs: LPCM 00000002 00\r\n").unwrap_err(),
            CapsError::NoVideoFormats
        );
    }

    #[test]
    fn a_truncated_video_formats_line_is_an_error() {
        assert!(matches!(
            parse("wfd_video_formats: 40 00 02\r\n").unwrap_err(),
            CapsError::MalformedVideoFormats(_)
        ));
    }

    #[test]
    fn a_display_demanding_content_protection_is_recorded() {
        // We cannot satisfy HDCP, and the honest answer is to say so rather
        // than stream a picture that will never appear.
        let body = our_sink_body().replace(
            "wfd_content_protection: none",
            "wfd_content_protection: HDCP2.0 port=1189",
        );
        let c = parse(&body).unwrap();
        assert_eq!(
            c.content_protection.as_deref(),
            Some("HDCP2.0 port=1189")
        );
    }

    #[test]
    fn quality_prefers_a_bigger_picture_and_game_a_faster_one() {
        // The toggle already means exactly this for castr's own protocol.
        let quality = our_modes(Mode::Quality, None);
        let game = our_modes(Mode::Game, None);
        assert_eq!(quality[0], VideoMode { width: 1920, height: 1080, fps: 60 });
        assert_eq!(
            quality[1],
            VideoMode { width: 1920, height: 1080, fps: 30 },
            "quality takes 1080p30 over 720p60"
        );
        assert_eq!(
            game[1],
            VideoMode { width: 1280, height: 720, fps: 60 },
            "game takes 720p60 over 1080p30"
        );
    }

    #[test]
    fn a_display_is_never_offered_more_than_it_says_it_can_carry() {
        // Our own Pi advertises 10 Mbit/s. Proposing 1080p60 to it would be
        // asking for a stream it has already said it cannot take.
        let modes = our_modes(Mode::Quality, Some(10));
        assert!(!modes.contains(&VideoMode { width: 1920, height: 1080, fps: 60 }));
        assert!(modes.contains(&P720P30));
    }

    #[test]
    fn a_generous_ceiling_keeps_everything() {
        // A television advertising 54 Mbit/s should see our whole list.
        assert_eq!(our_modes(Mode::Quality, Some(54)).len(), 5);
        assert_eq!(our_modes(Mode::Quality, None).len(), 5);
    }

    #[test]
    fn a_tiny_ceiling_still_leaves_something_to_offer() {
        // Better a small picture than "no format in common".
        let modes = our_modes(Mode::Quality, Some(3));
        assert!(!modes.is_empty(), "nothing left to propose");
        assert!(modes.iter().all(|m| needs_mbps(*m) <= 3));
    }

    #[test]
    fn the_cost_estimate_is_in_the_right_neighbourhood() {
        // It only has to be good enough to keep us from proposing the absurd.
        assert!((18..=22).contains(&needs_mbps(VideoMode { width: 1920, height: 1080, fps: 60 })));
        assert!((3..=6).contains(&needs_mbps(P720P30)));
    }

    #[test]
    fn against_our_own_sink_the_one_mode_it_offers_is_chosen() {
        // Whatever we now propose, a sink offering only 720p30 must still get
        // 720p30 - the working path must not break.
        let c = parse(&our_sink_body()).unwrap();
        for mode in [Mode::Quality, Mode::Game] {
            let ours = our_modes(mode, Some(10));
            assert_eq!(choose(&c, &ours).unwrap(), P720P30, "{mode:?}");
        }
    }

    #[test]
    fn the_bit_for_a_mode_round_trips() {
        assert_eq!(mode_bit(P720P30), Some(0x0000_0020));
        assert_eq!(cea_mode(5), Some(P720P30));
    }

    #[test]
    fn a_port_the_display_names_is_used_over_the_default() {
        let body = our_sink_body().replace("unicast 5000", "unicast 19000");
        assert_eq!(parse(&body).unwrap().rtp_port, 19000);
    }
}
