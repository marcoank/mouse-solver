#![windows_subsystem = "windows"]

mod report;

use eframe::egui::{self, Color32, RichText};
use mousecfg_core::{
    apply_state, backup,
    driver::{from_pct, pct},
    t, tf, vendor, Capabilities, Db, DeviceInfo, DpiStages, Effect, MouseDriver, MouseState, Rgb, SupportStatus,
};
use mousecfg_drivers::{hidapi::HidApi, measure, open, scan, Found};
use report::{Answers, Attempt};
use std::sync::mpsc::{channel, Receiver};
use std::time::Duration;

const GREEN: Color32 = Color32::from_rgb(74, 190, 110);
const RED: Color32 = Color32::from_rgb(235, 90, 80);
const ORANGE: Color32 = Color32::from_rgb(235, 160, 60);
const BLUE: Color32 = Color32::from_rgb(90, 150, 235);
/// Ambra del sito (Tailwind amber-400).
const ACCENT: Color32 = Color32::from_rgb(251, 191, 36);
const ON_ACCENT: Color32 = Color32::from_rgb(23, 23, 23);
const BG: Color32 = Color32::from_rgb(18, 18, 20);
const CARD: Color32 = Color32::from_rgb(29, 29, 32);
const LINE: Color32 = Color32::from_rgb(44, 44, 48);
const TEXT: Color32 = Color32::from_rgb(232, 232, 232);
const WEAK: Color32 = Color32::from_rgb(150, 150, 155);

/// Pagina dove allegare i report (modulo "test-report" in .github/ISSUE_TEMPLATE).
const ISSUES_URL: &str = "https://github.com/marcoank/mouse-solver/issues/new?template=test-report.yml";

const WIDTH: f32 = 460.0;

fn main() -> eframe::Result {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default().with_inner_size([WIDTH, 560.0]).with_min_inner_size([400.0, 360.0]),
        ..Default::default()
    };
    let title = format!("Mouse Solver {}", env!("CARGO_PKG_VERSION"));
    eframe::run_native(
        &title,
        options,
        Box::new(|cc| {
            setup_style(&cc.egui_ctx);
            Ok(Box::new(App::default()))
        }),
    )
}

fn bold() -> egui::FontFamily {
    egui::FontFamily::Name("bold".into())
}

/// Segoe UI dal sistema (se c'è), tema scuro con l'ambra del sito come colore principale.
fn setup_style(ctx: &egui::Context) {
    use egui::{FontData, FontFamily, FontId, TextStyle};
    let mut fonts = egui::FontDefinitions::default();
    let mut bold_family = fonts.families[&FontFamily::Proportional].clone();
    let dir = std::env::var("WINDIR").unwrap_or_else(|_| r"C:\Windows".into());
    for (key, file) in [("segoe", "segoeui.ttf"), ("segoe_sb", "seguisb.ttf")] {
        let Ok(bytes) = std::fs::read(format!(r"{dir}\Fonts\{file}")) else { continue };
        fonts.font_data.insert(key.into(), std::sync::Arc::new(FontData::from_owned(bytes)));
        if key == "segoe" {
            fonts.families.get_mut(&FontFamily::Proportional).unwrap().insert(0, key.into());
        } else {
            bold_family.insert(0, key.into());
        }
    }
    fonts.families.insert(bold(), bold_family);
    ctx.set_fonts(fonts);

    ctx.all_styles_mut(|s| {
        s.text_styles = [
            (TextStyle::Small, FontId::proportional(12.0)),
            (TextStyle::Body, FontId::proportional(14.0)),
            (TextStyle::Button, FontId::proportional(14.0)),
            (TextStyle::Heading, FontId::new(16.0, bold())),
            (TextStyle::Monospace, FontId::monospace(13.0)),
        ]
        .into();
        s.spacing.item_spacing = egui::vec2(8.0, 7.0);
        s.spacing.button_padding = egui::vec2(12.0, 5.0);
        s.spacing.interact_size.y = 28.0;

        let v = &mut s.visuals;
        *v = egui::Visuals::dark();
        v.override_text_color = Some(TEXT);
        v.panel_fill = BG;
        v.window_fill = CARD;
        v.extreme_bg_color = Color32::from_rgb(14, 14, 16);
        v.faint_bg_color = CARD;
        v.window_stroke = egui::Stroke::new(1.0, LINE);
        v.window_corner_radius = 8.into();
        v.menu_corner_radius = 8.into();
        v.hyperlink_color = ACCENT;
        v.selection.bg_fill = ACCENT;
        v.selection.stroke = egui::Stroke::new(1.0, ON_ACCENT);
        v.slider_trailing_fill = true;
        let w = &mut v.widgets;
        for (st, fill) in [
            (&mut w.inactive, Color32::from_rgb(40, 40, 44)),
            (&mut w.hovered, Color32::from_rgb(54, 54, 58)),
            (&mut w.active, Color32::from_rgb(64, 64, 68)),
            (&mut w.open, Color32::from_rgb(40, 40, 44)),
        ] {
            st.bg_fill = fill;
            st.weak_bg_fill = fill;
            st.corner_radius = 6.into();
            st.bg_stroke = egui::Stroke::new(1.0, LINE);
            st.fg_stroke.color = TEXT;
        }
        w.hovered.bg_stroke = egui::Stroke::new(1.0, Color32::from_rgb(80, 80, 86));
        w.noninteractive.bg_stroke = egui::Stroke::new(1.0, LINE);
        w.noninteractive.corner_radius = 6.into();
        w.noninteractive.fg_stroke.color = TEXT;
    });
}

