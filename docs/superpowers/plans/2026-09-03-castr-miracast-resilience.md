# Miracast Resilience Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** A Miracast cast to the Pi survives a few seconds of radio trouble, and a session that does end can be resumed without rediscovery or a PIN.

**Architecture:** The Wi-Fi Direct group's lifetime is separated from the session's, so the group, its credentials and (for thirty seconds) the screen outlive a dropped peer. Detection of a dead peer moves from one signal at 60 s to three signals at 2-10 s. A new pure module turns the sink's existing loss counters into bitrate requests, so a degrading link loses picture quality instead of the session.

**Tech Stack:** Rust (workspace already in place), `wpa_supplicant` 2.10 on DietPi/Debian 13, no new dependencies.

**Spec:** `docs/superpowers/specs/2026-09-03-castr-miracast-resilience-design.md`

## Global Constraints

- **Sink-side only.** No change to the Windows machine, and no change to the castr protocol's own path. This is Miracast only.
- **No new crates.** Nothing is added to any `Cargo.toml`.
- **Pure layers stay pure.** Everything except `sink.rs` must compile and test on Windows. `sink.rs` alone is `#[cfg(target_os = "linux")]`. Never put a socket, a process spawn or a `libc` call in `quality.rs` or `lifecycle.rs`.
- **Hold the screen for 30 seconds; hold the group indefinitely.** `P2P_GROUP_REMOVE` is sent only on a radio error or on shutdown.
- **Bitrate ladder: 8000 → 4000 → 2000 kbps.** A second counts as bad at **5 or more** lost packets plus continuity errors. Falling goes straight to the floor. Rising takes **10 consecutive clean seconds per step**. At most one request per second.
- **Keep-alive 5 s, timeout 10 s. RTP silence timeout 2 s.**
- **Comments explain why, not what.** Match the surrounding code: the existing crate comments the reasoning behind a decision and never narrates the code.
- **Verification commands.** Windows: `cargo test -q --workspace`. Linux crates plus clippy with `-D warnings`: `bash scripts/pi/test-linux.sh`. Cross-build: `bash scripts/pi/build-pi.sh`. Deploy: `bash scripts/pi/deploy.sh dietpi@192.168.88.157`.

---

## File Structure

| File | Responsibility |
|---|---|
| `scripts/pi/wpa_supplicant-p2p.conf` (modify) | The three radio settings that stop the group owner giving up on a blipping client. |
| `crates/castr-miracast/src/rtsp.rs` (modify) | Keep-alive timing; the `SET_PARAMETER` that asks the source for less bitrate. |
| `crates/castr-miracast/src/quality.rs` (create) | Pure: cumulative loss in, bitrate ceiling out. Nothing else. |
| `crates/castr-miracast/src/lifecycle.rs` (create) | Pure: the Advertising/Streaming/Holding state machine, events in, actions out. |
| `crates/castr-miracast/src/session.rs` (modify) | RTP silence detection; feeds the ladder and emits its requests. |
| `crates/castr-miracast/src/sink.rs` (modify) | Drives the lifecycle machine; the group now outlives the session. |
| `crates/castr-receiver/src/pipeline.rs` (modify) | Shows "Reconnecting…" while the sink holds the screen. |
| `crates/castr-miracast/examples/loopback-source.rs` (modify) | Two flags so loss and a vanishing peer can be simulated without a radio. |
| `docs/superpowers/verification/2026-09-03-castr-miracast-resilience-e2e.md` (create) | What the hardware proved, including the three things only hardware can answer. |

---

### Task 1: The radio settings

**Files:**
- Modify: `scripts/pi/wpa_supplicant-p2p.conf`

**Interfaces:**
- Consumes: nothing.
- Produces: nothing in code. Later tasks assume these settings are deployed.

This task has no unit test — it is configuration for a daemon we do not own. Its deliverable is the settings deployed and their effect read back from the running group owner.

- [ ] **Step 1: Add the three settings**

Append to `scripts/pi/wpa_supplicant-p2p.conf`:

```
# As group owner we are the access point, and the default AP behaviour is to
# deauthenticate a client whose acknowledgements start failing. That is exactly
# what a shared Wi-Fi/Bluetooth antenna looks like from this side: acks stop for
# under a second and we throw the PC off a working cast.
disassoc_low_ack=0
# A paused cast is not a departed peer.
ap_max_inactivity=300
# The group owner does not go quiet on a power-save schedule. The Pi is
# mains-powered, and a client that misses the window sees a stalled link.
p2p_go_ctwindow=0
```

Do not touch `beacon_int` or `dtim_period`. They are plausible tuning targets with no measurement behind them, and changing them would be guessing.

- [ ] **Step 2: Deploy**

Run: `bash scripts/pi/deploy.sh dietpi@192.168.88.157`
Expected: ends with `deployed to dietpi@192.168.88.157`. `deploy.sh` already pushes this file to `/etc/castr/wpa_supplicant-p2p.conf` on every run.

- [ ] **Step 3: Confirm the supplicant accepted them**

The supplicant refuses to start on an unparsable config, so a running group owner is most of the proof. Confirm both halves:

```bash
ssh dietpi@192.168.88.157 'sudo journalctl -u castr-receiver --since "-2min" --no-pager | grep -E "group .* up|Failed to parse|line [0-9]+"'
```

Expected: a `group p2p-wlan0-N up` line and no parse errors. If a setting were rejected the supplicant would log `Line N: unknown ...` and exit, and the sink would log `wpa_supplicant exited with status`.

Record the exact output in the task report: whether `brcmfmac` *honours* these (as opposed to parsing them) is a hardware question that Task 7 answers, and the report is where that thread starts.

- [ ] **Step 4: Commit**

```bash
git add scripts/pi/wpa_supplicant-p2p.conf
git commit -m "feat(miracast): stop the group owner giving up on a blipping client"
```

---

### Task 2: Faster detection in the negotiation

