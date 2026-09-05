//! The control endpoint a running cast serves.
//!
//! Impure. The listener binds loopback on an ephemeral port and writes a
//! record so another process can find it; the record is removed on drop,
//! exactly as `Connection` owns the Wi-Fi Direct group in the radio layer.
//!
//! **A cast must never die because its control channel did.** Nothing in here
//! returns an error the caller is expected to treat as fatal, and
//! [`ControlServer::start`] failing means the cast runs without a control
//! channel, not that it does not run.

use super::record::{self, Record};
use super::stats::Snapshot;
use super::wire::{self, Denial, Request};
use anyhow::Context as _;
use std::io::{BufRead, BufReader, Write};
use std::net::{Ipv4Addr, SocketAddr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::time::Duration;

/// What a client can ask a running cast to do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Command {
    Stop,
}

/// The snapshot the cast loop keeps current for the listener to read.
pub type Published = Arc<Mutex<Option<Snapshot>>>;

/// A client that connects and then says nothing must not hold up the next one.
const CLIENT_TIMEOUT: Duration = Duration::from_secs(5);

pub struct ControlServer {
    path: PathBuf,
    port: u16,
    shutdown: Arc<AtomicBool>,
}

impl ControlServer {
    /// Binds, writes the record, and serves until dropped.
    pub fn start(
        config_dir: &Path,
        display: &str,
        address: &str,
        cmds: mpsc::Sender<Command>,
        published: Published,
    ) -> anyhow::Result<Self> {
        let listener = TcpListener::bind(SocketAddr::from((Ipv4Addr::LOCALHOST, 0)))
            .context("binding the control socket")?;
        let port = listener.local_addr()?.port();
        let token = mint_token();

        let rec = Record {
            pid: std::process::id(),
            port,
            token: token.clone(),
            display: display.to_string(),
            address: address.to_string(),
            started: unix_now(),
        };
        let path = record::path(config_dir);
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        std::fs::write(&path, rec.to_toml()).with_context(|| {
            format!("writing the cast record to {}", path.display())
        })?;

        let shutdown = Arc::new(AtomicBool::new(false));
        let stop = shutdown.clone();
        let _ = std::thread::Builder::new()
            .name("miracast-control".into())
            .spawn(move || {
                for stream in listener.incoming() {
                    if stop.load(Ordering::SeqCst) {
                        break;
                    }
                    match stream {
                        Ok(s) => serve(s, &token, &cmds, &published),
                        Err(e) => {
                            tracing::debug!("miracast: control accept: {e:#}");
                        }
                    }
                }
            });

        tracing::info!("miracast: control on 127.0.0.1:{port}");
        Ok(Self {
            path,
            port,
            shutdown,
        })
    }
}

impl Drop for ControlServer {
    fn drop(&mut self) {
        // The record goes first: a client arriving during teardown should see
        // nothing rather than a port about to close.
        let _ = std::fs::remove_file(&self.path);
        self.shutdown.store(true, Ordering::SeqCst);
        // Wake the thread out of `accept`, which is otherwise blocked for as
        // long as the process lives.
        let addr = SocketAddr::from((Ipv4Addr::LOCALHOST, self.port));
        let _ = TcpStream::connect_timeout(&addr, Duration::from_millis(200));
    }
}

fn serve(mut sock: TcpStream, token: &str, cmds: &mpsc::Sender<Command>, published: &Published) {
    let _ = sock.set_read_timeout(Some(CLIENT_TIMEOUT));
    let _ = sock.set_write_timeout(Some(CLIENT_TIMEOUT));

    let mut line = String::new();
    if BufReader::new(&sock).read_line(&mut line).is_err() {
        return;
    }

    let reply = match wire::parse_request(&line, token) {
        Err(denial) => denial.line(),
        Ok(Request::Stop) => {
            // If the loop is already gone the cast is ending anyway, so this
            // is still the truth.
            let _ = cmds.send(Command::Stop);
            wire::format_ok("stopping")
        }
        Ok(Request::Status) => match published.lock() {
            Ok(guard) => match guard.as_ref() {
                Some(snap) => wire::format_ok(&snap.to_fields()),
                None => wire::format_ok("mode=-\tmbps=0.0"),
            },
            // A poisoned lock means the cast loop panicked. Say so rather than
            // panicking in sympathy and taking the listener down too.
            Err(_) => Denial::BadRequest.line(),
        },
    };
    let _ = sock.write_all(reply.as_bytes());
    let _ = sock.flush();
}