/// Riquadro di una sezione, con titolo.
fn card<R>(ui: &mut egui::Ui, title: &str, add: impl FnOnce(&mut egui::Ui) -> R) -> R {
    egui::Frame::new()
        .fill(CARD)
        .stroke(egui::Stroke::new(1.0, LINE))
        .corner_radius(10)
        .inner_margin(egui::Margin { left: 14, right: 14, top: 10, bottom: 12 })
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.set_max_width(ui.available_width());
            if !title.is_empty() {
                ui.label(RichText::new(title).heading());
                ui.add_space(2.0);
            }
            add(ui)
        })
        .inner
}

/// Etichetta colorata arrotondata (stato del supporto).
fn pill(ui: &mut egui::Ui, text: &str, color: Color32) {
    egui::Frame::new()
        .fill(color.gamma_multiply(0.18))
        .stroke(egui::Stroke::new(1.0, color.gamma_multiply(0.6)))
        .corner_radius(20)
        .inner_margin(egui::Margin::symmetric(9, 2))
        .show(ui, |ui| ui.label(RichText::new(text).small().color(color)));
}

fn primary_button(text: &str) -> egui::Button<'_> {
    egui::Button::new(RichText::new(text).color(ON_ACCENT).family(bold())).fill(ACCENT).stroke(egui::Stroke::NONE)
}

type MeasureResult = anyhow::Result<Vec<measure::Measurement>>;

#[derive(Default)]
struct App {
    api: Option<HidApi>,
    db: Db,
    found: Vec<Found>,
    sel: usize,
    drv: Option<Box<dyn MouseDriver>>,
    info: Option<DeviceInfo>,
    current: Option<MouseState>,
    edit: MouseState,
    msg: Option<(String, bool)>,
    vendors: Vec<&'static str>,
    frames: u32,
    measuring: Option<Receiver<MeasureResult>>,
    measured: Option<String>,
    attempts: Vec<Attempt>,
    answers: Answers,
    report_path: Option<String>,
    /// Altezza della finestra da adattare al contenuto (dopo l'apertura di un mouse).
    fit: bool,
}

impl App {
    fn ok(&mut self, s: impl Into<String>) {
        self.msg = Some((s.into(), false));
    }
    fn err(&mut self, e: impl std::fmt::Display) {
        self.msg = Some((format!("{e:#}"), true));
    }