**Files:**
- Modify: `crates/castr-miracast/src/rtsp.rs:317-318` (the constants), `crates/castr-miracast/src/rtsp.rs:541-566` (`tick`)

**Interfaces:**
- Consumes: `rtsp::Negotiation::tick(&mut self, now: Instant) -> Vec<Action>`, `rtsp::Action::{Send, Play, Teardown}`.
- Produces: no signature change. `KEEPALIVE_EVERY` becomes 5 s and `KEEPALIVE_TIMEOUT` 10 s, and the teardown reason text changes.

- [ ] **Step 1: Write the failing test**

Add to the `negotiation_tests` module at the bottom of `crates/castr-miracast/src/rtsp.rs`:

```rust
#[test]
fn a_dead_peer_is_noticed_within_ten_seconds() {
    let mut n = playing();
    let t0 = Instant::now();
    // Nine seconds of silence is not yet a dead peer: a single lost keep-alive
    // on a busy link must not end a healthy session.
    let quiet = n.tick(t0 + Duration::from_secs(9));
    assert!(
        !quiet.iter().any(|a| matches!(a, Action::Teardown(_))),
        "{quiet:?}"
    );
    let dead = n.tick(t0 + Duration::from_secs(11));
    assert!(
        dead.iter().any(|a| matches!(a, Action::Teardown(_))),
        "{dead:?}"
    );
}

#[test]
fn keep_alives_go_out_every_five_seconds() {
    let mut n = playing();
    let t0 = Instant::now();
    let early = n.tick(t0 + Duration::from_secs(3));
    assert!(early.is_empty(), "too soon: {early:?}");
    let due = n.tick(t0 + Duration::from_secs(6));
    assert_eq!(due.len(), 1, "one keep-alive: {due:?}");
}
```

Both tests need a helper that drives a fresh `Negotiation` to `Playing`. Add it to that test module:

```rust
/// A negotiation driven to Playing. Named for the state, not the fixture, so it
/// does not read as a recursive call to `test_support::negotiation_to_playing`.
fn playing() -> Negotiation {
    let mut n = Negotiation::new(caps(), "01234567".into());
    for msg in crate::test_support::negotiation_to_playing() {
        let (m, _) = parse(msg.as_bytes()).unwrap().unwrap();
        n.on_message_at(&m, Instant::now());
    }
    n
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -q -p castr-miracast rtsp::negotiation_tests`
Expected: FAIL. `a_dead_peer_is_noticed_within_ten_seconds` fails because at 11 s the 60 s timeout has not elapsed, so no `Teardown` is produced.

- [ ] **Step 3: Change the constants and the reason text**

In `crates/castr-miracast/src/rtsp.rs`, replace lines 317-318:

```rust
/// A dead radio is invisible to TCP for minutes, so the keep-alive is the
/// fastest signal the control channel has. Five seconds costs nothing on a
/// link carrying 8 Mbps of video.
const KEEPALIVE_EVERY: Duration = Duration::from_secs(5);
/// Two missed keep-alives, not one: a single loss on a busy link must not end
/// a healthy session.
const KEEPALIVE_TIMEOUT: Duration = Duration::from_secs(10);
```

In `tick`, change the teardown text:

```rust
out.push(Action::Teardown("no keep-alive reply for 10 s"));
```

- [ ] **Step 4: Fix the existing test that asserts the old text**

Search for it and update the expectation:

```bash
grep -rn "60 s" crates/castr-miracast/src/
```

Any test asserting the old reason string, or driving the clock 60+ seconds to force a teardown, must be updated to the new timing. Do not delete such a test — retime it.

- [ ] **Step 5: Run the tests to verify they pass**

Run: `cargo test -q -p castr-miracast`
Expected: PASS, with the new tests included.

- [ ] **Step 6: Commit**

```bash
git add crates/castr-miracast/src/rtsp.rs
git commit -m "feat(miracast): notice a dead peer in ten seconds, not sixty"
```

---

### Task 3: The bitrate ladder

**Files:**
- Create: `crates/castr-miracast/src/quality.rs`
- Modify: `crates/castr-miracast/src/lib.rs`

**Interfaces:**
- Consumes: nothing. This module is pure and standalone.
- Produces: `quality::BitrateLadder::{new() -> Self, current_kbps(&self) -> u32, sample(&mut self, cumulative_loss: u64, now: Instant) -> Option<u32>}`, and `quality::LADDER: [u32; 3]`.

`sample` is fed the *cumulative* loss counter, not a per-second delta, because that is what the session already has: `Reorder::lost()` and `DemuxStats::continuity_errors` both count up forever. The ladder does its own differencing so no caller has to remember the previous value.

- [ ] **Step 1: Write the failing tests**

Create `crates/castr-miracast/src/quality.rs` with only the tests and the module doc:

