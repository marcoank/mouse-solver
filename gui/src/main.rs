#![windows_subsystem = "windows"]

use eframe::egui::{self, Color32, RichText};
use mousecfg_core::{
    apply_state, backup,
    driver::{from_pct, pct},
    vendor, Capabilities, Db, DeviceInfo, DpiStages, Effect, MouseDriver, MouseState, Rgb, SupportStatus,
};
use mousecfg_drivers::{hidapi::HidApi, measure, open, scan, Found};
use std::sync::mpsc::{channel, Receiver};
use std::time::Duration;

const GREEN: Color32 = Color32::from_rgb(60, 170, 90);
const RED: Color32 = Color32::from_rgb(220, 70, 60);
const ORANGE: Color32 = Color32::from_rgb(220, 150, 40);
const BLUE: Color32 = Color32::from_rgb(70, 130, 220);

fn main() -> eframe::Result {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default().with_inner_size([460.0, 680.0]).with_min_inner_size([380.0, 400.0]),
        ..Default::default()
    };
    eframe::run_native("MouseCfg", options, Box::new(|_cc| Ok(Box::new(App::default()))))
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
                return self.err(format!("backup non riuscito, non scrivo nulla: {e}"));
            }
        }
        let res = apply_state(drv.as_mut(), &cur, &target);
        self.reload();
        let how = if cur.cached { "Inviato al mouse (non si può rileggere)" } else { "Salvato nel mouse e verificato" };
        match res {
            Ok(done) if done.is_empty() => self.ok("Niente da cambiare."),
            Ok(done) => self.ok(format!("{how}:\n{}", done.join("\n"))),
            Err(e) => self.err(e),
        }
    }

    fn start_measure(&mut self) {
        let (tx, rx) = channel();
        std::thread::spawn(move || {
            let _ = tx.send(measure::measure(Duration::from_secs(3), true));
        });
        self.measuring = Some(rx);
        self.measured = Some("Muovi il mouse in cerchio, veloce, per 3 secondi...".into());
    }

    fn poll_measure(&mut self) {
        let Some(rx) = &self.measuring else { return };
        let Ok(res) = rx.try_recv() else { return };
        self.measuring = None;
        let f = &self.found[self.sel];
        self.measured = Some(match res {
            Err(e) => format!("Misura non riuscita: {e}"),
            Ok(list) => {
                let mine = list.iter().find(|m| m.vid == Some(f.vid) && m.pid == Some(f.pid)).or(list.first());
                match mine {
                    Some(m) => format!("Misurato: {} Hz (circa {:.0} Hz)", m.nearest_hz, m.measured_hz),
                    None => "Nessun movimento rilevato: riprova muovendo il mouse di continuo.".into(),
                }
            }
        });
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

        egui::Panel::bottom("actions").show(ui, |ui| {
            ui.add_space(6.0);
            if let Some((m, is_err)) = &self.msg {
                ui.label(RichText::new(m).color(if *is_err { RED } else { GREEN }));
                ui.add_space(4.0);
            }
            ui.horizontal(|ui| {
                let dirty = self.current.as_ref().is_some_and(|c| *c != self.edit);
                if ui.add_enabled(dirty, egui::Button::new(RichText::new("Applica").strong())).clicked() {
                    self.apply(self.edit.clone());
                }
                if ui.button("Rileggi").clicked() {
                    self.rescan();
                }
                let last = self.info.as_ref().and_then(|i| backup::list(i.vid, i.pid).into_iter().nth(0));
                if ui.add_enabled(last.is_some(), egui::Button::new("Ripristina ultimo backup")).clicked() {
                    match backup::load(last.as_ref().unwrap()) {
                        Ok(b) => self.apply(b.state),
                        Err(e) => self.err(e),
                    }
                }
            });
            ui.add_space(6.0);
        });

        egui::CentralPanel::default().show(ui, |ui| {
            egui::ScrollArea::vertical().show(ui, |ui| self.body(ui));
        });
    }
}