/// 128 bits. Not a secret against a determined local attacker — anyone who can
/// read the record can read the token — but enough that an unrelated process
/// cannot stop a cast by connecting to a loopback port and guessing.
fn mint_token() -> String {
    use rand::RngCore;
    let mut bytes = [0u8; 16];
    rand::thread_rng().fill_bytes(&mut bytes);
    hex::encode(bytes)
}

fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::control::client::{self, Report};
    use crate::control::record::Record;
    use crate::control::stats::{Context, Stats};
    use crate::control::wire::Response;
    use std::sync::atomic::AtomicU32;
    use std::time::Instant;

    /// A directory of our own, so tests do not tread on each other or on the
    /// real record in the user's configuration directory.
    fn temp_dir() -> PathBuf {
        static N: AtomicU32 = AtomicU32::new(0);
        let dir = std::env::temp_dir().join(format!(
            "castr-control-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::SeqCst)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn a_snapshot() -> Snapshot {
        let t0 = Instant::now();
        Stats::new().snapshot(
            t0,
            t0,
            &Context {
                display: "DietPi".into(),
                address: "192.168.173.1:7236".into(),
                mode: Some("1280x720@30".into()),
                ceiling_mbps: Some(10),
                last_heard: Some(t0),
            },
        )
    }

    fn start(dir: &Path) -> (ControlServer, mpsc::Receiver<Command>, Published) {
        let (tx, rx) = mpsc::channel();
        let published: Published = Arc::new(Mutex::new(Some(a_snapshot())));
        let server =
            ControlServer::start(dir, "DietPi", "192.168.173.1:7236", tx, published.clone())
                .unwrap();
        (server, rx, published)
    }

    #[test]
    fn stop_reaches_the_cast_loop() {
        let dir = temp_dir();
        let (_server, rx, _pub) = start(&dir);

        let report = client::talk(&dir, Request::Stop).unwrap();
        assert_eq!(report, Report::Answered(Response::Ok("stopping".into())));
        assert_eq!(rx.recv_timeout(Duration::from_secs(2)), Ok(Command::Stop));
    }

    #[test]
    fn status_returns_what_the_loop_published() {
        let dir = temp_dir();
        let (_server, _rx, _pub) = start(&dir);

        let Report::Answered(Response::Ok(body)) = client::talk(&dir, Request::Status).unwrap()
        else {
            panic!("expected a status body");
        };
        let fields = crate::control::stats::fields(&body);
        assert_eq!(fields.iter().find(|(k, _)| *k == "display").unwrap().1, "DietPi");
        assert_eq!(
            fields.iter().find(|(k, _)| *k == "mode").unwrap().1,
            "1280x720@30"
        );
    }

    #[test]
    fn status_before_the_first_snapshot_still_answers() {
        // A status asked during negotiation, before anything has been sent.
        let dir = temp_dir();
        let (tx, _rx) = mpsc::channel();
        let published: Published = Arc::new(Mutex::new(None));
        let _server =
            ControlServer::start(&dir, "DietPi", "1.2.3.4:7236", tx, published).unwrap();

        let Report::Answered(Response::Ok(body)) = client::talk(&dir, Request::Status).unwrap()
        else {
            panic!("expected a status body");
        };
        assert!(body.contains("mode=-"), "got {body:?}");
    }

    #[test]
    fn a_forged_token_is_refused_and_stops_nothing() {
        let dir = temp_dir();
        let (_server, rx, _pub) = start(&dir);

        // A client that read the port but not the token.
        let rec = Record::parse(&std::fs::read_to_string(record::path(&dir)).unwrap()).unwrap();
        let addr = SocketAddr::from((Ipv4Addr::LOCALHOST, rec.port));
        let mut sock = TcpStream::connect(addr).unwrap();
        sock.write_all(b"STOP not-the-token\n").unwrap();
        let mut line = String::new();
        BufReader::new(&sock).read_line(&mut line).unwrap();

        assert_eq!(line.trim(), "ERR unauthorised");
        assert!(rx.recv_timeout(Duration::from_millis(300)).is_err());
    }

    #[test]
    fn a_running_cast_is_found_by_name() {
        let dir = temp_dir();
        let (_server, _rx, _pub) = start(&dir);
        assert_eq!(client::running(&dir).unwrap().display, "DietPi");
    }

    #[test]
    fn no_record_means_no_cast() {
        let dir = temp_dir();
        assert_eq!(client::talk(&dir, Request::Status).unwrap(), Report::NoCast);
        assert!(client::running(&dir).is_none());
    }

    #[test]
    fn a_record_left_by_a_dead_cast_is_reported_stale_and_removed() {
        let dir = temp_dir();
        // Port 1 on loopback: nothing is listening, and binding it needs
        // privilege, so nothing will be.
        let rec = Record {
            pid: 999_999,
            port: 1,
            token: "abc".into(),
            display: "DietPi".into(),
            address: "192.168.173.1:7236".into(),
            started: 1_757_001_234,
        };
        std::fs::write(record::path(&dir), rec.to_toml()).unwrap();

        assert_eq!(
            client::talk(&dir, Request::Status).unwrap(),
            Report::Stale {
                started: Some(1_757_001_234)
            }
        );
        assert!(!record::path(&dir).exists(), "the stale record should be gone");
        assert!(client::running(&dir).is_none());
    }

    #[test]
    fn an_unreadable_record_is_cleaned_up_rather_than_wedging_every_command() {
        let dir = temp_dir();
        std::fs::write(record::path(&dir), "pid = 12345\nport = 5432").unwrap();

        assert_eq!(
            client::talk(&dir, Request::Status).unwrap(),
            Report::Stale { started: None }
        );
        assert!(!record::path(&dir).exists());
    }

    #[test]
    fn dropping_the_server_removes_the_record() {
        let dir = temp_dir();
        let (server, _rx, _pub) = start(&dir);
        assert!(record::path(&dir).exists());
        drop(server);
        assert!(!record::path(&dir).exists());
        assert_eq!(client::talk(&dir, Request::Status).unwrap(), Report::NoCast);
    }

    #[test]
    fn two_requests_in_a_row_are_both_served() {
        // The listener handles one connection at a time; a second client must
        // not find the door shut.
        let dir = temp_dir();
        let (_server, rx, _pub) = start(&dir);
        assert!(matches!(
            client::talk(&dir, Request::Status).unwrap(),
            Report::Answered(Response::Ok(_))
        ));
        assert!(matches!(
            client::talk(&dir, Request::Stop).unwrap(),
            Report::Answered(Response::Ok(_))
        ));
        assert_eq!(rx.recv_timeout(Duration::from_secs(2)), Ok(Command::Stop));
    }

    #[test]
    fn a_client_that_says_nothing_does_not_block_the_next_one() {
        let dir = temp_dir();
        let (_server, _rx, _pub) = start(&dir);
        let rec = Record::parse(&std::fs::read_to_string(record::path(&dir)).unwrap()).unwrap();
        let addr = SocketAddr::from((Ipv4Addr::LOCALHOST, rec.port));

        // Connect and say nothing, then leave. The listener's read timeout is
        // what keeps the accept loop moving.
        let silent = TcpStream::connect(addr).unwrap();
        drop(silent);

        assert!(matches!(
            client::talk(&dir, Request::Status).unwrap(),
            Report::Answered(Response::Ok(_))
        ));
    }
}
