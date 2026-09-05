//! The "Check my Wi-Fi" panel.
//!
//! Moved out of the main window unchanged. Its generation counter and drop
//! guard are the reason it is worth keeping together in one file: the button
//! must unlock on every path out, including a panic inside the probe.

use eframe::egui;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

/// Clears `running` when dropped, but only if `generation` still matches the
/// value captured when the worker started. This is the panic-safety net:
/// whatever happens inside the worker closure (a normal return, an early
/// return, or a panic caught by `catch_unwind`), this still runs and the
/// button unlocks. It also makes "Close" safe: "Close" bumps the shared
/// generation counter, so a worker still running when the user closes the
/// panel finds its captured generation stale by the time it finishes and does
/// not resurrect `running`.
struct RunGuard {
    running: Arc<AtomicBool>,
    generation: Arc<AtomicU64>,
    my_generation: u64,
}

impl Drop for RunGuard {
    fn drop(&mut self) {
        if self.generation.load(Ordering::Relaxed) == self.my_generation {
            self.running.store(false, Ordering::Relaxed);
        }
    }
}

pub struct Panel {
    /// `None` until the check has been run; `Some(text)` afterwards.
    report: Arc<Mutex<Option<String>>>,
    running: Arc<AtomicBool>,
    /// Incremented on every click and on every "Close". A worker only writes
    /// its result if this still matches the value it was spawned with, so a
    /// stale result is discarded instead of reappearing in the panel.
    generation: Arc<AtomicU64>,
}

impl Default for Panel {
    fn default() -> Self {
        Self {
            report: Arc::new(Mutex::new(None)),
            running: Arc::new(AtomicBool::new(false)),
            generation: Arc::new(AtomicU64::new(0)),
        }
    }
}

impl Panel {
    /// The button, which belongs in the toolbar row.
    pub fn button(&self, ui: &mut egui::Ui) {
        if ui
            .add_enabled(
                !self.running.load(Ordering::Relaxed),
                egui::Button::new("Check my Wi-Fi"),
            )
            .on_hover_text("Looks for the local causes of Miracast disconnects")
            .clicked()
        {
            self.start();
        }
    }

    fn start(&self) {
        let out = self.report.clone();
        let running = self.running.clone();
        let generation = self.generation.clone();
        let my_generation = generation.fetch_add(1, Ordering::Relaxed) + 1;
        running.store(true, Ordering::Relaxed);
        std::thread::spawn(move || {
            // Constructed first so it runs on every exit path, including a
            // panic caught below: the button must never stay disabled just
            // because the probe code broke.
            let guard = RunGuard {
                running,
                generation: generation.clone(),
                my_generation,
            };
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                #[cfg(windows)]
                let text = {
                    let facts = crate::diagnose::collect::facts();
                    let findings = crate::diagnose::rules::analyse(&facts);
                    crate::diagnose::render::report(&findings, &facts)
                };
                #[cfg(not(windows))]
                let text = "The Wi-Fi health check is Windows only.".to_string();
                text
            }));
            let text = match result {
                Ok(text) => text,
                Err(_) => "The check failed unexpectedly. Please report this.".to_string(),
            };
            if generation.load(Ordering::Relaxed) == my_generation {
                *out.lock().unwrap() = Some(text);
            }
            drop(guard);
        });
    }

    /// The report itself, once there is one.
    pub fn show(&self, ui: &mut egui::Ui) {
        let Some(text) = self.report.lock().unwrap().clone() else {
            return;
        };
        ui.separator();
        ui.horizontal(|ui| {
            ui.heading("Wi-Fi health");
            if ui.button("Copy").clicked() {
                ui.output_mut(|o| o.copied_text = text.clone());
            }
            if ui.button("Close").clicked() {
                *self.report.lock().unwrap() = None;
                // Discard any in-flight worker's result (it will see a stale
                // generation when it finishes) and clear the running flag
                // unconditionally, so a hung or abandoned check cannot leave
                // the button locked.
                self.generation.fetch_add(1, Ordering::Relaxed);
                self.running.store(false, Ordering::Relaxed);
            }
        });
        egui::ScrollArea::vertical()
            .max_height(200.0)
            .show(ui, |ui| {
                ui.monospace(text);
            });
    }
}
