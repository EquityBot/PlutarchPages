//! PlutarchPages' external converters and Read Out Loud in the desktop app: Office and HTML files
//! and web pages convert on their own thread (the window stays responsive and Stop works), the
//! Create ▸ From web page dialog, and reading the current page or the rest of the document aloud.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, TryRecvError};

use pdfcraft_engine::SourceKind;

use crate::{PdfCraftApp, theme};

/// The most pages Read to end reads in one go.
const READ_PAGES: usize = 50;

/// A conversion running on its own thread.
pub(crate) struct Job {
    name: String,
    cancel: Arc<AtomicBool>,
    rx: Receiver<Result<Vec<u8>, String>>,
}

#[derive(Default)]
pub(crate) struct External {
    pub(crate) jobs: Vec<Job>,
    /// The Create ▸ From web page dialog's address.
    pub(crate) url: String,
    #[cfg(not(target_arch = "wasm32"))]
    pub(crate) speech: pdfcraft_engine::Speech,
}

impl Drop for External {
    fn drop(&mut self) {
        for j in &self.jobs {
            j.cancel.store(true, Ordering::Relaxed);
        }
    }
}

fn stem(name: &str) -> &str {
    name.rsplit_once('.').map_or(name, |(s, _)| s)
}

/// The Create ▸ From web page dialog. Returns (create, cancel).
pub(crate) fn web_page_body(ui: &mut egui::Ui, app: &mut PdfCraftApp) -> (bool, bool) {
    let t = theme::Tokens::get(ui.ctx());
    ui.heading(tl!("Create PDF from web page"));
    ui.add_space(6.0);
    let label = ui.label(tl!("Web address (http or https)"));
    let edit = ui.add(egui::TextEdit::singleline(&mut app.external.url).desired_width(f32::INFINITY).char_limit(2048)).labelled_by(label.id);
    ui.add_space(6.0);
    let on = app.session.external_converters().enabled;
    let note = if on {
        tl!(
            "The page opens in a fresh, private window of your installed Chrome or Edge and is printed to PDF. Addresses on this computer or a private network are refused."
        )
    } else {
        tl!("External converters are off. Turn them on in Preferences to use your installed Chrome or Edge.")
    };
    ui.label(egui::RichText::new(note).small().color(t.text_muted));
    ui.add_space(12.0);
    let enter = edit.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
    let mut go = false;
    let mut cancel = false;
    ui.horizontal(|ui| {
        go = crate::widgets::pill_button(ui, tl!("Create"), true).clicked() || enter;
        cancel = crate::widgets::pill_button(ui, tl!("Cancel"), false).clicked();
    });
    (go, cancel)
}

/// Preferences ▸ External converters.
pub(crate) fn preferences(ui: &mut egui::Ui, app: &mut PdfCraftApp, t: &theme::Tokens) {
    ui.label(egui::RichText::new(tl!("External converters")).font(theme::semibold(13.0)));
    egui::Frame::new().fill(t.hover).corner_radius(egui::CornerRadius::same(6)).inner_margin(egui::Margin::same(10)).show(ui, |ui| {
        ui.set_width(ui.available_width());
        let mut c = app.session.external_converters().clone();
        if ui.checkbox(&mut c.enabled, tl!("Convert Office files, HTML files and web pages with installed programs")).changed() {
            app.session.set_external_converters(c);
        }
        ui.label(
            egui::RichText::new(tl!("Uses LibreOffice (26.2.5 or later) for Office files and Chrome or Edge for HTML and web pages, each in a fresh private profile. Local HTML opens with scripts and network off. These programs are not sandboxed by PlutarchPages: convert only files you trust."))
                .small()
                .color(t.text_muted),
        );
    });
}

impl PdfCraftApp {
    /// Convert an Office or HTML file through the external converters on a background thread;
    /// the result opens in a new tab when it's ready.
    pub(crate) fn convert_external(&mut self, kind: SourceKind, name: &str, bytes: Vec<u8>) {
        let conv = self.session.external_converters().clone();
        if !conv.enabled || cfg!(target_arch = "wasm32") {
            // Off (or the web build): the engine explains what to do.
            if let Err(e) = conv.convert(kind, name, &bytes, &AtomicBool::new(false)) {
                self.notify(e.to_string());
            }
            return;
        }
        let n = name.to_string();
        self.spawn_external(name, move |cancel| conv.convert(kind, &n, &bytes, cancel).map_err(|e| e.to_string()));
    }