impl App {
    fn body(&mut self, ui: &mut egui::Ui) {
        if self.frames < 2 {
            ui.label("Lettura del mouse...");
            return;
        }
        if self.found.is_empty() {
            ui.label("Nessun mouse trovato. Collegalo col cavo e premi Rileggi.");
            return;
        }

        let names: Vec<String> = self.found.iter().map(|f| f.display_name()).collect();
        let before = self.sel;
        egui::ComboBox::from_id_salt("dev").width(ui.available_width() - 8.0).selected_text(&names[self.sel]).show_ui(ui, |ui| {
            for (i, n) in names.iter().enumerate() {
                ui.selectable_value(&mut self.sel, i, n);
            }
        });
        if self.sel != before {
            self.open_selected();
        }

        let f = &self.found[self.sel];
        let Some(spec) = f.spec.clone() else {
            ui.add_space(8.0);
            ui.label(format!("{:04x}:{:04x} non è ancora supportato.", f.vid, f.pid));
            ui.label("Usa mouse-cfg-kit.exe per raccogliere le informazioni e mandarcele.");
            self.polling_ui(ui, &[]);
            return;
        };

        ui.horizontal(|ui| {
            let (txt, col) = match spec.status {
                SupportStatus::Verified => ("verificato", GREEN),
                SupportStatus::Community => ("community", BLUE),
                SupportStatus::Experimental => ("sperimentale", ORANGE),
            };
            ui.label(RichText::new(format!(" {txt} ")).color(Color32::WHITE).background_color(col));
            if let Some(fw) = self.info.as_ref().and_then(|i| i.firmware.clone()) {
                ui.label(format!("firmware {fw}"));
            }
        });

        if !self.vendors.is_empty() {
            ui.add_space(4.0);
            ui.label(RichText::new(format!("È aperto {}: può sovrascrivere le impostazioni. Meglio chiuderlo.", self.vendors.join(", "))).color(ORANGE));
        }
        if let Some(n) = &spec.notes {
            ui.add_space(4.0);
            ui.label(RichText::new(n.trim()).weak());
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
            ui.add_space(4.0);
            ui.label(RichText::new("Questo mouse non permette di rileggere: vedi i valori scritti l'ultima volta da qui.").color(ORANGE));
        }

        ui.add_space(10.0);
        ui.heading("DPI");
        if let Some(st) = self.edit.dpi_stages.as_mut() {
            dpi_ui(ui, st, &caps);
        } else {
            ui.label("Non leggibili su questo mouse.");
        }

        self.polling_ui(ui, &caps.polling);

        if !self.edit.zones.is_empty() {
            ui.add_space(10.0);
            ui.heading("Luci");
            egui::Grid::new("zones").num_columns(4).spacing([10.0, 8.0]).show(ui, |ui| {
                for z in &mut self.edit.zones {
                    zone_ui(ui, z, &caps);
                    ui.end_row();
                }
            });
        }
    }

    fn polling_ui(&mut self, ui: &mut egui::Ui, choices: &[u16]) {
        ui.add_space(10.0);
        ui.heading("Polling rate");
        if !choices.is_empty() {
            ui.horizontal(|ui| {
                for &hz in choices {
                    let on = self.edit.polling_hz == Some(hz);
                    if ui.selectable_label(on, format!("{hz} Hz")).clicked() {
                        self.edit.polling_hz = Some(hz);
                    }
                }
            });
        }
        ui.horizontal(|ui| {
            if ui.add_enabled(self.measuring.is_none(), egui::Button::new("Misura polling")).on_hover_text("Conta i pacchetti reali del mouse mentre lo muovi").clicked() {
                self.start_measure();
            }
            if let Some(m) = &self.measured {
                ui.label(m);
            }
        });
    }
}

fn zone_ui(ui: &mut egui::Ui, z: &mut mousecfg_core::ZoneState, caps: &Capabilities) {
    ui.label(&z.zone);
    let color = z.effect.as_ref().and_then(|e| e.color()).unwrap_or(Rgb(255, 255, 255));
    let mut name = z.effect.as_ref().map(|e| e.name().to_string()).unwrap_or_else(|| "?".into());
    let before = name.clone();
    egui::ComboBox::from_id_salt(format!("eff-{}", z.zone)).selected_text(&name).show_ui(ui, |ui| {
        for e in &caps.effects {
            ui.selectable_value(&mut name, e.clone(), e);
        }
    });
    if name != before {
        z.effect = Effect::from_name(&name, color);
    }
    if let Some(c) = z.effect.as_ref().and_then(|e| e.color()) {
        let mut rgb = [c.0, c.1, c.2];
        if ui.color_edit_button_srgb(&mut rgb).changed() {
            z.effect = Effect::from_name(&name, Rgb(rgb[0], rgb[1], rgb[2]));
        }
    } else {
        ui.label("");
    }
    if let Some(b) = z.brightness {
        let mut p = pct(b);
        if ui.add(egui::Slider::new(&mut p, 0..=100).suffix("%")).changed() {
            z.brightness = Some(from_pct(p));
        }
    }
}

fn dpi_ui(ui: &mut egui::Ui, st: &mut DpiStages, caps: &Capabilities) {
    let step = caps.dpi_step.max(1);
    let mut remove = None;
    for i in 0..st.stages.len() {
        ui.horizontal(|ui| {
            if caps.active_stage_settable {
                ui.radio_value(&mut st.active, i, format!("Stadio {}", i + 1));
            } else {
                ui.label(format!("Stadio {}", i + 1));
            }
            let d = &mut st.stages[i];
            ui.add(egui::DragValue::new(d).range(caps.dpi_min..=caps.dpi_max).speed(step as f64).suffix(" DPI"));
            *d = caps.snap_dpi(*d);
            if st.stages.len() > caps.min_stages as usize && ui.small_button("✕").on_hover_text("Togli stadio").clicked() {
                remove = Some(i);
            }
        });
    }
    if !caps.active_stage_settable {
        ui.label(RichText::new("Lo stadio attivo si cambia col tasto DPI del mouse.").weak());
    }
    if let Some(i) = remove {
        st.stages.remove(i);
        st.active = st.active.min(st.stages.len() - 1);
    }
    if st.stages.len() < caps.max_stages as usize && ui.button("+ Aggiungi stadio").clicked() {
        let next = (st.stages.last().copied().unwrap_or(800) * 2).min(caps.dpi_max);
        st.stages.push(next);
    }
}
