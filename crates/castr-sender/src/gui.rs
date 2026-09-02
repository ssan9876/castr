use crate::cast::*;
use castr_net::ReceiverInfo;
use castr_proto::Mode;
use eframe::egui;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::{mpsc, watch};

struct Shared {
    receivers: Vec<ReceiverInfo>,
    scanning: bool,
    message: String,
    pairing_pin_tx: Option<std::sync::mpsc::Sender<String>>,
    pairing_target: Option<String>,
}

struct ActiveCast {
    cmd: mpsc::Sender<CastCommand>,
    status: watch::Receiver<CastStatus>,
}

struct App {
    rt: tokio::runtime::Handle,
    shared: Arc<Mutex<Shared>>,
    config_dir: PathBuf,
    sender_name: String,
    selected: Option<usize>,
    mode: Mode,
    pairing_pin_input: String,
    active: Option<ActiveCast>,
}

impl App {
    fn scan(&self) {
        let shared = self.shared.clone();
        shared.lock().unwrap().scanning = true;
        self.rt.spawn(async move {
            let found = discover(Duration::from_secs(2)).await.unwrap_or_default();
            let mut s = shared.lock().unwrap();
            s.receivers = found;
            s.scanning = false;
        });
    }

    fn start_pair(&mut self, target: ReceiverInfo) {
        let (pin_tx, pin_rx) = std::sync::mpsc::channel::<String>();
        {
            let mut s = self.shared.lock().unwrap();
            s.pairing_pin_tx = Some(pin_tx);
            s.pairing_target = Some(target.name.clone());
        }
        let shared = self.shared.clone();
        let dir = self.config_dir.clone();
        self.rt.spawn(async move {
            let name = target.name.clone();
            // `read_pin` below blocks this runtime worker thread until the
            // user submits a PIN through the GUI (or cancels, which drops
            // `pin_tx` and makes `recv()` return an error). That is
            // acceptable because the runtime is multi-threaded; if the
            // window is closed while this is pending, `App::on_exit` takes
            // and drops `shared.pairing_pin_tx`, closing the channel and
            // releasing this worker thread.
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
            s.pairing_pin_tx = None;
            s.pairing_target = None;
        });
    }

    fn do_cast(&mut self, target: ReceiverInfo) {
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
        self.active = Some(ActiveCast {
            cmd: cmd_tx,
            status: status_rx,
        });
    }
}

impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        ctx.request_repaint_after(Duration::from_millis(250));
        let (receivers, scanning, message, pairing_target, pairing_active) = {
            let s = self.shared.lock().unwrap();
            (
                s.receivers.clone(),
                s.scanning,
                s.message.clone(),
                s.pairing_target.clone(),
                s.pairing_pin_tx.is_some(),
            )
        };
        if let Some(a) = &self.active {
            if a.status.borrow().state == "stopped" || a.status.borrow().state == "failed" {
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
            });
            ui.separator();
            for (i, r) in receivers.iter().enumerate() {
                ui.selectable_value(
                    &mut self.selected,
                    Some(i),
                    format!("{}  ({})", r.name, r.addr.ip()),
                );
            }
            if receivers.is_empty() && !scanning {
                ui.label("No receivers found. Is the receiver running on this network?");
            }
            ui.separator();
            ui.horizontal(|ui| {
                ui.label("Mode:");
                if ui
                    .selectable_value(&mut self.mode, Mode::Game, "Game")
                    .clicked()
                {
                    if let Some(a) = &self.active {
                        let _ = a.cmd.try_send(CastCommand::SetMode(Mode::Game));
                    }
                }
                if ui
                    .selectable_value(&mut self.mode, Mode::Quality, "Quality")
                    .clicked()
                {
                    if let Some(a) = &self.active {
                        let _ = a.cmd.try_send(CastCommand::SetMode(Mode::Quality));
                    }
                }
            });
            let target = self.selected.and_then(|i| receivers.get(i).cloned());
            if pairing_active {
                let name = pairing_target.unwrap_or_default();
                ui.label(format!("Enter the PIN shown on '{name}'"));
                ui.horizontal(|ui| {
                    ui.add(
                        egui::TextEdit::singleline(&mut self.pairing_pin_input)
                            .desired_width(80.0)
                            .char_limit(6),
                    );
                    let ready = self.pairing_pin_input.trim().len() == 6
                        && self
                            .pairing_pin_input
                            .trim()
                            .chars()
                            .all(|c| c.is_ascii_digit());
                    if ui.add_enabled(ready, egui::Button::new("Submit")).clicked() {
                        let pin = self.pairing_pin_input.trim().to_string();
                        let mut s = self.shared.lock().unwrap();
                        if let Some(tx) = &s.pairing_pin_tx {
                            let _ = tx.send(pin);
                        }
                        s.pairing_pin_tx = None;
                        drop(s);
                        self.pairing_pin_input.clear();
                    }
                    if ui.button("Cancel").clicked() {
                        // Dropping the sender closes the channel; the
                        // `pair_interactive` closure's `recv()` then errors
                        // out, which fails the pairing cleanly.
                        let mut s = self.shared.lock().unwrap();
                        s.pairing_pin_tx = None;
                        drop(s);
                        self.pairing_pin_input.clear();
                    }
                });
            } else {
                match &self.active {
                    None => {
                        ui.horizontal(|ui| {
                            if ui
                                .add_enabled(target.is_some(), egui::Button::new("Pair"))
                                .clicked()
                            {
                                self.start_pair(target.clone().unwrap());
                            }
                            if ui
                                .add_enabled(target.is_some(), egui::Button::new("Cast"))
                                .clicked()
                            {
                                self.do_cast(target.clone().unwrap());
                            }
                        });
                    }
                    Some(a) => {
                        let s = a.status.borrow().clone();
                        ui.label(format!(
                            "{}  {}x{}  {:.1} Mbps  rtt {} ms  loss {:.1}%  {:.0} fps",
                            s.state,
                            s.width,
                            s.height,
                            s.bitrate_bps as f64 / 1e6,
                            s.rtt_ms,
                            s.loss_pct,
                            s.fps
                        ));
                        if ui.button("Stop").clicked() {
                            let _ = a.cmd.try_send(CastCommand::Stop);
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
        // If a pairing is pending, the spawned task is blocked in
        // `pin_rx.recv()` on a runtime worker thread. Dropping the sender
        // half here closes the channel, so `recv()` errors out and the
        // task finishes instead of blocking forever (which would otherwise
        // hang `rt.shutdown_timeout` below).
        if let Some(tx) = self.shared.lock().unwrap().pairing_pin_tx.take() {
            drop(tx);
        }
        if let Some(a) = &self.active {
            let _ = a.cmd.try_send(CastCommand::Stop);
        }
    }
}

pub fn run_gui(config_dir: PathBuf, sender_name: String) -> anyhow::Result<()> {
    let rt = tokio::runtime::Runtime::new()?;
    let app = App {
        rt: rt.handle().clone(),
        shared: Arc::new(Mutex::new(Shared {
            receivers: Vec::new(),
            scanning: false,
            message: String::new(),
            pairing_pin_tx: None,
            pairing_target: None,
        })),
        config_dir,
        sender_name,
        selected: None,
        mode: Mode::Game,
        pairing_pin_input: String::new(),
        active: None,
    };
    app.scan();
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default().with_inner_size([420.0, 320.0]),
        ..Default::default()
    };
    let result = eframe::run_native("castr", options, Box::new(|_| Ok(Box::new(app))))
        .map_err(|e| anyhow::anyhow!("gui: {e}"));
    // `App::on_exit` releases any worker thread blocked on a pending
    // pairing's PIN channel, but give shutdown a bound anyway so the
    // process exits even if some task is still stuck.
    rt.shutdown_timeout(std::time::Duration::from_secs(1));
    result
}
