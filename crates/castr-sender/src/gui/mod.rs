//! The sender's window: one list of everywhere the screen can go.
//!
//! The decisions live in [`targets`] and [`pin`], which are pure and tested.
//! What is here is egui and threads, and it cannot be verified without a
//! person clicking it - see the verification document rather than trusting
//! this file's comments.

mod pin;
mod session;
mod targets;
mod wifi;

use crate::cast::*;
use crate::control::server::Published;
use castr_net::ReceiverInfo;
use castr_proto::Mode;
use eframe::egui;
use pin::PinKind;
use session::{RunState, Session};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use targets::{DisplayInfo, Target, TargetId};
use tokio::sync::{mpsc, watch};

struct Shared {
    receivers: Vec<ReceiverInfo>,
    displays: Vec<DisplayInfo>,
    /// Tracked separately because the two scans are independent and the radio
    /// one is much the slower. Advice about an empty list is wrong until both
    /// have finished, and "no displays found" shown while the radio is still
    /// looking is worse than saying nothing.
    scanning_receivers: bool,
    scanning_displays: bool,
    /// Whether both have finished at least once.
    scanned: bool,
    message: String,
    /// Armed only while something is actually waiting for a PIN.
    pin_tx: Option<std::sync::mpsc::Sender<String>>,
    pin_target: Option<String>,
    pin_kind: PinKind,
}

struct App {
    rt: tokio::runtime::Handle,
    shared: Arc<Mutex<Shared>>,
    config_dir: PathBuf,
    sender_name: String,
    selected: Option<TargetId>,
    mode: Mode,
    pin_input: String,
    active: Option<Session>,
    /// `(index, label)` per monitor, empty where enumeration is unavailable.
    monitors: Vec<(u32, String)>,
    monitor: u32,
    wifi: wifi::Panel,
}

impl App {
    fn scan(&self) {
        let shared = self.shared.clone();
        {
            let mut s = shared.lock().unwrap();
            s.scanning_receivers = true;
            s.scanning_displays = cfg!(windows);
        }
        // castr receivers, over the ordinary network.
        {
            let shared = shared.clone();
            self.rt.spawn(async move {
                let found = discover(Duration::from_secs(2)).await;
                let mut s = shared.lock().unwrap();
                match found {
                    Ok(found) => s.receivers = found,
                    Err(e) => s.message = format!("Looking for receivers failed: {e:#}"),
                }
                s.scanning_receivers = false;
                s.scanned = !s.scanning_displays;
            });
        }
        // Miracast displays, over the radio. A separate thread because the
        // enumeration blocks, and separate from the receiver scan because
        // neither should wait for the other.
        #[cfg(windows)]
        std::thread::spawn(move || {
            let found = castr_wifidirect_win::radio::discover();
            let mut s = shared.lock().unwrap();
            match found {
                Ok(found) => {
                    s.displays = found
                        .into_iter()
                        .filter(|c| c.is_display())
                        .filter_map(|c| {
                            let caps = c.caps?;
                            Some(DisplayInfo {
                                id: c.id,
                                name: c.name,
                                max_mbps: caps.max_throughput_mbps,
                                hdcp: caps.content_protection,
                            })
                        })
                        .collect()
                }
                // Never swallowed. A scan that fails silently is
                // indistinguishable from a world with no displays in it, and
                // this project has already lost days to exactly that.
                Err(e) => s.message = format!("Looking for displays failed: {e:#}"),
            }
            s.scanning_displays = false;
            s.scanned = !s.scanning_receivers;
        });
    }

    fn start_pair(&mut self, target: ReceiverInfo) {
        let (pin_tx, pin_rx) = std::sync::mpsc::channel::<String>();
        {
            let mut s = self.shared.lock().unwrap();
            s.pin_tx = Some(pin_tx);
            s.pin_target = Some(target.name.clone());
            s.pin_kind = PinKind::Castr;
        }
        let shared = self.shared.clone();
        let dir = self.config_dir.clone();
        self.rt.spawn(async move {
            let name = target.name.clone();
            // `read_pin` below blocks this runtime worker thread until the
            // user submits a PIN through the window (or cancels, which drops
            // `pin_tx` and makes `recv()` return an error). That is acceptable
            // because the runtime is multi-threaded; if the window is closed
            // while this is pending, `App::on_exit` takes and drops
            // `shared.pin_tx`, closing the channel and releasing the thread.
            let result = pair_interactive(&target, &dir, move || {
                pin_rx
                    .recv()
                    .map_err(|_| anyhow::anyhow!("PIN entry cancelled"))
            })
            .await;
            let msg = match result {
                Ok(()) => format!("Paired with {name}"),
                Err(e) => format!("Pairing failed: {e:#}"),
            };
            let mut s = shared.lock().unwrap();
            s.message = msg;
            s.pin_tx = None;
            s.pin_target = None;
        });
    }

