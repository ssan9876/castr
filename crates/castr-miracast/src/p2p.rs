//! The wpa_supplicant control channel.
//!
//! `wpa_supplicant` speaks a line protocol over a Unix datagram socket: send a
//! command, read one reply; after `ATTACH`, unsolicited events arrive on the
//! same socket. The command builders and the event parser are pure so they are
//! tested everywhere; only `Control`, which owns the socket, is Linux-only.

/// Commands we send. Built as strings so they can be asserted in tests and
/// logged verbatim when something goes wrong on the hardware.
pub struct Command;

impl Command {
    /// The name Windows shows in its cast list.
    pub fn device_name(name: &str) -> String {
        format!("SET device_name {name}")
    }
    pub fn wifi_display_enable() -> String {
        "SET wifi_display 1".into()
    }
    /// Sets one WFD information-element subelement; index 0 is device info.
    pub fn subelement(index: u8, hex: &str) -> String {
        format!("WFD_SUBELEM_SET {index} {hex}")
    }
    /// Creates a *new* persistent group with us as group owner on a fixed
    /// channel. New credentials every time, so anything already paired is
    /// locked out - use `group_add_stored` when there is a group to reuse.
    pub fn group_add_persistent(freq_mhz: u32) -> String {
        format!("P2P_GROUP_ADD persistent freq={freq_mhz}")
    }
    /// Brings the stored persistent group back up, keeping the SSID and
    /// passphrase a source was already given.
    pub fn group_add_stored(network_id: u32, freq_mhz: u32) -> String {
        format!("P2P_GROUP_ADD persistent={network_id} freq={freq_mhz}")
    }
    /// Lists the stored networks, which is where a persistent group lives.
    pub fn list_networks() -> String {
        "LIST_NETWORKS".to_string()
    }
    /// Authorises an enrolment with the PIN we display.
    pub fn wps_pin(pin: &str) -> String {
        format!("WPS_PIN any {pin}")
    }
    pub fn group_remove(iface: &str) -> String {
        format!("P2P_GROUP_REMOVE {iface}")
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    GroupStarted {
        interface: String,
        go: bool,
        freq_mhz: u32,
    },
    GroupRemoved {
        interface: String,
    },
    ClientConnected {
        mac: String,
    },
    ClientDisconnected {
        mac: String,
    },
    ProvisionRequest {
        peer: String,
    },
    WpsSuccess,
    WpsFail,
}

/// Parses one unsolicited event line. Unknown events yield `None` rather than
/// an error: the supplicant emits many we do not care about.
/// The id of the stored persistent P2P group in a `LIST_NETWORKS` reply, if
/// there is one.
///
/// The reply is a header line then one tab-separated row per network:
/// `id / ssid / bssid / flags`. A persistent group owner's row carries
/// `[P2P-PERSISTENT]`, which is what distinguishes it from an ordinary saved
/// station network - and reusing it is the only way a source that paired once
/// gets back in without being asked for a PIN again.
pub fn stored_persistent_group(reply: &str) -> Option<u32> {
    reply
        .lines()
        .skip(1)
        .filter(|l| l.contains("[P2P-PERSISTENT]"))
        .find_map(|l| l.split('\t').next()?.trim().parse().ok())
}

pub fn parse_event(line: &str) -> Option<Event> {
    // Strip the "<3>" priority prefix if present.
    let body = match line.split_once('>') {
        Some((p, rest)) if p.starts_with('<') => rest,
        _ => line,
    };
    let mut parts = body.split_whitespace();
    let name = parts.next()?;
    match name {
        "P2P-GROUP-STARTED" => {
            let interface = parts.next()?.to_string();
            let role = parts.next().unwrap_or("");
            let freq_mhz = body
                .split_whitespace()
                .find_map(|t| t.strip_prefix("freq="))
                .and_then(|v| v.parse().ok())
                .unwrap_or(0);
            Some(Event::GroupStarted {
                interface,
                go: role == "GO",
                freq_mhz,
            })
        }
        "P2P-GROUP-REMOVED" => Some(Event::GroupRemoved {
            interface: parts.next()?.to_string(),
        }),
        "AP-STA-CONNECTED" => Some(Event::ClientConnected {
            mac: parts.next()?.to_string(),
        }),
        "AP-STA-DISCONNECTED" => Some(Event::ClientDisconnected {
            mac: parts.next()?.to_string(),
        }),
        "P2P-PROV-DISC-PBC-REQ" | "P2P-PROV-DISC-SHOW-PIN" | "P2P-PROV-DISC-ENTER-PIN" => {
            Some(Event::ProvisionRequest {
                peer: parts.next()?.to_string(),
            })
        }
        "WPS-SUCCESS" => Some(Event::WpsSuccess),
        "WPS-FAIL" | "WPS-TIMEOUT" => Some(Event::WpsFail),
        _ => None,
    }
}

#[cfg(all(test, target_os = "linux"))]
mod control_tests {
    use super::control::{local_socket_name, Control};
    use super::Event;
    use std::os::fd::{FromRawFd, OwnedFd};
    use std::time::Duration;