    /// Create ▸ From web page with the dialog's address.
    pub(crate) fn start_web_page(&mut self) {
        let url = self.external.url.trim().to_string();
        let conv = self.session.external_converters().clone();
        let name = match pdfcraft_engine::external::web_page_name(&url) {
            Ok(n) => n,
            Err(e) => {
                self.notify(e.to_string());
                return;
            }
        };
        if !conv.enabled || cfg!(target_arch = "wasm32") {
            if let Err(e) = conv.web_page(&url, &AtomicBool::new(false)) {
                self.notify(e.to_string());
            }
            return;
        }
        self.spawn_external(&name, move |cancel| conv.web_page(&url, cancel).map_err(|e| e.to_string()));
    }

    fn spawn_external(&mut self, name: &str, work: impl FnOnce(&AtomicBool) -> Result<Vec<u8>, String> + Send + 'static) {
        let (tx, rx) = std::sync::mpsc::channel();
        let cancel = Arc::new(AtomicBool::new(false));
        let flag = cancel.clone();
        let ctx = self.ctx.clone();
        std::thread::spawn(move || {
            // A panic in the converter's caller must not take the app down; report it instead.
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| work(&flag)))
                .unwrap_or_else(|_| Err("the conversion stopped unexpectedly".into()));
            // The receiver may be gone (the app quit): nothing to report to then.
            let _ = tx.send(result);
            if let Some(ctx) = ctx {
                ctx.request_repaint();
            }
        });
        self.external.jobs.push(Job { name: name.to_string(), cancel, rx });
        self.notify_fmt("Converting {name}…", &[("name", name)]);
    }

    /// Open finished conversions (each frame).
    pub(crate) fn poll_external(&mut self) {
        let mut finished = Vec::new();
        self.external.jobs.retain(|j| match j.rx.try_recv() {
            Ok(r) => {
                finished.push((j.name.clone(), r));
                false
            }
            Err(TryRecvError::Empty) => true,
            Err(TryRecvError::Disconnected) => {
                finished.push((j.name.clone(), Err("the conversion stopped unexpectedly".into())));
                false
            }
        });
        for (name, result) in finished {
            let opened = self.open_created_bytes(&format!("{}.pdf", stem(&name)), result.map(Arc::new));
            if let Err(e) = opened {
                self.notify_fmt("Couldn't convert {name}: {e}", &[("name", &name), ("e", &e)]);
            }
        }
    }

    /// View ▸ Read Out Loud: the current page, or (`to_end`) the rest of the document, at most
    /// [`READ_PAGES`] pages.
    pub(crate) fn read_aloud(&mut self, to_end: bool) {
        let Some((view, id)) = self.active_ids() else {
            self.notify_tr("Open a document to read it aloud");
            return;
        };
        let (current, pages) = match (self.views.get(view), self.session.get(id)) {
            (Some(v), Some(d)) => (v.current, d.info.pages.len()),
            _ => return,
        };
        let last = if to_end { pages.min(current.saturating_add(READ_PAGES)) } else { current.saturating_add(1).min(pages) };
        let mut text = String::new();
        for p in current..last {
            match self.session.speech_text(id, p) {
                Ok(t) => {
                    text.push_str(&t);
                    text.push('\n');
                }
                Err(e) => {
                    self.notify(e.to_string());
                    return;
                }
            }
        }
        self.speak(&text);
    }

    #[cfg(not(target_arch = "wasm32"))]
    fn speak(&mut self, text: &str) {
        let Some(backend) = pdfcraft_engine::SpeechBackend::detect() else {
            self.notify_tr("No speech program was found (Windows speech, macOS say, or eSpeak NG on Linux)");
            return;
        };
        match self.external.speech.start(&backend, text) {
            Ok(()) => self.notify_tr("Reading aloud. View ▸ Stop reading stops it."),
            Err(e) => self.notify(e.to_string()),
        }
    }

    #[cfg(target_arch = "wasm32")]
    fn speak(&mut self, _text: &str) {
        self.notify_tr("Read Out Loud needs the desktop app");
    }

    /// View ▸ Stop reading.
    pub(crate) fn stop_reading(&mut self) {
        #[cfg(not(target_arch = "wasm32"))]
        self.external.speech.stop();
    }

    /// Whether Read Out Loud is speaking now.
    pub fn reading_aloud(&mut self) -> bool {
        #[cfg(not(target_arch = "wasm32"))]
        return self.external.speech.speaking();
        #[cfg(target_arch = "wasm32")]
        false
    }

    /// Conversions still running.
    pub fn conversions_running(&self) -> usize {
        self.external.jobs.len()
    }
}
