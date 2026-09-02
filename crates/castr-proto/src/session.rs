use crate::control::*;

pub const TOKEN_TTL_US: u64 = 60_000_000;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReceiverState {
    AwaitingHello,
    Streaming { params: Option<StreamParams> },
    Disconnected { since_us: u64 },
    Closed,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    Send(ControlMessage),
    Resumed,
    Fail(String),
}

pub struct ReceiverSession {
    name: String,
    caps: Capabilities,
    token: [u8; 16],
    state: ReceiverState,
    params: Option<StreamParams>,
}

impl ReceiverSession {
    pub fn new(name: String, caps: Capabilities, token: [u8; 16]) -> Self {
        Self {
            name,
            caps,
            token,
            state: ReceiverState::AwaitingHello,
            params: None,
        }
    }

    pub fn state(&self) -> &ReceiverState {
        &self.state
    }
    pub fn token(&self) -> [u8; 16] {
        self.token
    }
    pub fn params(&self) -> Option<&StreamParams> {
        self.params.as_ref()
    }

    fn ack(&self) -> ControlMessage {
        ControlMessage::HelloAck {
            name: self.name.clone(),
            caps: self.caps.clone(),
        }
    }

    pub fn on_disconnect(&mut self, now_us: u64) {
        if !matches!(self.state, ReceiverState::Closed) {
            self.state = ReceiverState::Disconnected { since_us: now_us };
        }
    }

