//! The cast that is currently running, whichever kind it is.
//!
//! One interface over two genuinely different lifecycles: castr's cast is an
//! async task driven by tokio channels, a Miracast cast is a blocking loop on
//! its own thread. The window only ever needs to stop one and describe one.
//!
//! The Miracast half reuses the control channel built for `miracast-stop`:
//! the same `Command::Stop`, the same published snapshot. A cast started from
//! this window is therefore also visible to `miracast-status` and stoppable by
//! `miracast-stop`, and the one-cast-at-a-time rule covers window and terminal
//! together rather than each on its own.

use crate::cast::{CastCommand, CastStatus};
use crate::control::server::{Command as MiracastCommand, Published};
use crate::control::stats::Snapshot;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::{mpsc, watch};

/// How long a Miracast worker is given to tear down when the window closes.
/// A display left believing a session is live can refuse the next one, so this
/// waits rather than abandoning the thread — but not for ever.
const SHUTDOWN_GRACE: Duration = Duration::from_secs(5);

/// What a Miracast run is doing before, during and after it plays.
#[derive(Debug, Default, Clone)]
pub struct RunState {
    /// What is happening now, in the stage vocabulary the CLI reports in.
    pub stage: String,
    pub error: Option<String>,
    pub finished: bool,
}

pub enum Session {
    Castr {
        cmd: mpsc::Sender<CastCommand>,
        status: watch::Receiver<CastStatus>,
    },
    Miracast {
        cmd: std::sync::mpsc::Sender<MiracastCommand>,
        published: Published,
        state: Arc<Mutex<RunState>>,
        worker: Option<std::thread::JoinHandle<()>>,
    },
}

impl Session {
    pub fn stop(&self) {
        match self {
            Session::Castr { cmd, .. } => {
                let _ = cmd.try_send(CastCommand::Stop);
            }
            Session::Miracast { cmd, .. } => {
                let _ = cmd.send(MiracastCommand::Stop);
            }
        }
    }

    /// Whether it has ended on its own and the window should go back to idle.
    pub fn finished(&self) -> bool {
        match self {
            Session::Castr { status, .. } => {
                let s = status.borrow();
                s.state == "stopped" || s.state == "failed"
            }
            Session::Miracast { state, .. } => {
                state.lock().map(|s| s.finished).unwrap_or(true)
            }
        }
    }

    /// Anything that went wrong, once it has.
    pub fn error(&self) -> Option<String> {
        match self {
            Session::Castr { .. } => None,
            Session::Miracast { state, .. } => {
                state.lock().ok().and_then(|s| s.error.clone())
            }
        }
    }

    /// The line under the Stop button.
    pub fn line(&self) -> String {
        match self {
            Session::Castr { status, .. } => castr_line(&status.borrow()),
            Session::Miracast {
                published, state, ..
            } => {
                let snapshot = published.lock().ok().and_then(|g| g.clone());
                let stage = state
                    .lock()
                    .map(|s| s.stage.clone())
                    .unwrap_or_default();
                match snapshot {
                    Some(snap) => miracast_line(&snap),
                    None => stage,
                }
            }
        }
    }

    /// Stop, and wait for the teardown to actually happen.
    ///
    /// Called when the window closes. Closing the window is the likeliest way
    /// to leave a display believing a session is live - the same defect class
    /// as a Ctrl-C that skipped teardown - so this joins rather than dropping
    /// the worker and hoping.
    pub fn shutdown(&mut self) {
        self.stop();
        if let Session::Miracast { worker, .. } = self {
            if let Some(handle) = worker.take() {
                let deadline = std::time::Instant::now() + SHUTDOWN_GRACE;
                while !handle.is_finished() && std::time::Instant::now() < deadline {
                    std::thread::sleep(Duration::from_millis(50));
                }
                if handle.is_finished() {
                    let _ = handle.join();
                } else {
                    tracing::warn!(
                        "miracast: the cast did not finish tearing down within {SHUTDOWN_GRACE:?}; \
                         the display may hold the session until it times out"
                    );
                }
            }
        }
    }
}

/// castr's own protocol has receiver statistics, so it reports them.
pub fn castr_line(s: &CastStatus) -> String {
    format!(
        "{}  {}x{}  {:.1} Mbps  rtt {} ms  loss {:.1}%  {:.0} fps",
        s.state,
        s.width,
        s.height,
        s.bitrate_bps as f64 / 1e6,
        s.rtt_ms,
        s.loss_pct,
        s.fps
    )
}

/// Wi-Fi Display has no back-channel, so there is no round trip time and no
/// loss figure to report - and none is shown, rather than showing two fields
/// that would always read zero.
pub fn miracast_line(s: &Snapshot) -> String {
    let mode = s.mode.clone().unwrap_or_else(|| "negotiating".into());
    let keepalive = match s.keepalive_age_s {
        Some(age) => format!("answered {age}s ago"),
        None => "no reply yet".into(),
    };
    format!(
        "casting to {}  {}  {:.1} Mbps sent  {} repeated  {}",
        s.display, mode, s.mbps, s.repeated_frames, keepalive
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snapshot() -> Snapshot {
        Snapshot {
            display: "DietPi".into(),
            address: "192.168.173.1:7236".into(),
            mode: Some("1280x720@30".into()),
            ceiling_mbps: Some(10),
            mbps: 10.4,
            video_units: 202,
            audio_units: 718,
            datagrams: 6979,
            bytes: 8_825_560,
            repeated_frames: 3,
            elapsed_s: 7,
            keepalive_age_s: Some(2),
        }
    }

    #[test]
    fn a_castr_cast_reports_what_the_receiver_told_it() {
        let s = CastStatus {
            state: "casting".into(),
            width: 1920,
            height: 1080,
            bitrate_bps: 12_000_000,
            rtt_ms: 4,
            loss_pct: 0.5,
            fps: 60.0,
        };
        let line = castr_line(&s);
        assert!(line.contains("rtt 4 ms"), "got {line}");
        assert!(line.contains("loss 0.5%"), "got {line}");
    }

    #[test]
    fn a_miracast_cast_never_shows_rtt_or_loss() {
        // Wi-Fi Display gives a source no receiver statistics at all. Showing
        // the fields blank would read as a perfect link rather than an
        // unmeasured one.
        let line = miracast_line(&snapshot());
        assert!(!line.contains("rtt"), "got {line}");
        assert!(!line.contains("loss"), "got {line}");
    }

    #[test]
    fn a_miracast_cast_reports_what_it_does_know() {
        let line = miracast_line(&snapshot());
        assert!(line.contains("DietPi"), "got {line}");
        assert!(line.contains("1280x720@30"), "got {line}");
        assert!(line.contains("10.4 Mbps sent"), "got {line}");
        assert!(line.contains("3 repeated"), "got {line}");
        assert!(line.contains("answered 2s ago"), "got {line}");
    }

    #[test]
    fn throughput_is_labelled_as_sent_rather_than_received() {
        // The one word that keeps the reader honest about which end measured it.
        assert!(miracast_line(&snapshot()).contains("Mbps sent"));
    }

    #[test]
    fn before_a_mode_is_chosen_the_line_says_negotiating() {
        let s = Snapshot {
            mode: None,
            ..snapshot()
        };
        assert!(miracast_line(&s).contains("negotiating"));
    }

    #[test]
    fn before_the_display_answers_the_line_says_so() {
        let s = Snapshot {
            keepalive_age_s: None,
            ..snapshot()
        };
        assert!(miracast_line(&s).contains("no reply yet"));
    }
}
