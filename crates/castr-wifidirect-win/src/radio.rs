//! The WinRT calls, and nothing else.
//!
//! Every decision this makes is made in `select` or `failure`, which are pure
//! and tested; what is left here cannot be tested without a radio and a display
//! in the room, so there is deliberately as little of it as possible.
//!
//! The calls below were all run against real hardware on 2026-09-04 before any
//! of this was written, so they are known to work rather than merely plausible.

use crate::failure::{self, Stage};
use crate::select::{self, Candidate, NoMatch, WaitPolicy};
use anyhow::{bail, Context};
use castr_miracast::wfd;
use std::net::IpAddr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use windows::core::HSTRING;
use windows::Devices::Enumeration::{
    DeviceInformation, DeviceInformationCustomPairing, DevicePairingKinds,
    DevicePairingProtectionLevel, DevicePairingRequestedEventArgs, DevicePairingResultStatus,
};
use windows::Devices::WiFiDirect::{
    WiFiDirectConfigurationMethod, WiFiDirectConnectionParameters, WiFiDirectConnectionStatus,
    WiFiDirectDevice, WiFiDirectDeviceSelectorType, WiFiDirectInformationElement,
};
use windows::Foundation::TypedEventHandler;
use windows::Storage::Streams::{DataReader, IBuffer};

/// How long to keep asking for an address after the group comes up. DHCP has to
/// complete first, so an empty answer immediately afterwards means nothing.
const ADDRESS_TIMEOUT: Duration = Duration::from_secs(10);
/// How often to re-scan while waiting for a display to start advertising.
const SCAN_INTERVAL: Duration = Duration::from_secs(3);

fn buffer_bytes(buf: &IBuffer) -> windows::core::Result<Vec<u8>> {
    let reader = DataReader::FromBuffer(buf)?;
    let mut v = vec![0u8; buf.Length()? as usize];
    reader.ReadBytes(&mut v)?;
    Ok(v)
}

/// The Wi-Fi Display capabilities a device is advertising, if it is advertising
/// any.
///
/// The element's value is a list of subelements: a one-byte id, a two-byte
/// length, then the body. Only the device-information subelement is read here;
/// it is the one that says whether this is a display at all.
fn display_caps(info: &DeviceInformation) -> Option<wfd::DeviceCaps> {
    let elements = WiFiDirectInformationElement::CreateFromDeviceInformation(info).ok()?;
    for e in &elements {
        let oui = e.Oui().ok().and_then(|b| buffer_bytes(&b).ok())?;
        if oui.as_slice() != wfd::WFD_OUI || e.OuiType().ok()? != wfd::WFD_OUI_TYPE {
            continue;
        }
        let value = e.Value().ok().and_then(|b| buffer_bytes(&b).ok())?;
        if value.len() >= 3 && value[0] == 0 {
            return wfd::parse_device_info(&value[3..]);
        }
    }
    None
}

/// How this device is willing to be paired with, from its WPS element.
///
/// Read from the same beacon as the Wi-Fi Display element and before
/// connecting, so the ceremony can be chosen rather than assumed.
fn pairing_methods(info: &DeviceInformation) -> Option<wfd::ConfigMethods> {
    let elements = WiFiDirectInformationElement::CreateFromDeviceInformation(info).ok()?;
    for e in &elements {
        let Some(oui) = e.Oui().ok().and_then(|b| buffer_bytes(&b).ok()) else {
            continue;
        };
        if oui.as_slice() != wfd::WPS_OUI || e.OuiType().unwrap_or(0) != wfd::WPS_OUI_TYPE {
            continue;
        }
        let Some(value) = e.Value().ok().and_then(|b| buffer_bytes(&b).ok()) else {
            continue;
        };
        if let Some(m) = wfd::parse_config_methods(&value) {
            return Some(m);
        }
    }
    None
}