    pub fn on_message(&mut self, msg: ControlMessage, now_us: u64) -> Vec<Action> {
        match (&self.state, msg) {
            (_, ControlMessage::Hello { version, .. }) if version != PROTOCOL_VERSION => {
                self.state = ReceiverState::Closed;
                vec![
                    Action::Send(ControlMessage::Error {
                        code: 2,
                        message: format!("unsupported protocol version {version}"),
                    }),
                    Action::Fail("version mismatch".into()),
                ]
            }
            (ReceiverState::AwaitingHello, ControlMessage::Hello { .. }) => {
                self.state = ReceiverState::Streaming { params: None };
                vec![
                    Action::Send(self.ack()),
                    Action::Send(ControlMessage::SessionToken(self.token)),
                ]
            }
            (
                ReceiverState::Disconnected { since_us },
                ControlMessage::Hello { resume_token, .. },
            ) => {
                let since = *since_us;
                let fresh = now_us.saturating_sub(since) <= TOKEN_TTL_US;
                if resume_token == Some(self.token) && fresh {
                    self.state = ReceiverState::Streaming {
                        params: self.params.clone(),
                    };
                    vec![Action::Send(self.ack()), Action::Resumed]
                } else {
                    self.state = ReceiverState::Closed;
                    vec![
                        Action::Send(ControlMessage::Error {
                            code: 1,
                            message: "invalid or expired session token".into(),
                        }),
                        Action::Fail("bad resume token".into()),
                    ]
                }
            }
            (ReceiverState::Streaming { .. }, ControlMessage::StartStream(p)) => {
                self.params = Some(p.clone());
                self.state = ReceiverState::Streaming { params: Some(p) };
                vec![]
            }
            (ReceiverState::Streaming { .. }, ControlMessage::SetMode(m)) => {
                if let Some(p) = self.params.as_mut() {
                    p.mode = m;
                }
                vec![]
            }
            (_, ControlMessage::Goodbye { .. }) => {
                self.state = ReceiverState::Closed;
                vec![]
            }
            _ => vec![],
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::control::*;

    fn caps() -> Capabilities {
        Capabilities {
            max_width: 1920,
            max_height: 1080,
            max_fps: 60,
            max_bitrate_bps: 40_000_000,
            codecs: vec![Codec::H264],
            audio: true,
        }
    }
    fn params() -> StreamParams {
        StreamParams {
            codec: Codec::H264,
            width: 1280,
            height: 720,
            fps: 30,
            mode: Mode::Game,
            bitrate_bps: 5_000_000,
        }
    }
    fn hello(token: Option<[u8; 16]>) -> ControlMessage {
        ControlMessage::Hello {
            version: PROTOCOL_VERSION,
            name: "pc".into(),
            resume_token: token,
        }
    }

    #[test]
    fn fresh_hello_gets_ack_and_token() {
        let mut s = ReceiverSession::new("pi".into(), caps(), [1u8; 16]);
        let actions = s.on_message(hello(None), 0);
        assert_eq!(
            actions,
            vec![
                Action::Send(ControlMessage::HelloAck {
                    name: "pi".into(),
                    caps: caps()
                }),
                Action::Send(ControlMessage::SessionToken([1u8; 16])),
            ]
        );
        assert_eq!(s.state(), &ReceiverState::Streaming { params: None });
        assert!(s
            .on_message(ControlMessage::StartStream(params()), 0)
            .is_empty());
        assert_eq!(s.params(), Some(&params()));
    }

    #[test]
    fn wrong_version_is_rejected() {
        let mut s = ReceiverSession::new("pi".into(), caps(), [1u8; 16]);
        let actions = s.on_message(
            ControlMessage::Hello {
                version: 99,
                name: "pc".into(),
                resume_token: None,
            },
            0,
        );
        assert!(matches!(
            actions[0],
            Action::Send(ControlMessage::Error { code: 2, .. })
        ));
        assert!(matches!(actions[1], Action::Fail(_)));
    }

    #[test]
    fn resume_with_valid_token_keeps_params() {
        let mut s = ReceiverSession::new("pi".into(), caps(), [7u8; 16]);
        s.on_message(hello(None), 0);
        s.on_message(ControlMessage::StartStream(params()), 0);
        s.on_disconnect(10_000_000);
        assert_eq!(
            s.state(),
            &ReceiverState::Disconnected {
                since_us: 10_000_000
            }
        );
        let actions = s.on_message(hello(Some([7u8; 16])), 20_000_000);
        assert_eq!(
            actions,
            vec![
                Action::Send(ControlMessage::HelloAck {
                    name: "pi".into(),
                    caps: caps()
                }),
                Action::Resumed,
            ]
        );
        assert_eq!(s.params(), Some(&params()));
        assert!(matches!(s.state(), ReceiverState::Streaming { .. }));
    }

    #[test]
    fn resume_with_expired_token_fails() {
        let mut s = ReceiverSession::new("pi".into(), caps(), [7u8; 16]);
        s.on_message(hello(None), 0);
        s.on_disconnect(10_000_000);
        let actions = s.on_message(hello(Some([7u8; 16])), 10_000_000 + TOKEN_TTL_US + 1);
        assert!(matches!(
            actions[0],
            Action::Send(ControlMessage::Error { code: 1, .. })
        ));
        assert!(matches!(actions[1], Action::Fail(_)));
    }

    #[test]
    fn resume_with_wrong_token_fails() {
        let mut s = ReceiverSession::new("pi".into(), caps(), [7u8; 16]);
        s.on_message(hello(None), 0);
        s.on_disconnect(0);
        let actions = s.on_message(hello(Some([8u8; 16])), 1);
        assert!(matches!(actions[1], Action::Fail(_)));
    }

    #[test]
    fn hello_with_token_while_awaiting_is_treated_as_fresh() {
        let mut s = ReceiverSession::new("pi".into(), caps(), [7u8; 16]);
        let actions = s.on_message(hello(Some([9u8; 16])), 0);
        assert_eq!(actions.len(), 2);
        assert!(matches!(
            actions[1],
            Action::Send(ControlMessage::SessionToken(_))
        ));
    }

    #[test]
    fn goodbye_closes() {
        let mut s = ReceiverSession::new("pi".into(), caps(), [7u8; 16]);
        s.on_message(hello(None), 0);
        s.on_message(
            ControlMessage::Goodbye {
                reason: "done".into(),
            },
            0,
        );
        assert_eq!(s.state(), &ReceiverState::Closed);
    }
}
