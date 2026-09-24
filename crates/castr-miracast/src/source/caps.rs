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
    pub video_formats: Vec<VideoCodecCaps>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VideoCodecCaps {
    pub profile: u8,
    pub level: u8,
    pub cea: u32,
    pub vesa: u32,
    pub hh: u32,
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VideoTable {
    Cea,
    Vesa,
    Hh,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ModeSelection {
    pub mode: VideoMode,
    pub table: VideoTable,
    pub bit: u32,
    /// WFD profile bitmap: bit 0 is constrained baseline, bit 1 constrained high.
    pub profile: u8,
    /// WFD level bitmap, reduced to the one level used for this stream.
    pub level: u8,
}

/// CEA table index to a progressive resolution.
///
/// `rtsp::cea_mode` covers only the single mode our own sink offers; a source
/// meets displays that offer many, so the fuller table lives here. Unify the
/// two when the sink learns more modes.
pub fn cea_mode(bit: u32) -> Option<VideoMode> {
    let (width, height, fps) = match bit {
        0 => (640, 480, 60),
        1 => (720, 480, 60),
        // 2, 4, 9 and 14 are interlaced modes. We do not silently advertise
        // them as progressive: the encoder and MPEG-TS signal progressive.
        3 => (720, 576, 50),
        5 => (1280, 720, 30),
        6 => (1280, 720, 60),
        7 => (1920, 1080, 30),
        8 => (1920, 1080, 60),
        10 => (1280, 720, 25),
        11 => (1280, 720, 50),
        12 => (1920, 1080, 25),
        13 => (1920, 1080, 50),
        15 => (1280, 720, 24),
        16 => (1920, 1080, 24),
        _ => return None,
    };
    Some(VideoMode { width, height, fps })
}

pub fn vesa_mode(bit: u32) -> Option<VideoMode> {
    let modes = [
        (800, 600),
        (800, 600),
        (1024, 768),
        (1024, 768),
        (1152, 864),
        (1152, 864),
        (1280, 768),
        (1280, 768),
        (1280, 800),
        (1280, 800),
        (1360, 768),
        (1360, 768),
        (1366, 768),
        (1366, 768),
        (1280, 1024),
        (1280, 1024),
        (1400, 1050),
        (1400, 1050),
        (1440, 900),
        (1440, 900),
        (1600, 900),
        (1600, 900),
        (1600, 1200),
        (1600, 1200),
        (1680, 1050),
        (1680, 1050),
        (1920, 1200),
        (1920, 1200),
    ];
    let (width, height) = *modes.get(bit as usize)?;
    Some(VideoMode {
        width,
        height,
        fps: if bit.is_multiple_of(2) { 30 } else { 60 },
    })
}

pub fn hh_mode(bit: u32) -> Option<VideoMode> {
    let modes = [
        (800, 480),
        (800, 480),
        (854, 480),
        (854, 480),
        (864, 480),
        (864, 480),
        (640, 360),
        (640, 360),
        (960, 540),
        (960, 540),
        (848, 480),
        (848, 480),
    ];
    let (width, height) = *modes.get(bit as usize)?;
    Some(VideoMode {
        width,
        height,
        fps: if bit.is_multiple_of(2) { 30 } else { 60 },
    })
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
    let bad = || CapsError::MalformedVideoFormats(video.to_string());
    let hex32 = |s: &str| u32::from_str_radix(s, 16).ok();
    let hex8 = |s: &str| u8::from_str_radix(s, 16).ok();

    // Native/preferred precede the first H.264 codec tuple. Further tuples are
    // comma-separated and each owns its own profile, level and mode masks.
    let mut video_formats = Vec::new();
    for (index, tuple) in video.split(',').enumerate() {
        let fields: Vec<&str> = tuple.split_whitespace().collect();
        let offset = if index == 0 { 2 } else { 0 };
        if fields.len() < offset + 5 {
            return Err(bad());
        }
        video_formats.push(VideoCodecCaps {
            profile: hex8(fields[offset]).ok_or_else(bad)?,
            level: hex8(fields[offset + 1]).ok_or_else(bad)?,
            cea: hex32(fields[offset + 2]).ok_or_else(bad)?,
            vesa: hex32(fields[offset + 3]).ok_or_else(bad)?,
            hh: hex32(fields[offset + 4]).ok_or_else(bad)?,
        });
    }
    let first = video_formats.first().ok_or_else(bad)?;

    Ok(SinkCaps {
        profile: first.profile,
        level: first.level,
        cea: first.cea,
        vesa: first.vesa,
        hh: first.hh,
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
        video_formats,
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

/// Every progressive mode advertised across the CEA, VESA and handheld tables.
pub fn sink_modes(c: &SinkCaps) -> Vec<VideoMode> {
    codec_formats(c)
        .flat_map(table_modes)
        .map(|(_, _, mode)| mode)
        .collect()
}

fn codec_formats(c: &SinkCaps) -> impl Iterator<Item = VideoCodecCaps> + '_ {
    if c.video_formats.len() <= 1 {
        // Keep the legacy public fields authoritative for callers that adjust
        // a captured capability in tests or policy code.
        EitherFormats::One(std::iter::once(VideoCodecCaps {
            profile: c.profile,
            level: c.level,
            cea: c.cea,
            vesa: c.vesa,
            hh: c.hh,
        }))
    } else {
        EitherFormats::Many(c.video_formats.iter().cloned())
    }
}

enum EitherFormats<A, B> {
    One(A),
    Many(B),
}
impl<T, A: Iterator<Item = T>, B: Iterator<Item = T>> Iterator for EitherFormats<A, B> {
    type Item = T;
    fn next(&mut self) -> Option<T> {
        match self {
            Self::One(i) => i.next(),
            Self::Many(i) => i.next(),
        }
    }
}

fn table_modes(c: VideoCodecCaps) -> std::vec::IntoIter<(VideoTable, u32, VideoMode)> {
    [
        (VideoTable::Cea, c.cea),
        (VideoTable::Vesa, c.vesa),
        (VideoTable::Hh, c.hh),
    ]
    .into_iter()
    .flat_map(|(table, mask)| {
        (0..32).filter_map(move |bit| {
            if mask & (1 << bit) == 0 {
                return None;
            }
            let mode = match table {
                VideoTable::Cea => cea_mode(bit),
                VideoTable::Vesa => vesa_mode(bit),
                VideoTable::Hh => hh_mode(bit),
            }?;
            Some((table, 1 << bit, mode))
        })
    })
    .collect::<Vec<_>>()
    .into_iter()
}

/// Smallest WFD H.264 level that can carry the mode's macroblock rate.
pub fn required_level(mode: VideoMode) -> Option<u8> {
    let mb_per_frame = mode.width.div_ceil(16) * mode.height.div_ceil(16);
    match mb_per_frame * mode.fps {
        0..=108_000 => Some(0x01),       // 3.1
        108_001..=216_000 => Some(0x02), // 3.2
        216_001..=245_760 => Some(0x04), // 4.0
        245_761..=522_240 => Some(0x10), // 4.2
        _ => None,
    }
}

fn level_rank(level: u8) -> Option<u8> {
    [0x01, 0x02, 0x04, 0x08, 0x10]
        .iter()
        .rposition(|b| level & b != 0)
        .map(|n| n as u8)
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
        VideoMode {
            width: 1920,
            height: 1080,
            fps: 60,
        },
        VideoMode {
            width: 1920,
            height: 1080,
            fps: 30,
        },
        VideoMode {
            width: 1280,
            height: 720,
            fps: 60,
        },
        VideoMode {
            width: 1280,
            height: 720,
            fps: 30,
        },
        VideoMode {
            width: 1366,
            height: 768,
            fps: 60,
        },
        VideoMode {
            width: 1366,
            height: 768,
            fps: 30,
        },
        VideoMode {
            width: 1024,
            height: 768,
            fps: 60,
        },
        VideoMode {
            width: 1024,
            height: 768,
            fps: 30,
        },
        VideoMode {
            width: 960,
            height: 540,
            fps: 60,
        },
        VideoMode {
            width: 960,
            height: 540,
            fps: 30,
        },
        VideoMode {
            width: 640,
            height: 480,
            fps: 60,
        },
        VideoMode {
            width: 640,
            height: 360,
            fps: 60,
        },
        VideoMode {
            width: 640,
            height: 360,
            fps: 30,
        },
    ];
    match mode {
        Mode::Quality => all.sort_by(|a, b| b.height.cmp(&a.height).then(b.fps.cmp(&a.fps))),
        Mode::Game => all.sort_by(|a, b| b.fps.cmp(&a.fps).then(b.height.cmp(&a.height))),
    }
    if let Some(ceiling) = ceiling_mbps {
        all.retain(|m| needs_mbps(*m) <= ceiling);
    }
    all
}

/// Our modes in preference order; the first the display also offers wins.
pub fn choose_selection(
    sink: &SinkCaps,
    ours: &[VideoMode],
) -> Result<ModeSelection, NoCommonFormat> {
    for wanted in ours {
        let Some(required) = required_level(*wanted) else {
            continue;
        };
        for codec in codec_formats(sink) {
            let profile = if codec.profile & 0x01 != 0 {
                0x01
            } else if codec.profile & 0x02 != 0 {
                0x02
            } else {
                0
            };
            if profile == 0 || level_rank(required) > level_rank(codec.level) {
                continue;
            }
            if let Some((kind, bit, mode)) = table_modes(codec).find(|(_, _, mode)| mode == wanted)
            {
                return Ok(ModeSelection {
                    mode,
                    table: kind,
                    bit,
                    profile,
                    level: required,
                });
            }
        }
    }
    Err(NoCommonFormat {
        sink_offered: sink_modes(sink),
        we_offered: ours.to_vec(),
    })
}

pub fn choose(sink: &SinkCaps, ours: &[VideoMode]) -> Result<VideoMode, NoCommonFormat> {
    choose_selection(sink, ours).map(|selection| selection.mode)
}

/// The CEA bit standing for a mode, for the body that selects it.
pub fn mode_bit(mode: VideoMode) -> Option<u32> {
    (0..32)
        .find(|b| cea_mode(*b) == Some(mode))
        .map(|b| 1u32 << b)
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
        c.level = 0x10;
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
        assert!(
            text.contains("1280x720"),
            "must name the display's offer: {text}"
        );
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
        assert_eq!(c.content_protection.as_deref(), Some("HDCP2.0 port=1189"));
    }

    #[test]
    fn quality_prefers_a_bigger_picture_and_game_a_faster_one() {
        // The toggle already means exactly this for castr's own protocol.
        let quality = our_modes(Mode::Quality, None);
        let game = our_modes(Mode::Game, None);
        assert_eq!(
            quality[0],
            VideoMode {
                width: 1920,
                height: 1080,
                fps: 60
            }
        );
        assert_eq!(
            quality[1],
            VideoMode {
                width: 1920,
                height: 1080,
                fps: 30
            },
            "quality takes 1080p30 over 720p60"
        );
        assert_eq!(
            game[1],
            VideoMode {
                width: 1366,
                height: 768,
                fps: 60
            },
            "game takes the next-largest 60 fps mode over 1080p30"
        );
    }

    #[test]
    fn a_display_is_never_offered_more_than_it_says_it_can_carry() {
        // Our own Pi advertises 10 Mbit/s. Proposing 1080p60 to it would be
        // asking for a stream it has already said it cannot take.
        let modes = our_modes(Mode::Quality, Some(10));
        assert!(!modes.contains(&VideoMode {
            width: 1920,
            height: 1080,
            fps: 60
        }));
        assert!(modes.contains(&P720P30));
    }

    #[test]
    fn a_generous_ceiling_keeps_everything() {
        // A television advertising 54 Mbit/s should see our whole list.
        assert_eq!(our_modes(Mode::Quality, Some(54)).len(), 13);
        assert_eq!(our_modes(Mode::Quality, None).len(), 13);
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
        assert!((18..=22).contains(&needs_mbps(VideoMode {
            width: 1920,
            height: 1080,
            fps: 60
        })));
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

    #[test]
    fn interlaced_cea_bits_are_not_labeled_progressive() {
        for bit in [2, 4, 9, 14] {
            assert_eq!(cea_mode(bit), None);
        }
    }

    #[test]
    fn a_vesa_mode_keeps_its_table_and_bit() {
        let mut c = parse(&our_sink_body()).unwrap();
        c.cea = 0;
        c.vesa = 1 << 13; // 1366x768p60
        c.level = 0x10;
        let wanted = VideoMode {
            width: 1366,
            height: 768,
            fps: 60,
        };
        let selected = choose_selection(&c, &[wanted]).unwrap();
        assert_eq!(selected.table, VideoTable::Vesa);
        assert_eq!(selected.bit, 1 << 13);
        assert_eq!(selected.level, 0x10);
    }

    #[test]
    fn multiple_codec_tuples_keep_modes_with_their_own_profile() {
        let body = "wfd_video_formats: 40 00 01 02 00000040 00000000 00000000 00 0000 0000 00 none none, 02 04 00000080 00000000 00000000 00 0000 0000 00 none none\r\n\
                    wfd_audio_codecs: LPCM 00000002 00\r\n";
        let c = parse(body).unwrap();
        let selected = choose_selection(
            &c,
            &[VideoMode {
                width: 1920,
                height: 1080,
                fps: 30,
            }],
        )
        .unwrap();
        assert_eq!(selected.profile, 0x02);
        assert_eq!(selected.bit, 0x80);
    }
}