    /// A connected pair of Unix datagram sockets: one stands in for the
    /// supplicant, the other for our control connection.
    fn pair() -> (OwnedFd, OwnedFd) {
        let mut fds = [0i32; 2];
        // SAFETY: writing two fds into an array we own.
        let r = unsafe { libc::socketpair(libc::AF_UNIX, libc::SOCK_DGRAM, 0, fds.as_mut_ptr()) };
        assert_eq!(r, 0, "socketpair: {}", std::io::Error::last_os_error());
        // SAFETY: both are fresh fds we own from here on.
        unsafe { (OwnedFd::from_raw_fd(fds[0]), OwnedFd::from_raw_fd(fds[1])) }
    }

    fn write(fd: &OwnedFd, line: &str) {
        use std::os::fd::AsRawFd;
        // SAFETY: sending from a live buffer on a socket we own.
        let n = unsafe {
            libc::send(
                fd.as_raw_fd(),
                line.as_ptr() as *const libc::c_void,
                line.len(),
                0,
            )
        };
        assert_eq!(n as usize, line.len());
    }

    #[test]
    fn a_zero_timeout_still_reads_an_event_that_is_already_waiting() {
        // The sink polls every socket itself and then drains the ready ones
        // with a zero timeout. Treating zero as "do not look" made the whole
        // event loop deaf: no station connecting, no WPS result, ever.
        let (supplicant, ours) = pair();
        write(&supplicant, "<3>AP-STA-CONNECTED aa:bb:cc:dd:ee:ff");
        let mut ctrl = Control::from_fd(ours);
        let ev = ctrl
            .poll_event(Duration::from_millis(0))
            .expect("read")
            .expect("an event that was already waiting");
        assert_eq!(
            ev,
            Event::ClientConnected {
                mac: "aa:bb:cc:dd:ee:ff".to_string()
            }
        );
    }

    #[test]
    fn a_zero_timeout_returns_nothing_when_nothing_is_waiting() {
        // It must still not block: an empty socket returns immediately.
        let (_supplicant, ours) = pair();
        let mut ctrl = Control::from_fd(ours);
        assert_eq!(ctrl.poll_event(Duration::from_millis(0)).expect("read"), None);
    }

    #[test]
    fn every_control_binds_its_own_local_socket() {
        // The sink holds one control for wlan0 and one for the group. A
        // shared name meant the second bind unlinked the first's address, and
        // the supplicant's events for it went nowhere.
        let a = local_socket_name("wlan0");
        let b = local_socket_name("p2p-wlan0-0");
        let c = local_socket_name("wlan0");
        assert_ne!(a, b);
        assert_ne!(a, c, "two controls on one interface still differ");
    }
}

#[cfg(target_os = "linux")]
mod control {
    use super::{parse_event, Event};
    use std::io;
    use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
    use std::path::Path;
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::time::{Duration, Instant};

    /// How long a command waits for its reply before giving up.
    const REPLY_TIMEOUT: Duration = Duration::from_secs(5);