    /// Rileva i mouse e apre quello selezionato.
    fn rescan(&mut self) {
        self.vendors = vendor::running();
        if self.api.is_none() {
            match Db::load().and_then(|db| Ok((db, HidApi::new()?))) {
                Ok((db, api)) => {
                    self.db = db;
                    self.api = Some(api);
                }
                Err(e) => return self.err(e),
            }
        } else if let Some(api) = self.api.as_mut() {
            let _ = api.refresh_devices();
        }
        self.found = scan(self.api.as_ref().unwrap(), &self.db);
        if self.sel >= self.found.len() || self.found[self.sel].spec.is_none() {
            self.sel = self.found.iter().position(|f| f.spec.is_some()).unwrap_or(0);
        }
        self.open_selected();
    }

    fn open_selected(&mut self) {
        self.fit = true;
        self.drv = None;
        self.info = None;
        self.current = None;
        self.measured = None;
        let Some(found) = self.found.get(self.sel).filter(|f| f.spec.is_some()) else { return };
        match open(self.api.as_ref().unwrap(), found) {
            Ok(d) => {
                self.drv = Some(d);
                self.reload();
            }
            Err(e) => self.err(e),
        }
    }

    fn reload(&mut self) {
        let Some(drv) = self.drv.as_mut() else { return };
        match drv.identify().and_then(|i| Ok((i, drv.read_state()?))) {
            Ok((info, state)) => {
                self.info = Some(info);
                self.edit = state.clone();
                // Mouse di sola scrittura mai configurati da qui: parto da valori sensati.
                if state.cached && state.dpi_stages.is_none() {
                    let caps = drv.capabilities();
                    let stages = (0..caps.min_stages.max(1)).map(|i| (800u16 << i).min(caps.dpi_max)).collect();
                    self.edit.dpi_stages = Some(DpiStages { stages, active: 0 });
                }
                self.current = Some(state);
                self.msg = None;
            }
            Err(e) => self.err(e),
        }
    }

    fn apply(&mut self, target: MouseState) {
        let (Some(drv), Some(cur), Some(info)) = (self.drv.as_mut(), self.current.clone(), self.info.clone()) else { return };
        if !cur.cached {
            if let Err(e) = backup::save(&info, &cur) {
                return self.err(tf!("backup non riuscito, non scrivo nulla: {e}", "backup failed, nothing was written: {e}"));
            }
        }
        let res = apply_state(drv.as_mut(), &cur, &target);
        // riapro il mouse: alcuni (Corsair) si ricollegano dopo il cambio di polling
        self.rescan();
        let how = if cur.cached {
            t!("Inviato al mouse (non si può rileggere)", "Sent to the mouse (it cannot be read back)")
        } else {
            t!("Salvato nel mouse e verificato", "Saved to the mouse and verified")
        };
        match res {
            Ok(done) if done.is_empty() => self.ok(t!("Niente da cambiare.", "Nothing to change.")),
            Ok(done) => {
                self.attempts.push(Attempt { changes: done.join("; "), ok: true, message: how.into() });
                self.ok(format!("{how}:\n{}", done.join("\n")));
            }
            Err(e) => {
                self.attempts.push(Attempt { changes: t!("modifica", "change").into(), ok: false, message: format!("{e:#}") });
                self.err(e);
            }
        }
    }

    fn start_measure(&mut self) {
        let (tx, rx) = channel();
        std::thread::spawn(move || {
            let _ = tx.send(measure::measure(Duration::from_secs(3), true));
        });
        self.measuring = Some(rx);
        self.measured = Some(t!("Muovi il mouse in cerchio, veloce, per 3 secondi...", "Move the mouse in circles, fast, for 3 seconds...").into());
    }

    fn poll_measure(&mut self) {
        let Some(rx) = &self.measuring else { return };
        let Ok(res) = rx.try_recv() else { return };
        self.measuring = None;
        let f = &self.found[self.sel];
        self.measured = Some(match res {
            Err(e) => tf!("Misura non riuscita: {e}", "Measurement failed: {e}"),
            Ok(list) => {
                let mine = list.iter().find(|m| m.vid == Some(f.vid) && m.pid == Some(f.pid)).or(list.first());
                match mine {
                    Some(m) => tf!("Misurato: {} Hz (circa {:.0} Hz)", "Measured: {} Hz (about {:.0} Hz)", m.nearest_hz, m.measured_hz),
                    None => t!("Nessun movimento rilevato: riprova muovendo il mouse di continuo.", "No movement detected: try again and keep moving the mouse.").into(),
                }
            }
        });
    }

