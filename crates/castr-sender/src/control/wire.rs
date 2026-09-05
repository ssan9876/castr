//! The control protocol: one line each way.
//!
//! Pure. Text rather than a serialization format because the payload is a
//! dozen scalars and a protocol a person can type while debugging a cast is
//! worth more here than one that is convenient to derive.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Request {
    Stop,
    Status,
}

impl Request {
    fn verb(self) -> &'static str {
        match self {
            Request::Stop => "STOP",
            Request::Status => "STATUS",
        }
    }
}

/// Why a request was not carried out. Both are reported to the client; neither
/// is a reason to stop serving.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Denial {
    Unauthorised,
    BadRequest,
}

impl Denial {
    pub fn line(self) -> String {
        match self {
            Denial::Unauthorised => "ERR unauthorised\n".into(),
            Denial::BadRequest => "ERR bad request\n".into(),
        }
    }
}

pub fn format_request(req: Request, token: &str) -> String {
    format!("{} {}\n", req.verb(), token)
}

/// Reads a request, checking the token against the one in our own record.
///
/// The verb is recognised before the token is checked, so an unparseable line
/// is reported as such rather than as a refusal — telling a caller "bad
/// request" when they typed `STOPP` is more useful than telling them their
/// token is wrong.
pub fn parse_request(line: &str, token: &str) -> Result<Request, Denial> {
    let mut parts = line.split_whitespace();
    let req = match parts.next() {
        Some("STOP") => Request::Stop,
        Some("STATUS") => Request::Status,
        _ => return Err(Denial::BadRequest),
    };
    match parts.next() {
        Some(t) if t == token => Ok(req),
        _ => Err(Denial::Unauthorised),
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Response {
    Ok(String),
    Err(String),
}

pub fn format_ok(body: &str) -> String {
    format!("OK {body}\n")
}

pub fn parse_response(line: &str) -> anyhow::Result<Response> {
    let line = line.trim_end_matches(['\r', '\n']);
    match line.split_once(' ') {
        Some(("OK", rest)) => Ok(Response::Ok(rest.to_string())),
        Some(("ERR", rest)) => Ok(Response::Err(rest.to_string())),
        _ if line == "OK" => Ok(Response::Ok(String::new())),
        _ => anyhow::bail!("the running cast answered something unreadable: {line:?}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const TOKEN: &str = "8f3a1c";

    #[test]
    fn a_request_survives_a_round_trip() {
        for req in [Request::Stop, Request::Status] {
            let line = format_request(req, TOKEN);
            assert_eq!(parse_request(&line, TOKEN), Ok(req));
        }
    }

    #[test]
    fn the_wrong_token_is_refused() {
        let line = format_request(Request::Stop, "not-the-token");
        assert_eq!(parse_request(&line, TOKEN), Err(Denial::Unauthorised));
    }

    #[test]
    fn a_request_with_no_token_is_refused() {
        assert_eq!(parse_request("STOP\n", TOKEN), Err(Denial::Unauthorised));
    }

    #[test]
    fn an_unknown_verb_is_a_bad_request_not_a_refusal() {
        assert_eq!(parse_request("STOPP abc\n", TOKEN), Err(Denial::BadRequest));
        assert_eq!(parse_request("", TOKEN), Err(Denial::BadRequest));
    }

    #[test]
    fn a_verb_in_the_wrong_case_is_not_accepted() {
        // Deliberate: one spelling, so a client that works once works always.
        assert_eq!(parse_request("stop 8f3a1c", TOKEN), Err(Denial::BadRequest));
    }

    #[test]
    fn a_truncated_line_does_not_panic() {
        for line in ["S", " ", "\n", "\r\n", "STATUS", "STATUS "] {
            assert!(parse_request(line, TOKEN).is_err());
        }
    }

    #[test]
    fn carriage_returns_are_tolerated() {
        assert_eq!(parse_request("STOP 8f3a1c\r\n", TOKEN), Ok(Request::Stop));
    }

    #[test]
    fn responses_round_trip() {
        assert_eq!(
            parse_response(&format_ok("stopping")).unwrap(),
            Response::Ok("stopping".into())
        );
        assert_eq!(
            parse_response(&Denial::Unauthorised.line()).unwrap(),
            Response::Err("unauthorised".into())
        );
    }

    #[test]
    fn a_status_body_with_spaces_in_it_stays_one_value() {
        // Field values are tab-separated precisely so a display name with
        // spaces cannot be mistaken for the next field.
        let body = "display=75\" Crystal UHD\tmbps=8.4";
        let Response::Ok(got) = parse_response(&format_ok(body)).unwrap() else {
            panic!("expected OK");
        };
        assert_eq!(got, body);
        assert_eq!(got.split('\t').count(), 2);
    }

    #[test]
    fn an_unreadable_response_is_an_error() {
        assert!(parse_response("WAT something").is_err());
        assert!(parse_response("").is_err());
    }
}
