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
    Ok(Some((
        Message {
            start,
            headers,
            body,
        },
        body_start + body_len,
    )))
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
        return Ok(StartLine::Response {
            status,
            reason: reason.to_string(),
        });
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
        start: StartLine::Response {
            status,
            reason: reason_for(status).to_string(),
        },
        headers: vec![("CSeq".into(), cseq.to_string())],
        body: body.to_string(),
    }
}

pub fn request(method: &str, uri: &str, cseq: u32, body: &str) -> Message {
    Message {
        start: StartLine::Request {
            method: method.into(),
            uri: uri.into(),
        },
        headers: vec![("CSeq".into(), cseq.to_string())],
        body: body.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const M1: &[u8] = b"OPTIONS * RTSP/1.0\r\nCSeq: 1\r\nRequire: org.wfa.wfd1.0\r\n\r\n";

    const M3: &[u8] = b"GET_PARAMETER rtsp://localhost/wfd1.0 RTSP/1.0\r\n\
CSeq: 2\r\n\
Content-Type: text/parameters\r\n\
Content-Length: 59\r\n\
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
        assert_eq!(m.body.len(), 59);
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
        let s = request(
            "SET_PARAMETER",
            "rtsp://192.168.173.2/wfd1.0",
            9,
            "wfd_idr_request\r\n",
        )
        .format();
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