```rust
//! Loss numbers in, a bitrate ceiling out.
//!
//! A 2.4 GHz link that is losing packets does not recover by being asked for
//! more data. Falling is instant and rising is slow on purpose: that asymmetry
//! is what stops the request oscillating on a link that is marginal rather
//! than broken.

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    #[test]
    fn one_bad_second_goes_straight_to_the_floor() {
        let mut l = BitrateLadder::new();
        let t0 = Instant::now();
        assert_eq!(l.current_kbps(), 8000);
        assert_eq!(l.sample(5, t0 + Duration::from_secs(1)), Some(2000));
        assert_eq!(l.current_kbps(), 2000);
    }

    #[test]
    fn a_quiet_second_asks_for_nothing() {
        let mut l = BitrateLadder::new();
        let t0 = Instant::now();
        assert_eq!(l.sample(4, t0 + Duration::from_secs(1)), None, "under the threshold");
        assert_eq!(l.current_kbps(), 8000);
    }

    #[test]
    fn ten_clean_seconds_buy_one_step_back_up() {
        let mut l = BitrateLadder::new();
        let mut t = Instant::now();
        t += Duration::from_secs(1);
        assert_eq!(l.sample(9, t), Some(2000));
        // Nine clean seconds are not enough.
        for _ in 0..9 {
            t += Duration::from_secs(1);
            assert_eq!(l.sample(9, t), None);
        }
        t += Duration::from_secs(1);
        assert_eq!(l.sample(9, t), Some(4000), "the tenth clean second");
        for _ in 0..9 {
            t += Duration::from_secs(1);
            assert_eq!(l.sample(9, t), None);
        }
        t += Duration::from_secs(1);
        assert_eq!(l.sample(9, t), Some(8000), "back to the top");
    }

    #[test]
    fn a_flapping_link_does_not_oscillate() {
        let mut l = BitrateLadder::new();
        let mut t = Instant::now();
        let mut loss = 0;
        // Alternating bad and clean seconds: the clean ones never accumulate to
        // ten, so after the first drop nothing further is ever requested.
        let mut requests = Vec::new();
        for i in 0..40 {
            t += Duration::from_secs(1);
            if i % 2 == 0 {
                loss += 5;
            }
            if let Some(k) = l.sample(loss, t) {
                requests.push(k);
            }
        }
        assert_eq!(requests, vec![2000], "one drop, no climb, no flapping");
    }

    #[test]
    fn samples_closer_than_a_second_are_ignored() {
        let mut l = BitrateLadder::new();
        let t0 = Instant::now();
        assert_eq!(l.sample(0, t0 + Duration::from_millis(100)), None);
        assert_eq!(l.sample(99, t0 + Duration::from_millis(200)), None,
                   "a burst inside one second is still one second");
    }

    #[test]
    fn the_floor_is_never_requested_twice() {
        let mut l = BitrateLadder::new();
        let mut t = Instant::now();
        t += Duration::from_secs(1);
        assert_eq!(l.sample(10, t), Some(2000));
        t += Duration::from_secs(1);
        assert_eq!(l.sample(20, t), None, "already at the floor");
    }
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -q -p castr-miracast quality::`
Expected: FAIL to compile — `BitrateLadder` does not exist.

- [ ] **Step 3: Write the implementation**

Add above the test module in `crates/castr-miracast/src/quality.rs`:

```rust
use std::time::{Duration, Instant};

/// Three rungs. More would be noise at 720p30 on a 2.4 GHz radio.
pub const LADDER: [u32; 3] = [8000, 4000, 2000];
/// A second is bad at five losses. At 720p30 a frame is roughly 24 datagrams,
/// so this sits well above the single-packet noise floor and well below a
/// visibly damaged second.
const BAD_SECOND: u64 = 5;
/// Clean seconds needed per step back up.
const CLEAN_PER_STEP: u32 = 10;
/// The ladder acts at most once per second, whatever the caller does.
const SAMPLE_INTERVAL: Duration = Duration::from_secs(1);

pub struct BitrateLadder {
    /// Index into `LADDER`; 0 is the top.
    rung: usize,
    clean: u32,
    last_loss: u64,
    last_sample: Option<Instant>,
}

impl Default for BitrateLadder {
    fn default() -> Self {
        Self::new()
    }
}

impl BitrateLadder {
    pub fn new() -> Self {
        Self {
            rung: 0,
            clean: 0,
            last_loss: 0,
            last_sample: None,
        }
    }

    pub fn current_kbps(&self) -> u32 {
        LADDER[self.rung]
    }

    /// Feeds the cumulative loss counter. Returns the new ceiling when it
    /// changed and the source must be told, and `None` otherwise.
    pub fn sample(&mut self, cumulative_loss: u64, now: Instant) -> Option<u32> {
        if let Some(t) = self.last_sample {
            if now.saturating_duration_since(t) < SAMPLE_INTERVAL {
                return None;
            }
        }
        self.last_sample = Some(now);
        let delta = cumulative_loss.saturating_sub(self.last_loss);
        self.last_loss = cumulative_loss;

        if delta >= BAD_SECOND {
            self.clean = 0;
            let floor = LADDER.len() - 1;
            if self.rung == floor {
                return None;
            }
            self.rung = floor;
            return Some(self.current_kbps());
        }
        self.clean += 1;
        if self.clean >= CLEAN_PER_STEP && self.rung > 0 {
            self.clean = 0;
            self.rung -= 1;
            return Some(self.current_kbps());
        }
        None
    }
}
```

- [ ] **Step 4: Declare the module**

In `crates/castr-miracast/src/lib.rs`, add `pub mod quality;` to the alphabetical list of pure modules (between `p2p` and `rtp`).

- [ ] **Step 5: Run the tests to verify they pass**

Run: `cargo test -q -p castr-miracast quality::`
Expected: PASS, 6 tests.

- [ ] **Step 6: Commit**

```bash
git add crates/castr-miracast/src/quality.rs crates/castr-miracast/src/lib.rs
git commit -m "feat(miracast): a bitrate ladder that falls fast and climbs slowly"
```

---

### Task 4: Asking the source for less, and noticing silence

**Files:**
- Modify: `crates/castr-miracast/src/rtsp.rs`, `crates/castr-miracast/src/session.rs`

**Interfaces:**
- Consumes: `quality::BitrateLadder`, `rtsp::Negotiation`, `rtsp::request(method, uri, cseq, body) -> Message`.
- Produces: `rtsp::Negotiation::request_bitrate(&mut self, kbps: u32, now: Instant) -> Vec<Action>`; `session::Session` gains RTP silence detection (no new public method — it surfaces through the existing `tick`).

- [ ] **Step 1: Write the failing test for the bitrate request**

Add to `negotiation_tests` in `crates/castr-miracast/src/rtsp.rs`:

```rust
#[test]
fn a_bitrate_request_names_the_ceiling_and_the_session() {
    let mut n = playing();
    let out = n.request_bitrate(2000, Instant::now());
    let Some(Action::Send(m)) = out.into_iter().next() else {
        panic!("no message");
    };
    let text = m.format();
    assert!(text.starts_with("SET_PARAMETER "), "{text}");
    assert!(text.contains("microsoft_max_bitrate: 2000\r\n"), "{text}");
    assert!(text.contains("Session: "), "the source needs to know which session: {text}");
}
```