    fn cast_to_receiver(&mut self, target: ReceiverInfo) {
        let (cmd_tx, cmd_rx) = mpsc::channel(4);
        let (status_tx, status_rx) = watch::channel(CastStatus::default());
        let opts = CastOptions {
            target: target.name.clone(),
            mode: self.mode,
            fps: 30,
            max_bitrate: None,
            sender_name: self.sender_name.clone(),
            config_dir: self.config_dir.clone(),
        };
        let shared = self.shared.clone();
        self.rt.spawn(async move {
            if let Err(e) = cast(opts, cmd_rx, status_tx).await {
                shared.lock().unwrap().message = format!("Cast ended: {e:#}");
            }
        });
        self.active = Some(Session::Castr {
            cmd: cmd_tx,
            status: status_rx,
        });
    }

    #[cfg(windows)]
    fn cast_to_display(&mut self, display: DisplayInfo) {
        // One cast at a time, counting one started from a terminal. A stale
        // record is cleaned up by `running` rather than blocking us.
        if let Some(other) = crate::control::client::running(&self.config_dir) {
            self.shared.lock().unwrap().message = format!(
                "Already casting to {:?}. Stop that first.",
                other.display
            );
            return;
        }

        let (cmd_tx, cmd_rx) = std::sync::mpsc::channel();
        let published: Published = Arc::new(Mutex::new(None));
        let state = Arc::new(Mutex::new(RunState {
            stage: format!("Looking for {}...", display.name),
            ..RunState::default()
        }));

        let shared = self.shared.clone();
        let config_dir = self.config_dir.clone();
        let name = display.name.clone();
        let mode = self.mode;
        let monitor = self.monitor;
        let state_w = state.clone();
        let published_w = published.clone();
        let cmd_w = cmd_tx.clone();

        let worker = std::thread::Builder::new()
            .name("miracast-gui-cast".into())
            .spawn(move || {
                let set_stage = {
                    let state = state_w.clone();
                    move |text: String| {
                        if let Ok(mut s) = state.lock() {
                            s.stage = text;
                        }
                    }
                };
                // Armed only when the radio actually asks, which is once the
                // pairing is under way and the display has been told to show a
                // PIN - so the box never appears before there is a number on
                // screen to copy, and never at all for a display Windows
                // already knows.
                //
                // Owns its own clones because the radio calls it from a WinRT
                // callback thread, not from this one.
                let ask_pin = {
                    let shared = shared.clone();
                    let state = state_w.clone();
                    let name = name.clone();
                    move || -> anyhow::Result<String> {
                        let (tx, rx) = std::sync::mpsc::channel::<String>();
                        {
                            let mut s = shared.lock().unwrap();
                            s.pin_tx = Some(tx);
                            s.pin_target = Some(name.clone());
                            s.pin_kind = PinKind::Miracast;
                        }
                        if let Ok(mut s) = state.lock() {
                            s.stage = "Waiting for the PIN shown on the display".into();
                        }
                        let pin = rx
                            .recv()
                            .map_err(|_| anyhow::anyhow!("pairing: PIN entry was cancelled"));
                        {
                            let mut s = shared.lock().unwrap();
                            s.pin_tx = None;
                            s.pin_target = None;
                        }
                        pin
                    }
                };

                let result = (|| -> anyhow::Result<()> {
                    let wait = castr_wifidirect_win::select::WaitPolicy::new(
                        Duration::from_secs(60),
                    );
                    let ask: castr_wifidirect_win::radio::PinSource = Arc::new(ask_pin);
                    let connection =
                        castr_wifidirect_win::radio::connect(&name, wait, &ask)?;
                    set_stage(format!("Connected to {name}; negotiating"));
                    let addr = std::net::SocketAddr::new(
                        connection.remote_ip(),
                        connection.rtsp_port(),
                    );
                    let opts = crate::miracast_cast::MiracastOptions {
                        duration: None,
                        output: monitor,
                        fps: 30,
                        mode,
                        ceiling_mbps: connection.max_throughput_mbps(),
                        display: name.clone(),
                        config_dir,
                    };
                    let outcome = crate::miracast_cast::cast_to(addr, opts, cmd_w, cmd_rx);
                    // The group goes when this does, which is the teardown.
                    drop(connection);
                    outcome
                })();

                if let Ok(mut s) = state_w.lock() {
                    s.finished = true;
                    if let Err(e) = &result {
                        s.error = Some(format!("{e:#}"));
                    }
                }
                // A failure while the PIN box was up must take the box down.
                let mut s = shared.lock().unwrap();
                s.pin_tx = None;
                s.pin_target = None;
                let _ = &published_w;
            })
            .ok();

        self.active = Some(Session::Miracast {
            cmd: cmd_tx,
            published,
            state,
            worker,
        });
    }