/// Everything Wi-Fi Direct can currently see, display or not.
///
/// **Slow**: about 50 seconds against the four devices in range here, and the
/// same on any thread. Roughly 10 s of that is the enumeration; the rest is
/// reading each device's information element, one device at a time.
///
/// Measured on 2026-09-04 rather than assumed, because a caller with a user
/// interface has to run this on a worker *and say what it is waiting for* - a
/// silent minute is indistinguishable from a hang, and was briefly mistaken
/// for one.
pub fn discover() -> anyhow::Result<Vec<Candidate>> {
    let selector =
        WiFiDirectDevice::GetDeviceSelector2(WiFiDirectDeviceSelectorType::AssociationEndpoint)
            .context("building the Wi-Fi Direct selector")?;
    let found = DeviceInformation::FindAllAsyncAqsFilter(&selector)
        .context("enumerating Wi-Fi Direct devices")?
        .get()
        .context("enumerating Wi-Fi Direct devices")?;

    let mut out = Vec::new();
    for d in &found {
        out.push(Candidate {
            id: d.Id()?.to_string(),
            name: d.Name()?.to_string(),
            caps: display_caps(&d),
            pairing: pairing_methods(&d),
        });
    }
    Ok(out)
}

/// A live Wi-Fi Direct group.
///
/// The group lives exactly as long as this object: dropping it is the teardown,
/// so there is no disconnect call anyone can forget. A group that outlives what
/// created it leaves a peer holding credentials for something that is gone,
/// which is a failure this project has already paid for twice.
pub struct Connection {
    device: WiFiDirectDevice,
    remote: IpAddr,
    rtsp_port: u16,
    name: String,
    caps: Option<castr_miracast::wfd::DeviceCaps>,
    up: Arc<AtomicBool>,
}

impl Connection {
    pub fn remote_ip(&self) -> IpAddr {
        self.remote
    }