- [ ] **Step 2: Run it to verify it fails**

Run: `cargo test -q -p castr-miracast a_bitrate_request_names`
Expected: FAIL to compile — no method `request_bitrate`.

- [ ] **Step 3: Implement `request_bitrate`**

In `crates/castr-miracast/src/rtsp.rs`, directly after `request_idr`:

```rust
    /// Asks the source to cap its bitrate. This is a request: the source may
    /// ignore it, which is why the sink never assumes the rate actually fell.
    pub fn request_bitrate(&mut self, kbps: u32, _now: Instant) -> Vec<Action> {
        if !matches!(self.state, NegState::Playing) {
            return Vec::new();
        }
        let c = self.cseq();
        let uri = self.uri();
        let body = format!("microsoft_max_bitrate: {kbps}\r\n");
        let mut m = request("SET_PARAMETER", &uri, c, &body);
        if let Some(s) = &self.peer_session {
            m.headers.push(("Session".into(), s.clone()));
        }
        vec![Action::Send(m)]
    }
```

The `_now` parameter is unused today and is there because every other
time-dependent method on this type takes the clock; a later rate limit would
land here rather than changing the signature.

- [ ] **Step 4: Write the failing tests for the session**

Add to the `tests` module in `crates/castr-miracast/src/session.rs`:

```rust
#[test]
fn sustained_loss_asks_the_source_for_less() {
    let mut s = sess();
    drive_to_playing(&mut s);
    let t0 = Instant::now();
    // A recorded stream with a third of its datagrams thrown away: the
    // reorder window counts the gaps as loss.
    let mut n = 0;
    for datagram in test_support::recorded_stream(60) {
        n += 1;
        if n % 3 == 0 {
            continue;
        }
        s.on_rtp_datagram_at(&datagram, t0);
    }
    let out = s.tick(t0 + Duration::from_secs(1));
    let asked = sent(&out)
        .iter()
        .any(|m| m.contains("microsoft_max_bitrate: 2000"));
    assert!(asked, "{:?}", sent(&out));
}

#[test]
fn two_seconds_without_media_ends_the_session() {
    let mut s = sess();
    drive_to_playing(&mut s);
    let t0 = Instant::now();
    for datagram in test_support::recorded_stream(4) {
        s.on_rtp_datagram_at(&datagram, t0);
    }
    let quiet = s.tick(t0 + Duration::from_millis(1500));
    assert!(
        !quiet.iter().any(|e| matches!(e, SinkEvent::Ended(_))),
        "1.5 s is a hiccup, not a departure: {quiet:?}"
    );
    let gone = s.tick(t0 + Duration::from_millis(2500));
    assert!(
        gone.iter().any(|e| matches!(e, SinkEvent::Ended(r) if r.contains("no media"))),
        "{gone:?}"
    );
}

#[test]
fn silence_before_any_media_is_not_a_departure() {
    // The gap between PLAY and the first datagram is the source starting its
    // encoder, which takes longer than two seconds on a cold start.
    let mut s = sess();
    drive_to_playing(&mut s);
    let out = s.tick(Instant::now() + Duration::from_secs(5));
    assert!(
        !out.iter().any(|e| matches!(e, SinkEvent::Ended(_))),
        "{out:?}"
    );
}
```

- [ ] **Step 5: Run them to verify they fail**

Run: `cargo test -q -p castr-miracast session::`
Expected: FAIL — no bitrate request is ever sent, and `tick` never ends a session for silence.

- [ ] **Step 6: Implement both in the session**

In `crates/castr-miracast/src/session.rs`, add to the imports:

```rust
use crate::quality::BitrateLadder;
use std::time::Duration;
```

Add a constant next to `REORDER_WINDOW`:

```rust
/// A playing session that hears no media for this long has lost its peer. TCP
/// will not tell us for minutes, so this is the fastest signal we have.
const MEDIA_SILENCE: Duration = Duration::from_secs(2);
```

Add three fields to `Session` and initialise them in `new`:

```rust
    ladder: BitrateLadder,
    /// When media last arrived. `None` until the first datagram, because the
    /// gap between PLAY and the first frame is the source starting its encoder.
    last_media: Option<Instant>,
```

(`ladder: BitrateLadder::new(), last_media: None,` in `new`.)

In `on_rtp_datagram_at`, immediately after the `payload_type` check and before the reorder loop:

```rust
        self.last_media = Some(now);
```

At the end of `on_rtp_datagram_at`, replace the existing loss block with one that also feeds the ladder:

```rust
        // A lost packet or a continuity break damaged a frame: ask for an IDR
        // rather than waiting for the source's next scheduled keyframe.
        let lost = self.reorder.lost() + self.demux.stats().continuity_errors;
        if lost > self.lost_at_last_check {
            self.lost_at_last_check = lost;
            let actions = self.negotiation.request_idr(now);
            self.apply(actions, &mut out);
        }
        // Sustained loss is a different problem from a damaged frame: the link
        // cannot carry what the source is sending, so ask it for less.
        if let Some(kbps) = self.ladder.sample(lost, now) {
            tracing::info!("miracast: loss is up, asking the source for {kbps} kbps");
            let actions = self.negotiation.request_bitrate(kbps, now);
            self.apply(actions, &mut out);
        }
        out
```

In `tick`, before delegating to the negotiation:

