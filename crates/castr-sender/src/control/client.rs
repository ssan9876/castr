//! Talking to a cast running in another process.
//!
//! Impure, and deliberately thin: what to conclude lives in
//! [`super::record::interpret`], what to say in [`super::wire`].

use super::record::{self, Outcome, Record};
use super::wire::{self, Request, Response};
use std::io::{BufRead, BufReader, Write};
use std::net::{Ipv4Addr, SocketAddr, TcpStream};
use std::path::Path;
use std::time::Duration;

/// Long enough for a busy cast loop to get round to accepting, short enough
/// that a wedged one does not hold the terminal.
const TIMEOUT: Duration = Duration::from_secs(2);

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Report {
    NoCast,
    /// A record left by a cast that is gone. Already removed.
    Stale { started: Option<u64> },
    Answered(Response),
}

/// What is on disk, and whether it answers.
enum Found {
    Nothing,
    /// A record written by a cast that died mid-write.
    Unreadable,
    Record(Record, Option<TcpStream>),
}

fn look(config_dir: &Path) -> Found {
    let Ok(text) = std::fs::read_to_string(record::path(config_dir)) else {
        return Found::Nothing;
    };
    let Ok(rec) = Record::parse(&text) else {
        return Found::Unreadable;
    };
    let addr = SocketAddr::from((Ipv4Addr::LOCALHOST, rec.port));
    let sock = TcpStream::connect_timeout(&addr, TIMEOUT).ok();
    Found::Record(rec, sock)
}

fn discard(config_dir: &Path) {
    let _ = std::fs::remove_file(record::path(config_dir));
}

pub fn talk(config_dir: &Path, req: Request) -> anyhow::Result<Report> {
    let (rec, mut sock) = match look(config_dir) {
        Found::Nothing => return Ok(Report::NoCast),
        // Self-healing is the right default here: the alternative is a file no
        // command will touch and every command trips over.
        Found::Unreadable => {
            discard(config_dir);
            return Ok(Report::Stale { started: None });
        }
        Found::Record(rec, sock) => (rec, sock),
    };

    let sock = match record::interpret(Some(&rec), sock.is_some()) {
        Outcome::Live => sock.take().expect("interpreted as live"),
        Outcome::Stale { started } => {
            discard(config_dir);
            return Ok(Report::Stale {
                started: Some(started),
            });
        }
        Outcome::NoCast => unreachable!("a record was read"),
    };

    let mut sock = sock;
    sock.set_read_timeout(Some(TIMEOUT))?;
    sock.set_write_timeout(Some(TIMEOUT))?;
    sock.write_all(wire::format_request(req, &rec.token).as_bytes())?;
    sock.flush()?;

    let mut line = String::new();
    BufReader::new(&sock).read_line(&mut line)?;
    if line.trim().is_empty() {
        // It accepted the connection and then went away: it was on its way out
        // as we arrived.
        discard(config_dir);
        return Ok(Report::Stale {
            started: Some(rec.started),
        });
    }
    Ok(Report::Answered(wire::parse_response(&line)?))
}

/// The cast currently running, if one is. Removes a stale record as a side
/// effect, so the next `miracast-cast` is not blocked by a dead one.
pub fn running(config_dir: &Path) -> Option<Record> {
    match look(config_dir) {
        Found::Nothing => None,
        Found::Unreadable => {
            discard(config_dir);
            None
        }
        Found::Record(rec, sock) => match record::interpret(Some(&rec), sock.is_some()) {
            Outcome::Live => Some(rec),
            _ => {
                discard(config_dir);
                None
            }
        },
    }
}