    pub fn rtsp_port(&self) -> u16 {
        self.rtsp_port
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    /// What the display's own advertisement said it can carry. Worth more than
    /// a guess: it is the display's own number, read before connecting.
    pub fn max_throughput_mbps(&self) -> Option<u16> {
        self.caps.map(|c| c.max_throughput_mbps)
    }

    /// False once the radio has seen the display go away. A cast watching this
    /// learns of a television switched off at once, rather than when a
    /// keep-alive expires some seconds later.
    pub fn is_up(&self) -> bool {
        self.up.load(Ordering::SeqCst)
    }
}

impl Drop for Connection {
    fn drop(&mut self) {
        // Windows keeps the group for about a minute after the owning handle
        // goes; that is its business, not a leak of ours.
        tracing::info!("wifidirect: releasing the group with {:?}", self.name);
        let _ = &self.device;
    }
}

/// How the caller asks somebody for the PIN a display is showing.
///
/// Shared and thread-safe rather than a borrowed closure, because it is called
/// from a WinRT callback thread during the pairing rather than from the thread
/// that started it. Keeping it a callback at all is what lets a graphical
/// sender substitute a dialog without touching this crate.
pub type PinSource = Arc<dyn Fn() -> anyhow::Result<String> + Send + Sync>;

/// Finds a display by name, pairs if it must, and brings up the group.
///
/// `pin` is called only when the display has to be paired with, so a display
/// Windows already knows connects silently.
pub fn connect(
    name: &str,
    wait: WaitPolicy,
    pin: &PinSource,
    preference: select::Preference,
) -> anyhow::Result<Connection> {
    let target = find(name, wait)?;
    let was_paired = DeviceInformation::CreateFromIdAsync(&HSTRING::from(&target.id))
        .and_then(|op| op.get())
        .and_then(|info| info.Pairing()?.IsPaired())
        .unwrap_or(false);

    match bring_up(&target, pin, preference) {
        Ok(c) => Ok(c),
        Err(e) => {
            // Credentials for a group that no longer exists look exactly like a
            // network that is not there, and produce an endless "connecting"
            // with nothing anywhere to explain it.
            //
            // Retrying from scratch used to be gated on the radio's own words
            // matching a known phrase, because re-pairing cost somebody a PIN
            // prompt. A wireless display adapter turned out to invalidate its
            // pairing after every session, and to describe that as "the
            // operation was cancelled" - which matches no phrase at all.
            //
            // So the gate is now the *cost* rather than the wording: when the
            // display pairs by button, re-pairing is free and unattended, and
            // there is no reason not to try. A display that would prompt for a
            // PIN still needs the wording to justify the interruption.
            let reason = last_wlan_failure().unwrap_or_default();
            let repairing_is_free = matches!(
                select::choose_ceremony(target.pairing, preference),
                select::Ceremony::PushButton
            );
            if was_paired && (repairing_is_free || failure::looks_like_stale_credentials(&reason)) {
                tracing::warn!(
                    "wifidirect: association failed ({reason}); forgetting the stored pairing \
                     and trying once more"
                );
                unpair(&target)?;
                return bring_up(&target, pin, preference);
            }
            Err(e)
        }
    }
}

/// Polls until the named display is advertising, or the policy gives up.
fn find(name: &str, wait: WaitPolicy) -> anyhow::Result<Candidate> {
    let start = Instant::now();
    let mut announced = false;
    loop {
        let candidates = discover()?;
        match select::match_by_name(&candidates, name) {
            Ok(c) => return Ok(c),
            Err(NoMatch::NotFound) if wait.keep_waiting(start) => {
                if !announced {
                    announced = true;
                    tracing::info!(
                        "wifidirect: waiting for {name:?} - open Screen Mirroring on it"
                    );
                }
                std::thread::sleep(SCAN_INTERVAL);
            }
            Err(e) => bail!("{e}"),
        }
    }
}

fn unpair(target: &Candidate) -> anyhow::Result<()> {
    let info = DeviceInformation::CreateFromIdAsync(&HSTRING::from(&target.id))?.get()?;
    let status = info.Pairing()?.UnpairAsync()?.get()?.Status()?;
    tracing::info!("wifidirect: unpaired {:?} -> {status:?}", target.name);
    Ok(())
}

fn bring_up(
    target: &Candidate,
    pin: &PinSource,
    preference: select::Preference,
) -> anyhow::Result<Connection> {
    let id = HSTRING::from(&target.id);
    let info = DeviceInformation::CreateFromIdAsync(&id)?
        .get()
        .with_context(|| format!("{}: reading {:?}", Stage::Discovery, target.name))?;

    if !info.Pairing()?.IsPaired()? {
        let ceremony = select::choose_ceremony(target.pairing, preference);
        tracing::info!(
            "wifidirect: pairing with {:?} by {ceremony:?}; it offers {}",
            target.name,
            target
                .pairing
                .map(|m| m.describe())
                .unwrap_or_else(|| "nothing it will admit to".into())
        );
        match ceremony {
            select::Ceremony::PushButton => pair_with_push_button(&info, target)?,
            select::Ceremony::Pin => pair_with_pin(&info, target, pin)?,
        }
    }

    let device = WiFiDirectDevice::FromIdAsync(&id)?
        .get()
        .with_context(|| {
            let reason = last_wlan_failure()
                .map(|r| format!(" ({r})"))
                .unwrap_or_default();
            format!(
                "{}: could not form a group with {:?}{reason}",
                Stage::Association,
                target.name
            )
        })?;

    let up = Arc::new(AtomicBool::new(true));
    let flag = up.clone();
    let watched = target.name.clone();
    device.ConnectionStatusChanged(&TypedEventHandler::<WiFiDirectDevice, _>::new(
        move |sender, _| {
            if let Some(d) = sender.as_ref() {
                if d.ConnectionStatus()? == WiFiDirectConnectionStatus::Disconnected {
                    tracing::warn!("wifidirect: {watched:?} went away");
                    flag.store(false, Ordering::SeqCst);
                }
            }
            Ok(())
        },
    ))?;

    let remote = address_of(&device, target)?;
    let rtsp_port = target.caps.map(|c| c.rtsp_port).unwrap_or(7236);
    tracing::info!(
        "wifidirect: {:?} is up at {remote}, RTSP on {rtsp_port}",
        target.name
    );
    Ok(Connection {
        device,
        remote,
        rtsp_port,
        name: target.name.clone(),
        caps: target.caps,
        up,
    })
}

/// Pairs by confirmation, with no PIN for anyone to read.
///
/// What a wireless display adapter actually wants: it shows a "ready to
/// connect" screen and mints a PIN per attempt that is often never displayed,
/// so the PIN ceremony cannot be completed by a person looking at it. The
/// button is metaphorical here - Windows confirms on our behalf - which is why
/// this needs nobody present at all.
fn pair_with_push_button(info: &DeviceInformation, target: &Candidate) -> anyhow::Result<()> {
    let params = WiFiDirectConnectionParameters::new()?;
    params.SetGroupOwnerIntent(0)?;
    let methods = params.PreferenceOrderedConfigurationMethods()?;
    methods.Clear()?;
    methods.Append(WiFiDirectConfigurationMethod::PushButton)?;

    let custom: DeviceInformationCustomPairing = info.Pairing()?.Custom()?;
    custom.PairingRequested(&TypedEventHandler::<
        DeviceInformationCustomPairing,
        DevicePairingRequestedEventArgs,
    >::new(move |_, args| {
        if let Some(args) = args.as_ref() {
            args.Accept()?;
        }
        Ok(())
    }))?;

    let result = custom
        .PairWithProtectionLevelAndSettingsAsync(
            DevicePairingKinds::ConfirmOnly,
            DevicePairingProtectionLevel::Default,
            &params,
        )?
        .get()?;
    let status = result.Status()?;
    if status != DevicePairingResultStatus::Paired
        && status != DevicePairingResultStatus::AlreadyPaired
    {
        let reason = last_wlan_failure()
            .map(|r| format!("; the radio said: {r}"))
            .unwrap_or_default();
        bail!(
            "{}: {:?} - {}{reason}. If the display has a physical button for \
             pairing, press it and try again",
            Stage::Pairing,
            target.name,
            failure::pairing_status(status.0)
        );
    }
    Ok(())
}

fn pair_with_pin(
    info: &DeviceInformation,
    target: &Candidate,
    pin: &PinSource,
) -> anyhow::Result<()> {
    let params = WiFiDirectConnectionParameters::new()?;
    // Intent zero: the display should own the group. That is how a Miracast
    // sink expects to run, and it is what our own sink is set up for.
    params.SetGroupOwnerIntent(0)?;
    let methods = params.PreferenceOrderedConfigurationMethods()?;
    methods.Clear()?;
    methods.Append(WiFiDirectConfigurationMethod::ProvidePin)?;

    // Anything the prompt refused, kept so it can be reported after the
    // pairing has failed rather than swallowed inside a callback.
    let refusal: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));

    let ask = pin.clone();
    let seen = refusal.clone();
    let custom: DeviceInformationCustomPairing = info.Pairing()?.Custom()?;
    custom.PairingRequested(&TypedEventHandler::<
        DeviceInformationCustomPairing,
        DevicePairingRequestedEventArgs,
    >::new(move |_, args| {
        let Some(args) = args.as_ref() else {
            return Ok(());
        };
        // The PIN is asked for *here*, not before the pairing starts.
        //
        // A display is only told to show a PIN once a pairing is actually
        // under way. Asking first prompts for a number that does not exist
        // yet - invisible against a sink that displays one permanently, as
        // ours does, and fatal against one that does not. A wireless display
        // adapter showed exactly that: it advertises Display, and had no PIN
        // on screen when we asked.
        //
        // The prompt blocks while somebody reads the display, so this takes a
        // deferral: without one Windows treats the handler as finished the
        // moment it returns and stops waiting for an answer.
        let deferral = args.GetDeferral()?;
        match ask() {
            Ok(digits) => args.AcceptWithPin(&HSTRING::from(&digits))?,
            Err(e) => {
                // Not accepting is what fails the pairing, which is the right
                // outcome for a cancelled prompt.
                *seen.lock().unwrap() = Some(format!("{e:#}"));
            }
        }
        deferral.Complete()?;
        Ok(())
    }))?;

    let result = custom
        .PairWithProtectionLevelAndSettingsAsync(
            DevicePairingKinds::ProvidePin,
            DevicePairingProtectionLevel::Default,
            &params,
        )?
        .get()?;
    if let Some(why) = refusal.lock().unwrap().take() {
        bail!("{}: {:?} - {why}", Stage::Pairing, target.name);
    }
    let status = result.Status()?;
    if status != DevicePairingResultStatus::Paired
        && status != DevicePairingResultStatus::AlreadyPaired
    {
        let reason = last_wlan_failure()
            .map(|r| format!("; the radio said: {r}"))
            .unwrap_or_default();
        bail!(
            "{}: {:?} - {}{reason}",
            Stage::Pairing,
            target.name,
            failure::pairing_status(status.0)
        );
    }
    Ok(())
}