```rust
    pub fn tick(&mut self, now: Instant) -> Vec<SinkEvent> {
        let mut out = Vec::new();
        if self.playing
            && self
                .last_media
                .is_some_and(|t| now.saturating_duration_since(t) > MEDIA_SILENCE)
        {
            self.playing = false;
            out.push(SinkEvent::Ended("no media for 2 s"));
            return out;
        }
        // The ladder is sampled here too, so a link that goes quiet rather
        // than lossy still gets a chance to climb back up.
        let lost = self.reorder.lost() + self.demux.stats().continuity_errors;
        if let Some(kbps) = self.ladder.sample(lost, now) {
            let actions = self.negotiation.request_bitrate(kbps, now);
            self.apply(actions, &mut out);
        }
        let actions = self.negotiation.tick(now);
        self.apply(actions, &mut out);
        out
    }
```

- [ ] **Step 7: Run the tests to verify they pass**

Run: `cargo test -q -p castr-miracast`
Expected: PASS. If `sustained_loss_asks_the_source_for_less` still fails, print `s.tick(...)`'s events and check the reorder window is actually counting loss — dropping every third datagram of a 60-unit stream must produce at least 5 losses in the first second.

- [ ] **Step 8: Commit**

```bash
git add crates/castr-miracast/src/rtsp.rs crates/castr-miracast/src/session.rs
git commit -m "feat(miracast): ask the source for less bitrate, and notice silence in two seconds"
```

---

### Task 5: The lifecycle state machine

**Files:**
- Create: `crates/castr-miracast/src/lifecycle.rs`
- Modify: `crates/castr-miracast/src/lib.rs`

**Interfaces:**
- Consumes: nothing. Pure.
- Produces: `lifecycle::{Phase, Event, Action, Lifecycle}` with `Lifecycle::{new() -> Self, phase(&self) -> Phase, on(&mut self, e: Event, now: Instant) -> Vec<Action>, tick(&mut self, now: Instant) -> Vec<Action>}` and `lifecycle::HOLD: Duration`.

This machine knows nothing about peers' identities. The supplicant decides whether a returning PC needs WPS; from here, a connection is a connection.

- [ ] **Step 1: Write the failing tests**

Create `crates/castr-miracast/src/lifecycle.rs` containing the module doc and tests only:

```rust
//! When the peer goes away, what happens to the group and the screen.
//!
//! The group's lifetime is the service's lifetime, not the session's. A peer
//! that drops for a moment must find everything exactly as it left it: same
//! group, same credentials, and for thirty seconds the same screen.

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    #[test]
    fn a_new_session_takes_the_display() {
        let mut l = Lifecycle::new();
        assert_eq!(l.phase(), Phase::Advertising);
        let out = l.on(Event::Connected, Instant::now());
        assert_eq!(l.phase(), Phase::Streaming);
        assert!(out.contains(&Action::AcquireDisplay), "{out:?}");
    }

    #[test]
    fn a_blip_resumes_without_releasing_the_display() {
        let mut l = Lifecycle::new();
        let t0 = Instant::now();
        l.on(Event::Connected, t0);
        let lost = l.on(Event::Ended("no media for 2 s"), t0);
        assert_eq!(l.phase(), Phase::Holding);
        assert!(
            !lost.iter().any(|a| matches!(a, Action::ReleaseDisplay)),
            "the screen stays theirs: {lost:?}"
        );
        assert!(lost.contains(&Action::ShowReconnecting), "{lost:?}");

        let back = l.on(Event::Connected, t0 + Duration::from_secs(3));
        assert_eq!(l.phase(), Phase::Streaming);
        assert!(back.contains(&Action::ClearOverlay), "{back:?}");
        assert!(
            !back.iter().any(|a| matches!(a, Action::AcquireDisplay)),
            "it was never released, so it is not re-acquired: {back:?}"
        );
    }

    #[test]
    fn the_hold_expires_after_thirty_seconds() {
        let mut l = Lifecycle::new();
        let t0 = Instant::now();
        l.on(Event::Connected, t0);
        l.on(Event::Ended("source closed"), t0);
        assert!(l.tick(t0 + Duration::from_secs(29)).is_empty(), "still holding");
        let expired = l.tick(t0 + Duration::from_secs(31));
        assert_eq!(l.phase(), Phase::Advertising);
        assert!(expired.contains(&Action::ReleaseDisplay), "{expired:?}");
        assert!(expired.contains(&Action::ClearOverlay), "{expired:?}");
    }

    #[test]
    fn the_hold_expires_only_once() {
        let mut l = Lifecycle::new();
        let t0 = Instant::now();
        l.on(Event::Connected, t0);
        l.on(Event::Ended("source closed"), t0);
        l.tick(t0 + Duration::from_secs(31));
        assert!(l.tick(t0 + Duration::from_secs(32)).is_empty());
        assert!(l.tick(t0 + Duration::from_secs(90)).is_empty());
    }

    #[test]
    fn a_radio_error_rebuilds_the_group_and_gives_the_screen_back() {
        let mut l = Lifecycle::new();
        let t0 = Instant::now();
        l.on(Event::Connected, t0);
        let out = l.on(Event::RadioError, t0);
        assert_eq!(l.phase(), Phase::Advertising);
        assert!(out.contains(&Action::ReleaseDisplay), "{out:?}");
        assert!(out.contains(&Action::RebuildGroup), "{out:?}");
    }

    #[test]
    fn a_radio_error_while_advertising_still_rebuilds() {
        let mut l = Lifecycle::new();
        let out = l.on(Event::RadioError, Instant::now());
        assert!(out.contains(&Action::RebuildGroup), "{out:?}");
        assert!(
            !out.contains(&Action::ReleaseDisplay),
            "nothing was held, so nothing is released: {out:?}"
        );
    }

    #[test]
    fn a_second_connection_while_streaming_changes_nothing() {
        let mut l = Lifecycle::new();
        let t0 = Instant::now();
        l.on(Event::Connected, t0);
        let again = l.on(Event::Connected, t0);
        assert_eq!(l.phase(), Phase::Streaming);
        assert!(again.is_empty(), "{again:?}");
    }

    #[test]
    fn an_end_while_advertising_is_ignored() {
        let mut l = Lifecycle::new();
        let out = l.on(Event::Ended("stale"), Instant::now());
        assert_eq!(l.phase(), Phase::Advertising);
        assert!(out.is_empty(), "{out:?}");
    }
}
```