    fn make_report(&mut self) {
        let (Some(api), Some(found)) = (self.api.as_ref(), self.found.get(self.sel)) else { return };
        let r = report::build(api, found, self.info.as_ref(), self.current.as_ref(), &self.attempts, &self.answers);
        match report::save(&r, found.vid, found.pid) {
            Ok(p) => self.report_path = Some(p.display().to_string()),
            Err(e) => self.err(tf!("report non salvato: {e}", "report not saved: {e}")),
        }
    }
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        // Il primo frame mostra la finestra subito; la lettura del mouse avviene al secondo.
        self.frames += 1;
        if self.frames == 2 {
            self.rescan();
        } else if self.frames == 1 {
            ui.ctx().request_repaint();
        }
        if self.measuring.is_some() {
            self.poll_measure();
            ui.ctx().request_repaint_after(Duration::from_millis(100));
        }

        let top = egui::Panel::top("device")
            .frame(egui::Frame::new().fill(BG).inner_margin(egui::Margin { left: 14, right: 14, top: 12, bottom: 4 }))
            .show_separator_line(false)
            .show(ui, |ui| self.header(ui))
            .response
            .rect
            .height();

        let bottom = egui::Panel::bottom("actions")
            .frame(egui::Frame::new().fill(CARD).inner_margin(egui::Margin::symmetric(14, 10)).stroke(egui::Stroke::new(1.0, LINE)))
            .show_separator_line(false)
            .show(ui, |ui| self.actions(ui))
            .response
            .rect
            .height();

        let content = egui::CentralPanel::default()
            .frame(egui::Frame::new().fill(BG).inner_margin(egui::Margin { left: 14, right: 14, top: 6, bottom: 12 }))
            .show(ui, |ui| {
                let w = ui.available_width();
                egui::ScrollArea::vertical()
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        ui.set_max_width(w);
                        self.body(ui);
                        self.feedback_ui(ui);
                    })
                    .content_size
                    .y
            })
            .inner;

        // Finestra alta quanto serve: niente spazio vuoto sotto, niente scorrimento se c'è posto.
        if self.fit && self.frames > 3 {
            self.fit = false;
            let h = (top + content + bottom + 20.0).clamp(360.0, 860.0);
            let w = ui.ctx().content_rect().width().max(WIDTH);
            ui.ctx().send_viewport_cmd(egui::ViewportCommand::InnerSize(egui::vec2(w, h)));
        } else if self.fit {
            ui.ctx().request_repaint();
        }
    }
}

impl App {
    /// Scelta del mouse e stato del supporto.
    fn header(&mut self, ui: &mut egui::Ui) {
        ui.label(RichText::new("Mouse Solver").family(bold()).size(18.0).color(ACCENT));
        if self.frames < 2 || self.found.is_empty() {
            return;
        }
        let names: Vec<String> = self.found.iter().map(|f| f.display_name()).collect();
        let before = self.sel;
        if names.len() == 1 {
            ui.label(RichText::new(&names[0]).family(bold()).size(15.0));
        } else {
            egui::ComboBox::from_id_salt("dev").width(ui.available_width()).selected_text(RichText::new(&names[self.sel]).family(bold())).show_ui(ui, |ui| {
                for (i, n) in names.iter().enumerate() {
                    ui.selectable_value(&mut self.sel, i, n);
                }
            });
        }
        if self.sel != before {
            self.open_selected();
        }
        let f = &self.found[self.sel];
        ui.horizontal(|ui| {
            let (txt, col) = match f.spec.as_ref().map(|s| s.status) {
                Some(SupportStatus::Verified) => (t!("verificato", "verified"), GREEN),
                Some(SupportStatus::Community) => (t!("provato da altri", "community tested"), BLUE),
                Some(SupportStatus::Experimental) => (t!("sperimentale", "experimental"), ORANGE),
                None => (t!("non ancora supportato", "not supported yet"), WEAK),
            };
            pill(ui, txt, col);
            if let Some(fw) = self.info.as_ref().and_then(|i| i.firmware.clone()) {
                ui.label(RichText::new(format!("firmware {fw}")).small().color(WEAK));
            }
            ui.label(RichText::new(format!("{:04x}:{:04x}", f.vid, f.pid)).small().color(WEAK));
        });
    }