/// The display's address, once DHCP has given it one.
fn address_of(device: &WiFiDirectDevice, target: &Candidate) -> anyhow::Result<IpAddr> {
    let deadline = Instant::now() + ADDRESS_TIMEOUT;
    loop {
        let pairs = device.GetConnectionEndpointPairs()?;
        for p in &pairs {
            // Both ends are logged, not just the one we use. Which side owns
            // the group decides who is at `.1`, and taking the remote on faith
            // is how a source ends up connecting to itself and reporting the
            // display as refusing it.
            let local = p
                .LocalHostName()
                .and_then(|h| h.ToString())
                .map(|s| s.to_string())
                .unwrap_or_else(|_| "?".into());
            if let Ok(host) = p.RemoteHostName() {
                if let Ok(text) = host.ToString() {
                    tracing::info!("wifidirect: endpoint local={local} remote={text}");
                    if let Ok(ip) = text.to_string().parse::<IpAddr>() {
                        return Ok(ip);
                    }
                }
            }
        }
        if Instant::now() >= deadline {
            bail!(
                "{}: {:?} joined but never offered an address. Its DHCP server may not be running",
                Stage::Address,
                target.name
            );
        }
        std::thread::sleep(Duration::from_millis(250));
    }
}

/// The most recent WLAN AutoConfig connection failure, as Windows recorded it.
///
/// Shelling out to `wevtutil` rather than binding the event-log API: it is on
/// every Windows, this runs once on a failure path, and the parsing it feeds is
/// tested against a real log.
fn last_wlan_failure() -> Option<String> {
    let out = std::process::Command::new("wevtutil")
        .args([
            "qe",
            "Microsoft-Windows-WLAN-AutoConfig/Operational",
            "/q:*[System[(EventID=8002)]]",
            "/c:1",
            "/rd:true",
            "/f:text",
        ])
        .output()
        .ok()?;
    failure::parse_wlan_failure(&String::from_utf8_lossy(&out.stdout))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Needs a radio, so it is not part of the ordinary run:
    /// `cargo test -p castr-wifidirect-win -- --ignored --nocapture`.
    ///
    /// Two things, neither of them obvious:
    ///
    /// - Discovery works off the main thread, which a graphical sender needs.
    /// - It takes about **50 seconds**. That is the point of the generous
    ///   bound below: a tighter one fails on a working radio, and reads as a
    ///   hang when it is only slow. It was read as exactly that once already.
    #[test]
    #[ignore]
    fn discovery_finishes_on_a_worker_thread_though_slowly() {
        let started = Instant::now();
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let _ = tx.send(discover().map(|v| v.len()));
        });
        match rx.recv_timeout(Duration::from_secs(180)) {
            Ok(Ok(n)) => println!("found {n} devices in {:?} from a worker", started.elapsed()),
            Ok(Err(e)) => panic!("discovery failed on a worker thread: {e:#}"),
            Err(_) => panic!("discovery did not finish within 180s, which is beyond slow"),
        }
    }
}