- [ ] **Step 2: Run them to verify they fail**

Run: `cargo test -q -p castr-miracast lifecycle::`
Expected: FAIL to compile — nothing in the module exists yet.

- [ ] **Step 3: Write the implementation**

Add above the test module:

```rust
use std::time::{Duration, Instant};

/// How long the screen stays with a peer that vanished. Long enough to cover a
/// radio blip; short enough that a room is not stuck looking at a dead cast.
pub const HOLD: Duration = Duration::from_secs(30);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    /// Group up, credentials valid, listening.
    Advertising,
    Streaming,
    /// The peer vanished. The group and the screen are still theirs.
    Holding,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    /// An RTSP connection was accepted. The caller has already taken the
    /// display from the arbiter if it needed to.
    Connected,
    Ended(&'static str),
    /// The radio itself failed; nothing about the group can be trusted.
    RadioError,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    AcquireDisplay,
    ReleaseDisplay,
    ShowReconnecting,
    ClearOverlay,
    RebuildGroup,
}

pub struct Lifecycle {
    phase: Phase,
    holding_since: Option<Instant>,
}

impl Default for Lifecycle {
    fn default() -> Self {
        Self::new()
    }
}

impl Lifecycle {
    pub fn new() -> Self {
        Self {
            phase: Phase::Advertising,
            holding_since: None,
        }
    }

    pub fn phase(&self) -> Phase {
        self.phase
    }

    /// True while the display is ours, which is both Streaming and Holding.
    fn holds_display(&self) -> bool {
        matches!(self.phase, Phase::Streaming | Phase::Holding)
    }

    pub fn on(&mut self, e: Event, now: Instant) -> Vec<Action> {
        match (self.phase, e) {
            (Phase::Advertising, Event::Connected) => {
                self.phase = Phase::Streaming;
                vec![Action::AcquireDisplay, Action::ClearOverlay]
            }
            (Phase::Holding, Event::Connected) => {
                // The display was never released, so it is not re-acquired.
                self.phase = Phase::Streaming;
                self.holding_since = None;
                vec![Action::ClearOverlay]
            }
            (Phase::Streaming, Event::Ended(_)) => {
                self.phase = Phase::Holding;
                self.holding_since = Some(now);
                vec![Action::ShowReconnecting]
            }
            (_, Event::RadioError) => {
                let held = self.holds_display();
                self.phase = Phase::Advertising;
                self.holding_since = None;
                let mut out = vec![Action::RebuildGroup];
                if held {
                    out.insert(0, Action::ReleaseDisplay);
                    out.push(Action::ClearOverlay);
                }
                out
            }
            _ => Vec::new(),
        }
    }

    pub fn tick(&mut self, now: Instant) -> Vec<Action> {
        let Some(since) = self.holding_since else {
            return Vec::new();
        };
        if now.saturating_duration_since(since) < HOLD {
            return Vec::new();
        }
        self.phase = Phase::Advertising;
        self.holding_since = None;
        vec![Action::ReleaseDisplay, Action::ClearOverlay]
    }
}
```

- [ ] **Step 4: Declare the module**

In `crates/castr-miracast/src/lib.rs`, add `pub mod lifecycle;` to the list (between `dhcp` and `p2p`).

- [ ] **Step 5: Run the tests to verify they pass**

Run: `cargo test -q -p castr-miracast lifecycle::`
Expected: PASS, 8 tests.

- [ ] **Step 6: Commit**

```bash
git add crates/castr-miracast/src/lifecycle.rs crates/castr-miracast/src/lib.rs
git commit -m "feat(miracast): the group outlives the session, as a state machine"
```

---

### Task 6: The sink drives the lifecycle

**Files:**
- Modify: `crates/castr-miracast/src/sink.rs`, `crates/castr-receiver/src/pipeline.rs`

**Interfaces:**
- Consumes: `lifecycle::{Lifecycle, Phase, Event, Action}`, `session::{Session, SinkEvent}`, `p2p::{Command, Control, Event as P2pEvent}`.
- Produces: `sink::SinkOut` gains a `Reconnecting` variant. The `DisplayArbiterHandle` trait is unchanged.

This is the structural change the whole sub-project exists for. Today `one_group` builds a group, serves exactly one session, and returns — after which `GroupGuard` destroys the group. After this task, `one_group` builds a group and then serves sessions in a loop until the radio errors or the sink stops.

- [ ] **Step 1: Create the lifecycle and the new event variant**

At the top of `crates/castr-miracast/src/sink.rs`, add the import:

```rust
use crate::lifecycle;
```

At the top of `serve`, alongside `let mut held = false;`:

```rust
    let mut life = lifecycle::Lifecycle::new();
```

`GroupGuard` stays exactly as it is: after this task `serve` returns only on a
radio error or on stop, which are precisely the two cases where the group
*should* be torn down.

Then add to `enum SinkOut`:

```rust
    /// The peer vanished but the group and the screen are still theirs.
    Reconnecting,
```

- [ ] **Step 2: Hold the group across sessions**

In `serve`, the block that currently ends the pass on session end must instead
report the end to the lifecycle machine and keep looping. Replace:

```rust
        if let Some(reason) = ended {
            tracing::info!("miracast: session ended: {reason}");
            let _ = out.send(SinkOut::Ended(reason));
            if held {
                arbiter.release();
            }
            // A new group for the next connection: the source expects a fresh
            // advertisement, and this clears any half-open radio state.
            return Ok(());
        }
```

with:

```rust
        if let Some(reason) = ended {
            tracing::info!("miracast: session ended: {reason}, holding the group");
            let _ = out.send(SinkOut::Ended(reason.clone()));
            conn = None;
            session = None;
            // The group, its credentials and (for now) the screen stay. A peer
            // that dropped for a moment finds everything as it left it.
            let leaked: &'static str = Box::leak(reason.into_boxed_str());
            for a in life.on(lifecycle::Event::Ended(leaked), Instant::now()) {
                apply_lifecycle(a, arbiter, out, &mut held);
            }
        }
```