    /// A connected control socket. Commands and events share it, so a reply
    /// read may turn up an event first; `request` queues those and hands them
    /// to the caller on the next `poll_event`.
    pub struct Control {
        fd: OwnedFd,
        local_path: Option<std::path::PathBuf>,
        pending: Vec<Event>,
    }

    /// The name of the local socket a control binds. Distinct per interface
    /// and per open, because the supplicant addresses its replies and its
    /// events to whatever address the client bound: two controls sharing a
    /// name means one of them silently stops receiving.
    pub(super) fn local_socket_name(iface: &str) -> String {
        static NEXT: AtomicU32 = AtomicU32::new(0);
        format!(
            "castr-miracast-{}-{}-{}",
            std::process::id(),
            iface.replace('/', "_"),
            NEXT.fetch_add(1, Ordering::Relaxed)
        )
    }

    impl Control {
        /// Connects to `<ctrl_dir>/<iface>`, binding our own socket first
        /// because the supplicant replies to the address we send from.
        pub fn open(ctrl_dir: &Path, iface: &str) -> io::Result<Self> {
            let path = ctrl_dir.join(iface);
            // SAFETY: creating a Unix datagram socket with the standard flags.
            let raw =
                unsafe { libc::socket(libc::AF_UNIX, libc::SOCK_DGRAM | libc::SOCK_CLOEXEC, 0) };
            if raw < 0 {
                return Err(io::Error::last_os_error());
            }
            // SAFETY: `raw` is a fresh fd we own from here on.
            let fd = unsafe { OwnedFd::from_raw_fd(raw) };
            // A filesystem path rather than an abstract one: the supplicant
            // may run in its own network namespace, where abstract names do
            // not cross the boundary. Unique per control, not per process: the
            // sink holds one for wlan0 and one for the group, and a shared
            // name meant the second bind unlinked the first's address and took
            // its events with it.
            let local = std::env::temp_dir().join(local_socket_name(iface));
            let _ = std::fs::remove_file(&local);
            bind_unix(&fd, local.as_os_str())?;
            connect_unix(&fd, path.as_os_str())?;
            Ok(Self {
                fd,
                local_path: Some(local),
                pending: Vec::new(),
            })
        }

        /// Wraps an already-connected socket. Only for tests: it is the one
        /// way to exercise the read path without a supplicant.
        #[cfg(test)]
        pub fn from_fd(fd: OwnedFd) -> Self {
            Self {
                fd,
                local_path: None,
                pending: Vec::new(),
            }
        }

        /// Sends a command and returns its reply, queueing any event lines
        /// that arrive first.
        pub fn request(&mut self, cmd: &str) -> io::Result<String> {
            send(&self.fd, cmd.as_bytes())?;
            let deadline = Instant::now() + REPLY_TIMEOUT;
            loop {
                let Some(line) = recv_line(&self.fd, deadline)? else {
                    return Err(io::Error::new(
                        io::ErrorKind::TimedOut,
                        format!("{cmd}: no reply"),
                    ));
                };
                if line.starts_with('<') {
                    if let Some(e) = parse_event(&line) {
                        self.pending.push(e);
                    }
                    continue;
                }
                return Ok(line);
            }
        }

        /// Subscribes to unsolicited events.
        pub fn attach(&mut self) -> io::Result<()> {
            let reply = self.request("ATTACH")?;
            if reply.trim() == "OK" {
                Ok(())
            } else {
                Err(io::Error::other(format!("ATTACH: {reply}")))
            }
        }

        /// The raw descriptor, so a caller can poll it alongside its own
        /// sockets instead of spinning on short timeouts.
        pub fn as_raw_fd(&self) -> std::os::fd::RawFd {
            self.fd.as_raw_fd()
        }

        pub fn poll_event(&mut self, timeout: Duration) -> io::Result<Option<Event>> {
            if !self.pending.is_empty() {
                return Ok(Some(self.pending.remove(0)));
            }
            let deadline = Instant::now() + timeout;
            while let Some(line) = recv_line(&self.fd, deadline)? {
                if let Some(e) = parse_event(&line) {
                    return Ok(Some(e));
                }
            }
            Ok(None)
        }
    }