    /// Barra in basso: esito dell'ultima operazione e pulsanti.
    fn actions(&mut self, ui: &mut egui::Ui) {
        if let Some((m, is_err)) = &self.msg {
            let (icon, col) = if *is_err { ("⚠", RED) } else { ("✔", GREEN) };
            ui.label(RichText::new(format!("{icon} {m}")).color(col));
        }
        ui.horizontal(|ui| {
            let last = self.info.as_ref().and_then(|i| backup::list(i.vid, i.pid).into_iter().nth(0));
            if ui
                .add_enabled(last.is_some(), egui::Button::new(t!("Ripristina backup", "Restore backup")))
                .on_hover_text(t!("Rimette le impostazioni salvate prima dell'ultima modifica", "Puts back the settings saved before the last change"))
                .clicked()
            {
                match backup::load(last.as_ref().unwrap()) {
                    Ok(b) => self.apply(b.state),
                    Err(e) => self.err(e),
                }
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let dirty = self.current.as_ref().is_some_and(|c| *c != self.edit);
                let apply = if dirty { primary_button(t!("Applica", "Apply")) } else { egui::Button::new(t!("Applica", "Apply")) };
                if ui.add_enabled(dirty, apply.min_size(egui::vec2(96.0, 30.0))).clicked() {
                    self.apply(self.edit.clone());
                }
                if ui.button(t!("Rileggi", "Reload")).on_hover_text(t!("Cerca di nuovo i mouse e rilegge le impostazioni", "Looks for mice again and reads the settings")).clicked() {
                    self.rescan();
                }
                if dirty && ui.button(t!("Annulla", "Undo")).clicked() {
                    if let Some(c) = self.current.clone() {
                        self.edit = c;
                    }
                }
            });
        });
    }

    fn body(&mut self, ui: &mut egui::Ui) {
        if self.frames < 2 {
            ui.label(RichText::new(t!("Lettura del mouse...", "Reading the mouse...")).color(WEAK));
            return;
        }
        if self.found.is_empty() {
            card(ui, t!("Nessun mouse trovato", "No mouse found"), |ui| {
                ui.label(RichText::new(t!("Collegalo col cavo e premi Rileggi.", "Connect it with the cable and press Reload.")).color(WEAK));
            });
            return;
        }

        let f = &self.found[self.sel];
        let Some(spec) = f.spec.clone() else {
            let (vid, pid) = (f.vid, f.pid);
            card(ui, t!("Modello non ancora supportato", "Model not supported yet"), |ui| {
                ui.label(tf!("{:04x}:{:04x} non è ancora nel nostro elenco.", "{:04x}:{:04x} is not in our list yet.", vid, pid));
                ui.label(RichText::new(t!("Puoi aiutarci: crea un report qui sotto oppure usa mouse-solver-kit.exe.", "You can help: create a report below or use mouse-solver-kit.exe.")).color(WEAK));
            });
            self.polling_ui(ui, &[]);
            return;
        };

        if !self.vendors.is_empty() {
            notice(
                ui,
                ORANGE,
                &tf!(
                    "È aperto {}: può sovrascrivere le impostazioni. Meglio chiuderlo.",
                    "{} is running: it may overwrite the settings. Better close it.",
                    self.vendors.join(", ")
                ),
            );
        }
        if let Some(n) = spec.notes_text() {
            notice(ui, WEAK, n.trim());
        }
        let Some(current) = self.current.clone() else { return };
        // Alcuni driver (Logitech) leggono le capacità dal mouse: valgono quelle.
        let caps = self.drv.as_ref().map(|d| d.capabilities().clone()).unwrap_or(spec.caps.clone());

        // Mouse senza driver: solo istruzioni e misura del polling.
        if spec.driver == "zowie" {
            self.polling_ui(ui, &[]);
            return;
        }
        if current.cached {
            notice(
                ui,
                ORANGE,
                t!(
                    "Questo mouse non permette di rileggere: vedi i valori scritti l'ultima volta da qui.",
                    "This mouse cannot be read back: you see the values last written from here."
                ),
            );
        }

        card(ui, "DPI", |ui| match self.edit.dpi_stages.as_mut() {
            Some(st) => dpi_ui(ui, st, &caps),
            None => {
                ui.label(RichText::new(t!("Non leggibili su questo mouse.", "Not readable on this mouse.")).color(WEAK));
            }
        });

        self.polling_ui(ui, &caps.polling);

        if !self.edit.zones.is_empty() {
            ui.add_space(4.0);
            card(ui, t!("Luci", "Lighting"), |ui| {
                // Nome, effetto, colore, luminosità (cursore + valore): il cursore prende il resto.
                let slider_w = (ui.available_width() - 72.0 - 116.0 - 40.0 - 52.0 - 4.0 * 10.0).max(50.0);
                egui::Grid::new("zones").num_columns(4).spacing([10.0, 10.0]).min_col_width(40.0).show(ui, |ui| {
                    for z in &mut self.edit.zones {
                        zone_ui(ui, z, &caps, slider_w);
                        ui.end_row();
                    }
                });
            });
        }
    }

    fn polling_ui(&mut self, ui: &mut egui::Ui, choices: &[u16]) {
        ui.add_space(4.0);
        card(ui, "Polling rate", |ui| {
            if !choices.is_empty() {
                ui.horizontal(|ui| {
                    let gap = ui.spacing().item_spacing.x;
                    let n = choices.len() as f32;
                    let w = ((ui.available_width() - gap * (n - 1.0)) / n).max(60.0);
                    for &hz in choices {
                        let on = self.edit.polling_hz == Some(hz);
                        let text = if hz >= 1000 && hz % 1000 == 0 { format!("{} kHz", hz / 1000) } else { format!("{hz} Hz") };
                        if ui.add(egui::Button::new(text).selected(on).min_size(egui::vec2(w, 30.0))).clicked() {
                            self.edit.polling_hz = Some(hz);
                        }
                    }
                });
            }
            ui.horizontal(|ui| {
                let b = egui::Button::new(t!("Misura", "Measure"));
                if ui
                    .add_enabled(self.measuring.is_none(), b)
                    .on_hover_text(t!("Conta i pacchetti reali del mouse mentre lo muovi", "Counts the real packets of the mouse while you move it"))
                    .clicked()
                {
                    self.start_measure();
                }
                let m = self.measured.as_deref().unwrap_or(t!("Controlla il polling reale del mouse.", "Check the real polling of the mouse."));
                ui.label(RichText::new(m).color(WEAK));
            });
        });
    }

    /// "Com'è andata?": risposte della persona e salvataggio del report.
    fn feedback_ui(&mut self, ui: &mut egui::Ui) {
        if self.frames < 2 || self.found.is_empty() {
            return;
        }
        ui.add_space(4.0);
        card(ui, "", |ui| {
            egui::CollapsingHeader::new(RichText::new(t!("Com'è andata? Aiutaci con un report", "How did it go? Help us with a report")).family(bold()))
                .default_open(false)
                .show(ui, |ui| {
                    ui.label(
                        RichText::new(t!(
                            "Il report contiene modello, impostazioni lette, esito delle prove e i dati scambiati col mouse. \
                             Niente numeri di serie né dati personali. L'app non lo invia: lo salva sul Desktop.",
                            "The report holds the model, the settings read, the test results and the data exchanged with \
                             the mouse. No serial numbers or personal data. The app does not send it: it saves it on the Desktop."
                        ))
                        .small()
                        .color(WEAK),
                    );
                    ui.add_space(4.0);
                    let a = &mut self.answers;
                    egui::Grid::new("answers").num_columns(4).spacing([10.0, 6.0]).show(ui, |ui| {
                        for (label, v) in [
                            (t!("DPI", "DPI"), &mut a.dpi),
                            (t!("Stadio attivo", "Active stage"), &mut a.active_stage),
                            ("Polling", &mut a.polling),
                            (t!("Luci", "Lighting"), &mut a.lighting),
                            (t!("Restano dopo aver ricollegato", "Kept after replugging"), &mut a.persists),
                        ] {
                            ui.label(label);
                            ui.radio_value(v, Some(true), t!("sì", "yes"));
                            ui.radio_value(v, Some(false), t!("no", "no"));
                            ui.radio_value(v, None, t!("non provato", "not tried"));
                            ui.end_row();
                        }
                    });
                    ui.label(RichText::new(t!("Note (modello esatto, cosa è successo):", "Notes (exact model, what happened):")).color(WEAK));
                    ui.add(egui::TextEdit::multiline(&mut a.notes).desired_rows(3).desired_width(f32::INFINITY));
                    ui.horizontal(|ui| {
                        if ui.add(primary_button(t!("Crea report", "Create report"))).clicked() {
                            self.make_report();
                        }
                        if ui.button(t!("Apri la pagina delle segnalazioni", "Open the reports page")).clicked() {
                            let _ = std::process::Command::new("explorer").arg(ISSUES_URL).spawn();
                        }
                    });
                    if let Some(p) = &self.report_path {
                        ui.label(RichText::new(tf!("Salvato: {p}\nAllegalo alla segnalazione.", "Saved: {p}\nAttach it to the report.")).color(GREEN));
                    }
                });
        });
    }
}