`Box::leak` is deliberate and bounded: the reason strings are a fixed set of
literals from the session layer, one leak per session end, and the alternative
is threading a lifetime through a state machine that has no other reason to
carry one.

- [ ] **Step 3: React to the peer disconnecting, without ending the group**

In the supplicant-event loop inside `serve`, replace the arm that returns:

```rust
                Event::ClientDisconnected { .. } | Event::GroupRemoved { .. } => {
                    if held {
                        arbiter.release();
                    }
                    let _ = out.send(SinkOut::Ended("peer disconnected".into()));
                    return Ok(());
                }
```

with two distinct cases — a client leaving is a session ending, but the group
vanishing is a radio error:

```rust
                Event::ClientDisconnected { .. } => {
                    conn = None;
                    session = None;
                    let _ = out.send(SinkOut::Ended("peer disconnected".into()));
                    for a in life.on(lifecycle::Event::Ended("peer disconnected"), Instant::now()) {
                        apply_lifecycle(a, arbiter, out, &mut held);
                    }
                }
                Event::GroupRemoved { .. } => {
                    // The group going away under us is the one thing we cannot
                    // hold through: everything about it is now invalid.
                    for a in life.on(lifecycle::Event::RadioError, Instant::now()) {
                        apply_lifecycle(a, arbiter, out, &mut held);
                    }
                    return Ok(());
                }
```

- [ ] **Step 4: Take the display through the lifecycle, not directly**

Replace the accept block's arbiter handling. The machine decides whether the
display needs acquiring; the arbiter is still what actually grants it:

```rust
        if conn.is_none() {
            match listener.accept() {
                Ok((s, from)) => {
                    let resuming = life.phase() == lifecycle::Phase::Holding;
                    if !resuming && !arbiter.try_acquire() {
                        tracing::info!("miracast: refusing {from}: the display is busy");
                        drop(s);
                    } else {
                        tracing::info!("miracast: RTSP connection from {from}");
                        held = true;
                        s.set_nonblocking(true)?;
                        s.set_nodelay(true)?;
                        conn = Some(s);
                        session = Some(Session::new(capabilities(cfg), session_id()));
                        for a in life.on(lifecycle::Event::Connected, Instant::now()) {
                            apply_lifecycle(a, arbiter, out, &mut held);
                        }
                    }
                }
                Err(e) if e.kind() == ErrorKind::WouldBlock => {}
                Err(e) => tracing::warn!("miracast: accept: {e}"),
            }
        }
```

- [ ] **Step 5: Expire the hold**

At the end of each pass through the `serve` loop, next to the session tick:

```rust
        for a in life.tick(Instant::now()) {
            apply_lifecycle(a, arbiter, out, &mut held);
        }
```

- [ ] **Step 6: Write the action applier**

Add near `dispatch` in `crates/castr-miracast/src/sink.rs`:

```rust
/// Carries out one lifecycle decision. The state machine decides; this does.
fn apply_lifecycle(
    a: lifecycle::Action,
    arbiter: &Arc<dyn DisplayArbiterHandle>,
    out: &mpsc::Sender<SinkOut>,
    held: &mut bool,
) {
    match a {
        lifecycle::Action::AcquireDisplay => {
            *held = arbiter.try_acquire();
        }
        lifecycle::Action::ReleaseDisplay => {
            if *held {
                arbiter.release();
                *held = false;
            }
        }
        lifecycle::Action::ShowReconnecting => {
            let _ = out.send(SinkOut::Reconnecting);
        }
        lifecycle::Action::ClearOverlay => {
            let _ = out.send(SinkOut::Started);
        }
        lifecycle::Action::RebuildGroup => {
            tracing::warn!("miracast: the group failed; rebuilding it");
        }
    }
}
```

`ClearOverlay` reuses `SinkOut::Started` because that is already what the
receiver treats as "clear the overlay"; adding a second event that means the
same thing would give the receiver two ways to do one job.

- [ ] **Step 8: Show "Reconnecting…" on the receiver**

In `crates/castr-receiver/src/pipeline.rs`, in the sink-event thread's `match`,
add an arm beside `SinkOut::Started`:

```rust
                    SinkOut::Reconnecting => {
                        let _ = ui.blocking_send(UiEvent::Overlay(Some(
                            "Reconnecting…".into(),
                        )));
                    }
```

- [ ] **Step 8: Verify everything builds and the suites pass**

Run, in order:

```bash
cargo test -q --workspace
bash scripts/pi/test-linux.sh
bash scripts/pi/build-pi.sh
```

Expected: all three green. `test-linux.sh` is the one that matters here — it is
the only check that compiles `sink.rs` at all, and it runs clippy with
`-D warnings`.

- [ ] **Step 9: Commit**

```bash
git add crates/castr-miracast/src/sink.rs crates/castr-receiver/src/pipeline.rs
git commit -m "feat(miracast): hold the group and the screen when the peer drops"
```

---

### Task 7: Prove it without a radio, then with one

**Files:**
- Modify: `crates/castr-miracast/examples/loopback-source.rs`
- Create: `docs/superpowers/verification/2026-09-03-castr-miracast-resilience-e2e.md`
- Modify: `README.md`

**Interfaces:**
- Consumes: everything above.
- Produces: the verification record.

- [ ] **Step 1: Add the two flags to the loopback source**

Replace the argument parsing at the top of `main` in
`crates/castr-miracast/examples/loopback-source.rs`:

```rust
    // loopback-source [addr] [units] [--drop N] [--vanish]
    let mut addr = "192.168.173.1:7236".to_string();
    let mut units: u32 = 48;
    let mut drop_percent: u32 = 0;
    let mut vanish = false;
    let mut positional = 0;
    let mut args = std::env::args().skip(1).peekable();
    while let Some(a) = args.next() {
        match a.as_str() {
            "--drop" => {
                drop_percent = args.next().and_then(|v| v.parse().ok()).unwrap_or(0);
            }
            "--vanish" => vanish = true,
            _ => {
                match positional {
                    0 => addr = a,
                    1 => units = a.parse().unwrap_or(48),
                    _ => {}
                }
                positional += 1;
            }
        }
    }
```

In the send loop, drop datagrams and optionally stop dead half way:

```rust
    let mut sent = 0u32;
    for (i, datagram) in stream.into_iter().enumerate() {
        // Drop `drop_percent` out of every hundred, spread through the
        // stream: sustained loss, not one contiguous hole, because a hole is a
        // different failure and the ladder is meant to see the former.
        if drop_percent > 0 && (i as u32 % 100) < drop_percent {
            continue;
        }
        if vanish && i == count / 2 {
            // Stop sending and stop answering, without closing the socket: this
            // is what a radio going away looks like, as opposed to a clean
            // teardown the sink would see as a normal end.
            println!("vanishing after {sent} datagrams");
            std::thread::sleep(Duration::from_secs(45));
            return Ok(());
        }
        udp.send_to(&datagram, &rtp_addr)?;
        sent += 1;
        std::thread::sleep(Duration::from_millis(33));
    }
```

- [ ] **Step 2: Cross-build and push the example**

```bash
export MSYS_NO_PATHCONV=1
docker run --rm -v "$(pwd -W 2>/dev/null || pwd):/src:ro" \
  -v "$(pwd -W 2>/dev/null || pwd)/dist:/out" \
  -v castr-xtarget:/work -v castr-xcargo:/root/.cargo/registry \
  castr-xbuild:aarch64 bash -c 'set -e
    cargo build --release --locked --target aarch64-unknown-linux-gnu \
      -p castr-miracast --example loopback-source --target-dir /work/target
    cp /work/target/aarch64-unknown-linux-gnu/release/examples/loopback-source \
      /out/loopback-source-aarch64'
cat dist/loopback-source-aarch64 | ssh dietpi@192.168.88.157 \
  'cat > /tmp/loopback-source && chmod +x /tmp/loopback-source'
```

- [ ] **Step 3: Prove the hold and the resume, with no radio peer**

```bash
bash scripts/pi/deploy.sh dietpi@192.168.88.157
ssh dietpi@192.168.88.157 '/tmp/loopback-source 192.168.173.1:7236 120 --vanish'
```

Then, within thirty seconds, in a second shell:

```bash
ssh dietpi@192.168.88.157 '/tmp/loopback-source 192.168.173.1:7236 60'
ssh dietpi@192.168.88.157 'sudo journalctl -u castr-receiver --since "-3min" --no-pager | grep -i miracast'
```

Expected in the journal, in order: `no media for 2 s`, `holding the group`, no
`P2P_GROUP_REMOVE`, then a second `RTSP connection from` — all inside the same
group interface number. Seeing `p2p-wlan0-0` throughout, rather than it
incrementing, is the proof that the group survived.

- [ ] **Step 4: Prove the bitrate ladder, with no radio peer**

```bash
ssh dietpi@192.168.88.157 '/tmp/loopback-source 192.168.173.1:7236 300 --drop 20'
ssh dietpi@192.168.88.157 'sudo journalctl -u castr-receiver --since "-2min" --no-pager | grep -i "asking the source"'
```

Expected: `asking the source for 2000 kbps`, once, not repeatedly.

- [ ] **Step 5: The three hardware questions**

These need the Windows machine and cannot be answered from a shell. Run each
and record the exact output:

1. **Does `brcmfmac` honour the GO settings?** Cast from Windows, then walk out
   of range for five seconds and back. Previously the session ended; record
   whether it now resumes. Also check the journal for `AP-STA-DISCONNECTED`
   during a brief blip — its absence is the evidence that `disassoc_low_ack=0`
   took effect.
2. **Does Windows honour `microsoft_max_bitrate`?** Cast, then force loss
   (start a large file copy over the same band, or run the loopback source
   alongside). Confirm `asking the source for 2000 kbps` appears, then measure
   whether received bitrate actually falls, from the `perf:` lines. **If it does
   not fall, say so plainly in the document** — the fallback (a format change
   to a smaller CEA mode) is deliberately not built yet, and the honest outcome
   of this test may be that it needs to be.
3. **Does a real Bluetooth blip now survive?** Cast, play audio to Bluetooth
   headphones from the same PC, and use them for two minutes. Record every
   disconnect. This is the measurement the sub-project exists to make; a null
   result is still a result and belongs in the document.

- [ ] **Step 6: Write the verification document**

Create `docs/superpowers/verification/2026-09-03-castr-miracast-resilience-e2e.md`
in the same shape as `2026-09-02-castr-miracast-sink-e2e.md`: a summary table
with a PASS/FAIL per numbered step, the commands with their real output, and a
closing section naming everything that did not work or was not run. If a
hardware step could not be run, the row says NOT RUN and the closing section
says why — never leave a reader to infer that an untested thing works.

- [ ] **Step 7: Add the README note**

In `README.md`, in the "Casting from Windows without installing anything"
section, after the three limits, add:

```markdown
If the link wobbles, the Pi holds your session: the picture drops to a lower
quality rather than stopping, and if the connection does break, the Pi keeps
the group and your screen for thirty seconds so you come straight back with no
PIN. A drop that lasts longer than that returns the Pi to its idle screen, and
you can reconnect from Windows+K without re-pairing.
```

- [ ] **Step 8: Commit**

```bash
git add crates/castr-miracast/examples/loopback-source.rs \
        docs/superpowers/verification/2026-09-03-castr-miracast-resilience-e2e.md \
        README.md
git commit -m "docs: Miracast resilience verification, and loss simulation in the loopback source"
```

---

## After all tasks

**REQUIRED SUB-SKILL:** Use superpowers:finishing-a-development-branch.