    impl Drop for Control {
        fn drop(&mut self) {
            if let Some(p) = &self.local_path {
                let _ = std::fs::remove_file(p);
            }
        }
    }

    fn sockaddr_un(path: &[u8]) -> io::Result<(libc::sockaddr_un, libc::socklen_t)> {
        // SAFETY: all-zero is a valid sockaddr_un.
        let mut addr: libc::sockaddr_un = unsafe { std::mem::zeroed() };
        addr.sun_family = libc::AF_UNIX as libc::sa_family_t;
        if path.len() >= addr.sun_path.len() {
            return Err(io::Error::other("control socket path too long"));
        }
        for (i, b) in path.iter().enumerate() {
            addr.sun_path[i] = *b as libc::c_char;
        }
        let len = (std::mem::size_of::<libc::sa_family_t>() + path.len() + 1) as libc::socklen_t;
        Ok((addr, len))
    }

    fn bind_unix(fd: &OwnedFd, path: &std::ffi::OsStr) -> io::Result<()> {
        use std::os::unix::ffi::OsStrExt;
        let (addr, len) = sockaddr_un(path.as_bytes())?;
        // SAFETY: `addr` is a correctly sized sockaddr_un for `len` bytes.
        let r = unsafe {
            libc::bind(
                fd.as_raw_fd(),
                &addr as *const _ as *const libc::sockaddr,
                len,
            )
        };
        if r < 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }

    fn connect_unix(fd: &OwnedFd, path: &std::ffi::OsStr) -> io::Result<()> {
        use std::os::unix::ffi::OsStrExt;
        let (addr, len) = sockaddr_un(path.as_bytes())?;
        // SAFETY: as above.
        let r = unsafe {
            libc::connect(
                fd.as_raw_fd(),
                &addr as *const _ as *const libc::sockaddr,
                len,
            )
        };
        if r < 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }

    fn send(fd: &OwnedFd, data: &[u8]) -> io::Result<()> {
        // SAFETY: writing `data.len()` bytes from a valid slice to our socket.
        let n = unsafe {
            libc::send(
                fd.as_raw_fd(),
                data.as_ptr() as *const libc::c_void,
                data.len(),
                0,
            )
        };
        if n < 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }

    /// Reads one datagram, waiting until `deadline`. `None` on timeout.
    fn recv_line(fd: &OwnedFd, deadline: Instant) -> io::Result<Option<String>> {
        let mut pfd = libc::pollfd {
            fd: fd.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        };
        // A zero or elapsed deadline means "take what is already there and do
        // not wait", not "do not look". The caller polls every socket itself
        // and then drains the ready ones with a zero timeout, so returning
        // early here made that drain read nothing, ever: the sink's whole
        // event loop was deaf, and only `wait_for_group`, which passes a real
        // timeout, ever saw an event.
        let remaining = deadline.saturating_duration_since(Instant::now());
        // SAFETY: one valid pollfd for the socket we own.
        let r = unsafe {
            libc::poll(
                &mut pfd,
                1,
                remaining.as_millis().min(i32::MAX as u128) as i32,
            )
        };
        if r < 0 {
            let e = io::Error::last_os_error();
            if e.kind() == io::ErrorKind::Interrupted {
                return Ok(None);
            }
            return Err(e);
        }
        if r == 0 {
            return Ok(None);
        }
        let mut buf = [0u8; 4096];
        // SAFETY: reading at most `buf.len()` bytes into a live buffer.
        let n = unsafe {
            libc::recv(
                fd.as_raw_fd(),
                buf.as_mut_ptr() as *mut libc::c_void,
                buf.len(),
                0,
            )
        };
        if n < 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(Some(
            String::from_utf8_lossy(&buf[..n as usize])
                .trim_end()
                .to_string(),
        ))
    }
}

#[cfg(target_os = "linux")]
pub use control::Control;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_command_strings_match_the_control_interface() {
        assert_eq!(Command::wifi_display_enable(), "SET wifi_display 1");
        assert_eq!(
            Command::subelement(0, "00060011 1c44 000a"),
            "WFD_SUBELEM_SET 0 00060011 1c44 000a"
        );
        assert_eq!(
            Command::group_add_persistent(2437),
            "P2P_GROUP_ADD persistent freq=2437"
        );
        assert_eq!(
            Command::group_add_stored(3, 2437),
            "P2P_GROUP_ADD persistent=3 freq=2437"
        );
        assert_eq!(Command::list_networks(), "LIST_NETWORKS");
        assert_eq!(Command::wps_pin("12345670"), "WPS_PIN any 12345670");
        assert_eq!(
            stored_persistent_group(
                "network id / ssid / bssid / flags\n\
                 0\tDIRECT-ab\tany\t[P2P-PERSISTENT]\n"
            ),
            Some(0),
            "a stored group must be found, or every restart re-pairs"
        );
        assert_eq!(
            stored_persistent_group(
                "network id / ssid / bssid / flags\n\
                 0\thome-wifi\tany\t[CURRENT]\n\
                 2\tDIRECT-xy\tany\t[P2P-PERSISTENT]\n"
            ),
            Some(2),
            "an ordinary saved network is not a persistent group"
        );
        assert_eq!(
            stored_persistent_group("network id / ssid / bssid / flags\n"),
            None
        );
        assert_eq!(stored_persistent_group(""), None);
        assert_eq!(
            Command::group_remove("p2p-wlan0-0"),
            "P2P_GROUP_REMOVE p2p-wlan0-0"
        );
    }