/// Avviso su una riga, con una barretta colorata a sinistra.
fn notice(ui: &mut egui::Ui, color: Color32, text: &str) {
    egui::Frame::new()
        .fill(color.gamma_multiply(0.08))
        .corner_radius(8)
        .inner_margin(egui::Margin::symmetric(12, 8))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.label(RichText::new(text).color(if color == WEAK { WEAK } else { color }));
        });
    ui.add_space(4.0);
}

fn effect_label(name: &str) -> &'static str {
    match name {
        "static" => t!("fisso", "static"),
        "breathing" => t!("respiro", "breathing"),
        "spectrum" => t!("ciclo colori", "color cycle"),
        "wave" => t!("onda", "wave"),
        "reactive" => t!("reattivo", "reactive"),
        "off" => t!("spento", "off"),
        _ => "?",
    }
}

fn zone_label(zone: &str) -> String {
    let z = match zone {
        "logo" => t!("Logo", "Logo"),
        "scroll" | "wheel" => t!("Rotella", "Wheel"),
        "underglow" => t!("Base", "Underglow"),
        "all" => t!("Tutto", "All"),
        other => return other.chars().next().map(|c| c.to_uppercase().chain(other.chars().skip(1)).collect()).unwrap_or_default(),
    };
    z.to_string()
}

