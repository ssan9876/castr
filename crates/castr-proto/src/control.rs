use serde::{Deserialize, Serialize};

pub const PROTOCOL_VERSION: u16 = 1;
pub const MAX_CONTROL_FRAME: usize = 64 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Mode {
    Game,
    Quality,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Codec {
    H264,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Capabilities {
    pub max_width: u32,
    pub max_height: u32,
    pub max_fps: u32,
    pub max_bitrate_bps: u32,
    pub codecs: Vec<Codec>,
    pub audio: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StreamParams {
    pub codec: Codec,
    pub width: u32,
    pub height: u32,
    pub fps: u32,
    pub mode: Mode,
    pub bitrate_bps: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct Stats {
    pub frames_received: u32,
    pub frames_dropped: u32,
    pub fragments_lost: u32,
    pub fragments_received: u32,
    pub decode_queue_depth: u32,
    pub interval_ms: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ControlMessage {
    Hello {
        version: u16,
        name: String,
        resume_token: Option<[u8; 16]>,
    },
    HelloAck {
        name: String,
        caps: Capabilities,
    },
    StartStream(StreamParams),
    SessionToken([u8; 16]),
    SetMode(Mode),
    RequestKeyframe,
    Stats(Stats),
    PairInit(Vec<u8>),
    PairResp(Vec<u8>),
    PairProof([u8; 32]),
    PairOk,
    Error {
        code: u16,
        message: String,
    },
    Goodbye {
        reason: String,
    },
}

#[derive(Debug, thiserror::Error)]
pub enum ControlError {
    #[error("frame exceeds {MAX_CONTROL_FRAME} bytes")]
    TooLarge,
    #[error("decode: {0}")]
    Postcard(#[from] postcard::Error),
}

/// Generic 4-byte LE length prefix + postcard body. Used for control messages and NACKs.
pub fn encode_len_prefixed<T: Serialize>(v: &T) -> Vec<u8> {
    let body = postcard::to_allocvec(v).expect("postcard encode cannot fail for these types");
    let mut out = Vec::with_capacity(4 + body.len());
    out.extend_from_slice(&(body.len() as u32).to_le_bytes());
    out.extend_from_slice(&body);
    out
}

pub fn decode_len_prefixed<T: serde::de::DeserializeOwned>(
    buf: &[u8],
) -> Result<Option<(T, usize)>, ControlError> {
    if buf.len() < 4 {
        return Ok(None);
    }
    let len = u32::from_le_bytes(buf[0..4].try_into().unwrap()) as usize;
    if len > MAX_CONTROL_FRAME {
        return Err(ControlError::TooLarge);
    }
    if buf.len() < 4 + len {
        return Ok(None);
    }
    let v: T = postcard::from_bytes(&buf[4..4 + len])?;
    Ok(Some((v, 4 + len)))
}

pub fn encode_frame(msg: &ControlMessage) -> Vec<u8> {
    encode_len_prefixed(msg)
}

pub fn decode_frame(buf: &[u8]) -> Result<Option<(ControlMessage, usize)>, ControlError> {
    decode_len_prefixed(buf)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_hello_with_token() {
        let m = ControlMessage::Hello {
            version: PROTOCOL_VERSION,
            name: "pc".into(),
            resume_token: Some([9u8; 16]),
        };
        let bytes = encode_frame(&m);
        let len = u32::from_le_bytes(bytes[0..4].try_into().unwrap()) as usize;
        assert_eq!(len + 4, bytes.len());
        let (decoded, used) = decode_frame(&bytes).unwrap().unwrap();
        assert_eq!(decoded, m);
        assert_eq!(used, bytes.len());
    }

    #[test]
    fn partial_buffer_returns_none() {
        let bytes = encode_frame(&ControlMessage::RequestKeyframe);
        assert!(decode_frame(&bytes[..2]).unwrap().is_none());
        assert!(decode_frame(&bytes[..bytes.len() - 1]).unwrap().is_none());
    }

    #[test]
    fn two_frames_back_to_back_decode_sequentially() {
        let mut bytes = encode_frame(&ControlMessage::SetMode(Mode::Game));
        bytes.extend(encode_frame(&ControlMessage::Goodbye {
            reason: "bye".into(),
        }));
        let (a, used) = decode_frame(&bytes).unwrap().unwrap();
        assert_eq!(a, ControlMessage::SetMode(Mode::Game));
        let (b, used2) = decode_frame(&bytes[used..]).unwrap().unwrap();
        assert_eq!(
            b,
            ControlMessage::Goodbye {
                reason: "bye".into()
            }
        );
        assert_eq!(used + used2, bytes.len());
    }

    #[test]
    fn oversized_length_is_rejected() {
        let mut bytes = ((MAX_CONTROL_FRAME + 1) as u32).to_le_bytes().to_vec();
        bytes.push(0);
        assert!(matches!(decode_frame(&bytes), Err(ControlError::TooLarge)));
    }

    #[test]
    fn stats_and_caps_round_trip() {
        let m = ControlMessage::HelloAck {
            name: "pi".into(),
            caps: Capabilities {
                max_width: 1920,
                max_height: 1080,
                max_fps: 30,
                max_bitrate_bps: 10_000_000,
                codecs: vec![Codec::H264],
                audio: true,
            },
        };
        let (d, _) = decode_frame(&encode_frame(&m)).unwrap().unwrap();
        assert_eq!(d, m);
    }
}