    #[cfg(not(windows))]
    fn cast_to_display(&mut self, _display: DisplayInfo) {
        self.shared.lock().unwrap().message =
            "Casting to a Miracast display is Windows only.".into();
    }
}

impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        ctx.request_repaint_after(Duration::from_millis(250));
        let (
            receivers,
            displays,
            scanning,
            scanning_displays,
            scanned,
            message,
            pin_target,
            pin_active,
            pin_kind,
        ) = {
            let s = self.shared.lock().unwrap();
            (
                s.receivers.clone(),
                s.displays.clone(),
                s.scanning_receivers || s.scanning_displays,
                s.scanning_displays,
                s.scanned,
                s.message.clone(),
                s.pin_target.clone(),
                s.pin_tx.is_some(),
                s.pin_kind,
            )
        };
        let list = targets::merge(&receivers, &displays);

        // A cast that ended on its own puts the window back to idle, and says
        // why if it ended badly.
        if let Some(a) = &self.active {
            if a.finished() {
                if let Some(e) = a.error() {
                    self.shared.lock().unwrap().message = format!("Cast ended: {e}");
                }
                self.active = None;
            }
        }

        egui::CentralPanel::default().show(ctx, |ui| {
            ui.heading("castr");
            ui.horizontal(|ui| {
                if ui
                    .add_enabled(!scanning, egui::Button::new("Scan"))
                    .clicked()
                {
                    self.scan();
                }
                if scanning {
                    ui.spinner();
                }
                self.wifi.button(ui);
            });
            ui.separator();

            for target in &list {
                let id = target.id();
                let checked = self.selected.as_ref() == Some(&id);
                if ui.selectable_label(checked, target.label()).clicked() {
                    self.selected = Some(id);
                }
            }
            // The radio enumeration takes about a minute, and a silent minute
            // reads as a hang - it was mistaken for one during development.
            if scanning_displays {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label("Looking for Miracast displays - this takes about a minute");
                });
            }
            if let Some(text) = targets::advice(&list, scanned) {
                ui.label(text);
            }

            self.wifi.show(ui);

            ui.separator();
            ui.horizontal(|ui| {
                ui.label("Mode:");
                if ui
                    .selectable_value(&mut self.mode, Mode::Game, "Game")
                    .clicked()
                {
                    if let Some(Session::Castr { cmd, .. }) = &self.active {
                        let _ = cmd.try_send(CastCommand::SetMode(Mode::Game));
                    }
                }
                if ui
                    .selectable_value(&mut self.mode, Mode::Quality, "Quality")
                    .clicked()
                {
                    if let Some(Session::Castr { cmd, .. }) = &self.active {
                        let _ = cmd.try_send(CastCommand::SetMode(Mode::Quality));
                    }
                }
            });

            // Which monitor. Only worth showing when there is a choice, and
            // locked while a cast is running because it is fixed at start.
            if self.monitors.len() > 1 {
                ui.horizontal(|ui| {
                    ui.label("Screen:");
                    let current = self
                        .monitors
                        .iter()
                        .find(|(i, _)| *i == self.monitor)
                        .map(|(_, l)| l.clone())
                        .unwrap_or_else(|| "?".into());
                    ui.add_enabled_ui(self.active.is_none(), |ui| {
                        egui::ComboBox::from_id_source("monitor")
                            .selected_text(current)
                            .show_ui(ui, |ui| {
                                for (index, label) in &self.monitors {
                                    ui.selectable_value(&mut self.monitor, *index, label);
                                }
                            });
                    });
                });
            }

            let selected = self
                .selected
                .as_ref()
                .and_then(|id| targets::position(&list, id))
                .and_then(|i| list.get(i).cloned());

            if pin_active {
                let name = pin_target.unwrap_or_default();
                ui.label(pin::prompt(pin_kind, &name));
                ui.horizontal(|ui| {
                    let width = 16.0 + 12.0 * pin_kind.digits() as f32;
                    if ui
                        .add(
                            egui::TextEdit::singleline(&mut self.pin_input)
                                .desired_width(width),
                        )
                        .changed()
                    {
                        self.pin_input = pin::sanitise(&self.pin_input, pin_kind);
                    }
                    let ready = pin::is_complete(&self.pin_input, pin_kind);
                    if ui.add_enabled(ready, egui::Button::new("Submit")).clicked() {
                        let mut s = self.shared.lock().unwrap();
                        if let Some(tx) = &s.pin_tx {
                            let _ = tx.send(self.pin_input.clone());
                        }
                        s.pin_tx = None;
                        drop(s);
                        self.pin_input.clear();
                    }
                    if ui.button("Cancel").clicked() {
                        // Dropping the sender closes the channel; whoever is
                        // waiting on `recv()` then errors out, which fails the
                        // pairing cleanly.
                        let mut s = self.shared.lock().unwrap();
                        s.pin_tx = None;
                        drop(s);
                        self.pin_input.clear();
                    }
                });
            } else {
                match &self.active {
                    None => {
                        let actions = targets::actions_for(selected.as_ref());
                        ui.horizontal(|ui| {
                            if ui
                                .add_enabled(actions.pair, egui::Button::new("Pair"))
                                .clicked()
                            {
                                if let Some(Target::Receiver(r)) = selected.clone() {
                                    self.start_pair(r);
                                }
                            }
                            let cast = ui.add_enabled(actions.cast, egui::Button::new("Cast"));
                            let cast = match selected.as_ref() {
                                Some(t) => cast.on_hover_text(format!(
                                    "Send this screen to {}",
                                    t.name()
                                )),
                                None => cast,
                            };
                            if cast.clicked() {
                                match selected.clone() {
                                    Some(Target::Receiver(r)) => self.cast_to_receiver(r),
                                    Some(Target::Display(d)) => self.cast_to_display(d),
                                    None => {}
                                }
                            }
                        });
                    }
                    Some(a) => {
                        ui.label(a.line());
                        if ui.button("Stop").clicked() {
                            a.stop();
                        }
                    }
                }
            }

            if !message.is_empty() {
                ui.separator();
                ui.label(message);
            }
        });
    }

    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        // If a pairing is pending, a worker is blocked in `recv()`. Dropping
        // the sender closes the channel so it finishes instead of blocking for
        // ever, which would hang `rt.shutdown_timeout` below.
        if let Some(tx) = self.shared.lock().unwrap().pin_tx.take() {
            drop(tx);
        }
        // A Miracast cast is waited for rather than abandoned: teardown has to
        // reach the display and the Wi-Fi Direct group has to be released
        // before this process goes.
        if let Some(mut a) = self.active.take() {
            a.shutdown();
        }
    }
}