fn zone_ui(ui: &mut egui::Ui, z: &mut mousecfg_core::ZoneState, caps: &Capabilities, slider_w: f32) {
    let size = egui::vec2(72.0, 28.0);
    ui.allocate_ui_with_layout(size, egui::Layout::left_to_right(egui::Align::Center), |ui| {
        ui.set_min_size(size);
        ui.add(egui::Label::new(zone_label(&z.zone)).truncate());
    });
    let color = z.effect.as_ref().and_then(|e| e.color()).unwrap_or(Rgb(255, 255, 255));
    let mut name = z.effect.as_ref().map(|e| e.name().to_string()).unwrap_or_else(|| "?".into());
    let before = name.clone();
    egui::ComboBox::from_id_salt(format!("eff-{}", z.zone)).width(116.0).selected_text(effect_label(&name)).show_ui(ui, |ui| {
        for e in &caps.effects {
            ui.selectable_value(&mut name, e.clone(), effect_label(e));
        }
    });
    if name != before {
        z.effect = Effect::from_name(&name, color);
    }
    if let Some(c) = z.effect.as_ref().and_then(|e| e.color()) {
        let mut rgb = [c.0, c.1, c.2];
        if ui.color_edit_button_srgb(&mut rgb).on_hover_text(t!("Colore", "Color")).changed() {
            z.effect = Effect::from_name(&name, Rgb(rgb[0], rgb[1], rgb[2]));
        }
    } else {
        ui.label("");
    }
    if let Some(b) = z.brightness {
        let mut p = pct(b);
        ui.spacing_mut().slider_width = slider_w;
        if ui.add(egui::Slider::new(&mut p, 0..=100).suffix("%")).on_hover_text(t!("Luminosità", "Brightness")).changed() {
            z.brightness = Some(from_pct(p));
        }
    }
}