    #[test]
    fn a_group_started_event_yields_the_interface_and_role() {
        let e = parse_event(
            "<3>P2P-GROUP-STARTED p2p-wlan0-0 GO ssid=\"DIRECT-xy\" freq=2437 passphrase=\"secret\" go_dev_addr=02:11:22:33:44:55",
        )
        .expect("event");
        assert_eq!(
            e,
            Event::GroupStarted {
                interface: "p2p-wlan0-0".into(),
                go: true,
                freq_mhz: 2437
            }
        );
    }

    #[test]
    fn a_client_joining_yields_its_address() {
        let e = parse_event("<3>AP-STA-CONNECTED 02:aa:bb:cc:dd:ee p2p_dev_addr=02:11:22:33:44:55")
            .expect("event");
        assert_eq!(
            e,
            Event::ClientConnected {
                mac: "02:aa:bb:cc:dd:ee".into()
            }
        );
    }

    #[test]
    fn a_client_leaving_is_recognised() {
        let e = parse_event("<3>AP-STA-DISCONNECTED 02:aa:bb:cc:dd:ee").expect("event");
        assert_eq!(
            e,
            Event::ClientDisconnected {
                mac: "02:aa:bb:cc:dd:ee".into()
            }
        );
    }

    #[test]
    fn a_provision_discovery_request_asks_us_for_a_pin() {
        let e = parse_event(
            "<3>P2P-PROV-DISC-PBC-REQ 02:11:22:33:44:55 p2p_dev_addr=02:11:22:33:44:55 name='PC'",
        )
        .expect("event");
        assert_eq!(
            e,
            Event::ProvisionRequest {
                peer: "02:11:22:33:44:55".into()
            }
        );
    }

    #[test]
    fn group_removal_and_wps_success_are_recognised() {
        assert_eq!(
            parse_event("<3>P2P-GROUP-REMOVED p2p-wlan0-0 GO reason=REQUESTED"),
            Some(Event::GroupRemoved {
                interface: "p2p-wlan0-0".into()
            })
        );
        assert_eq!(parse_event("<3>WPS-SUCCESS"), Some(Event::WpsSuccess));
        assert_eq!(
            parse_event("<3>WPS-FAIL msg=8 config_error=0"),
            Some(Event::WpsFail)
        );
    }

    #[test]
    fn an_unknown_event_is_none_not_a_panic() {
        assert!(parse_event("<3>CTRL-EVENT-SCAN-STARTED ").is_none());
        assert!(parse_event("").is_none());
        assert!(parse_event("<3>").is_none());
    }

    #[test]
    fn a_priority_prefix_is_optional() {
        assert!(parse_event("AP-STA-DISCONNECTED 02:aa:bb:cc:dd:ee").is_some());
    }
}