pub fn run_gui(config_dir: PathBuf, sender_name: String) -> anyhow::Result<()> {
    let rt = tokio::runtime::Runtime::new()?;

    #[cfg(windows)]
    let found = castr_capture_win::outputs().unwrap_or_default();
    #[cfg(windows)]
    let monitors: Vec<(u32, String)> = found
        .iter()
        .map(|o| (o.index, castr_capture_win::outputs::label(o)))
        .collect();
    #[cfg(windows)]
    let monitor = castr_capture_win::outputs::default_index(&found);
    #[cfg(not(windows))]
    let (monitors, monitor) = (Vec::new(), 0);

    let app = App {
        rt: rt.handle().clone(),
        shared: Arc::new(Mutex::new(Shared {
            receivers: Vec::new(),
            displays: Vec::new(),
            scanning_receivers: false,
            scanning_displays: false,
            scanned: false,
            message: String::new(),
            pin_tx: None,
            pin_target: None,
            pin_kind: PinKind::Castr,
        })),
        config_dir,
        sender_name,
        selected: None,
        mode: Mode::Game,
        pin_input: String::new(),
        active: None,
        monitors,
        monitor,
        wifi: wifi::Panel::default(),
    };
    app.scan();
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default().with_inner_size([480.0, 420.0]),
        ..Default::default()
    };
    let result = eframe::run_native("castr", options, Box::new(|_| Ok(Box::new(app))))
        .map_err(|e| anyhow::anyhow!("gui: {e}"));
    // `App::on_exit` releases any worker blocked on a pending PIN, but give
    // shutdown a bound anyway so the process exits even if one is stuck.
    rt.shutdown_timeout(std::time::Duration::from_secs(1));
    result
}