fn dpi_ui(ui: &mut egui::Ui, st: &mut DpiStages, caps: &Capabilities) {
    let step = caps.dpi_step.max(1);
    let mut remove = None;
    let can_remove = st.stages.len() > caps.min_stages as usize;
    // Larghezze fisse: calcolate una volta dalla riga, così il riquadro non si allarga da solo.
    let gap = ui.spacing().item_spacing.x;
    let (label_w, value_w, x_w) = (80.0, 88.0, if can_remove { 30.0 } else { 0.0 });
    let slider_w = (ui.available_width() - label_w - value_w - x_w - gap * if can_remove { 3.0 } else { 2.0 }).max(60.0);
    for i in 0..st.stages.len() {
        ui.horizontal(|ui| {
            let label = tf!("Stadio {}", "Stage {}", i + 1);
            let active = st.active == i;
            let text = RichText::new(label).color(if active && caps.active_stage_settable { ACCENT } else { TEXT });
            let size = egui::vec2(label_w, 28.0);
            if caps.active_stage_settable {
                let r = ui.allocate_ui_with_layout(size, egui::Layout::left_to_right(egui::Align::Center), |ui| {
                    ui.set_min_size(size);
                    ui.add(egui::RadioButton::new(active, text))
                });
                if r.inner.on_hover_text(t!("Stadio in uso adesso", "Stage in use now")).clicked() {
                    st.active = i;
                }
            } else {
                ui.allocate_ui_with_layout(size, egui::Layout::left_to_right(egui::Align::Center), |ui| {
                    ui.set_min_size(size);
                    ui.label(text);
                });
            }
            let d = &mut st.stages[i];
            ui.spacing_mut().slider_width = slider_w;
            let mut v = *d as f64;
            if ui
                .add(egui::Slider::new(&mut v, caps.dpi_min as f64..=caps.dpi_max as f64).logarithmic(true).show_value(false))
                .changed()
            {
                *d = v.round() as u16;
            }
            ui.add_sized([value_w, 28.0], egui::DragValue::new(d).range(caps.dpi_min..=caps.dpi_max).speed(step as f64).suffix(" DPI"));
            *d = caps.snap_dpi(*d);
            ui.spacing_mut().button_padding.x = 4.0;
            if can_remove && ui.add(egui::Button::new("×").min_size(egui::vec2(x_w, 28.0))).on_hover_text(t!("Togli stadio", "Remove stage")).clicked() {
                remove = Some(i);
            }
        });
    }
    if let Some(i) = remove {
        st.stages.remove(i);
        st.active = st.active.min(st.stages.len() - 1);
    }
    ui.horizontal(|ui| {
        if st.stages.len() < caps.max_stages as usize && ui.button(t!("+ Aggiungi stadio", "+ Add stage")).clicked() {
            let next = (st.stages.last().copied().unwrap_or(800) * 2).min(caps.dpi_max);
            st.stages.push(next);
        }
        if !caps.active_stage_settable {
            ui.label(RichText::new(t!("Lo stadio attivo si cambia col tasto DPI del mouse.", "The active stage is changed with the mouse's DPI button.")).small().color(WEAK));
        }
    });
}