#[cfg(test)]
mod unpair_probe {
    use super::*;

    /// Forget a display, so the next connection pairs from scratch.
    ///
    /// Diagnostic for a display that advertises but will not associate: the
    /// stored credentials may be for a group that no longer exists, which
    /// `connect` only retries automatically when the radio's own words match
    /// the stale-credential heuristic - and "the operation was cancelled" does
    /// not.
    ///
    /// `cargo test -p castr-wifidirect-win unpair_probe -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn forget_the_display() {
        let name = std::env::var("CASTR_PROBE_DISPLAY").unwrap_or_else(|_| "MR-A202".into());
        let target = find(&name, WaitPolicy::new(Duration::from_secs(90))).expect("find");
        let info = DeviceInformation::CreateFromIdAsync(&HSTRING::from(&target.id))
            .unwrap()
            .get()
            .unwrap();
        println!(
            "before: paired={:?} can_pair={:?}",
            info.Pairing().and_then(|p| p.IsPaired()),
            info.Pairing().and_then(|p| p.CanPair())
        );
        match unpair(&target) {
            Ok(()) => println!("unpaired {name}"),
            Err(e) => println!("unpair failed: {e:#}"),
        }
        let after = DeviceInformation::CreateFromIdAsync(&HSTRING::from(&target.id))
            .unwrap()
            .get()
            .unwrap();
        println!(
            "after:  paired={:?} can_pair={:?}",
            after.Pairing().and_then(|p| p.IsPaired()),
            after.Pairing().and_then(|p| p.CanPair())
        );
    }
}
