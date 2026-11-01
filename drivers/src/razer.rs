//! Driver Razer. Protocollo reimplementato leggendo OpenRazer (driver/razercommon.h,
//! razerchromacommon.c): nessun codice copiato.
//!
//! Report da 90 byte (+1 di report ID 0x00 su Windows):
//! `[0]` stato, `[1]` transaction id, `[2..3]` pacchetti rimanenti, `[4]` tipo protocollo,
//! `[5]` dimensione dati, `[6]` classe, `[7]` comando, `[8..87]` argomenti, `[88]` CRC (XOR 2..87).

use anyhow::{bail, Context, Result};
use hidapi::{HidApi, HidDevice};
use mousecfg_core::*;
use serde::Deserialize;
use std::collections::BTreeMap;
use std::thread::sleep;
use std::time::Duration;

const LEN: usize = 90;
const VARSTORE: u8 = 0x01;

/// Unici comandi che il driver può inviare. Tutto il resto (firmware, bootloader,
/// modalità dispositivo...) è rifiutato prima di arrivare al mouse.
const ALLOWED: &[(u8, u8)] = &[
    // letture
    (0x00, 0x81), // firmware
    (0x00, 0x85), // polling
    (0x00, 0xC0), // polling HyperPolling
    (0x04, 0x85), // DPI
    (0x04, 0x86), // stadi DPI
    (0x0F, 0x82), // effetto LED (extended)
    (0x0F, 0x84), // luminosità LED (extended)
    (0x03, 0x80), // LED acceso/spento (classic)
    (0x03, 0x81), // colore LED (classic)
    (0x03, 0x82), // effetto LED (classic)
    (0x03, 0x83), // luminosità LED (classic)
    // scritture
    (0x00, 0x05), // polling
    (0x00, 0x40), // polling HyperPolling
    (0x04, 0x05), // DPI
    (0x04, 0x06), // stadi DPI
    (0x0F, 0x02), // effetto LED (extended)
    (0x0F, 0x04), // luminosità LED (extended)
    (0x03, 0x00), // LED acceso/spento (classic)
    (0x03, 0x01), // colore LED (classic)
    (0x03, 0x02), // effetto LED (classic)
    (0x03, 0x03), // luminosità LED (classic)
];

/// Variante del comando polling.
#[derive(Debug, Clone, Copy, PartialEq, Default, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Polling {
    /// 00/05: 125, 500, 1000 Hz
    #[default]
    Standard,
    /// 00/40 (HyperPolling): fino a 8000 Hz
    V2,
    /// come V2, inviato due volte (argomento 0x00 e 0x01) come fa Synapse
    V2Dual,
    None,
}

/// Variante del protocollo luci.
#[derive(Debug, Clone, Copy, PartialEq, Default, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Lighting {
    /// classe 0x0F, mouse recenti
    #[default]
    Extended,
    /// classe 0x03 (LED singoli: acceso, colore, effetto), mouse più vecchi
    Classic,
    None,
}

fn yes() -> bool {
    true
}

#[derive(Debug, Clone, Deserialize)]
pub struct RazerConfig {
    pub transaction_id: u8,
    /// Transaction id diversi per singole funzioni: dpi, dpi_stages, polling, lighting.
    #[serde(default)]
    pub transaction_ids: BTreeMap<String, u8>,
    /// Interfaccia HID da provare per prima (le altre sono il ripiego).
    #[serde(default)]
    pub interface: Option<i32>,
    #[serde(default)]
    pub usage_page: Option<u16>,
    #[serde(default)]
    pub usage: Option<u16>,
    #[serde(default)]
    pub polling: Polling,
    #[serde(default)]
    pub lighting: Lighting,
    #[serde(default = "yes")]
    pub dpi: bool,
    #[serde(default = "yes")]
    pub dpi_stages: bool,
    #[serde(default = "yes")]
    pub brightness: bool,
    #[serde(default)]
    pub leds: BTreeMap<String, u8>,
}

pub struct Razer {
    dev: HidDevice,
    spec: DeviceSpec,
    cfg: RazerConfig,
}

/// Codici polling HyperPolling.
const V2_CODES: [(u8, u16); 7] = [(0x01, 8000), (0x02, 4000), (0x04, 2000), (0x08, 1000), (0x10, 500), (0x20, 250), (0x40, 125)];

pub fn status_text(s: u8) -> &'static str {
    match s {
        0x00 => t!("nuovo", "new"),
        0x01 => t!("occupato", "busy"),
        0x02 => "ok",
        0x03 => t!("errore", "error"),
        0x04 => "timeout",
        0x05 => t!("non supportato", "not supported"),
        _ => "?",
    }
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect::<Vec<_>>().join(" ")
}

fn build(tid: u8, class: u8, id: u8, size: u8, args: &[u8]) -> [u8; LEN + 1] {
    let mut buf = [0u8; LEN + 1]; // buf[0] = report ID
    let r = &mut buf[1..];
    r[1] = tid;
    r[5] = size;
    r[6] = class;
    r[7] = id;
    r[8..8 + args.len()].copy_from_slice(args);
    r[88] = r[2..88].iter().fold(0, |a, b| a ^ b);
    buf
}

/// Invia un comando e restituisce la risposta grezza (90 byte), qualunque sia lo stato.
fn raw_transact(dev: &HidDevice, tid: u8, class: u8, id: u8, size: u8, args: &[u8]) -> Result<[u8; LEN]> {
    if !ALLOWED.contains(&(class, id)) {
        bail!(tf!("comando {class:02x}/{id:02x} bloccato: non è nella lista dei comandi sicuri", "command {class:02x}/{id:02x} blocked: not in the list of safe commands"));
    }
    let req = build(tid, class, id, size, args);
    let debug = mousecfg_core::trace::on();
    if debug {
        mousecfg_core::trace!("> {}", hex(&req[1..16]));
    }
    dev.send_feature_report(&req)?;
    let mut last = [0u8; LEN];
    // Attesa adattiva: di solito bastano 20-60 ms, con Synapse aperto a volte di più.
    for wait in [20, 20, 40, 60, 100, 200] {
        sleep(Duration::from_millis(wait));
        let mut buf = [0u8; LEN + 1];
        let n = dev.get_feature_report(&mut buf)?;
        let resp = if n == LEN + 1 { &buf[1..] } else { &buf[..LEN] };
        last.copy_from_slice(resp);
        if debug {
            mousecfg_core::trace!("< {} (dopo {wait} ms)", hex(&last[..16]));
        }
        // 0x00 = non ancora elaborato, 0x01 = occupato: riproviamo a leggere.
        // Una risposta di un altro comando (es. di Synapse) va scartata.
        let mine = last[6] == class && last[7] == id;
        if last[0] != 0x00 && last[0] != 0x01 && mine {
            break;
        }
    }
    Ok(last)
}

impl Razer {
    pub fn open(api: &HidApi, spec: &DeviceSpec, vid: u16, pid: u16) -> Result<Razer> {
        let cfg: RazerConfig = spec.section("razer")?;
        if !api.device_list().any(|d| d.vendor_id() == vid && d.product_id() == pid) {
            bail!(tf!("{} non trovato. È collegato?", "{} not found. Is it plugged in?", spec.name));
        }
        let tid = cfg.transaction_id;
        let dev = crate::open_first(
            api,
            vid,
            pid,
            |d| {
                cfg.interface.is_none_or(|i| d.interface_number() == i)
                    && cfg.usage_page.is_none_or(|u| d.usage_page() == u)
                    && cfg.usage.is_none_or(|u| d.usage() == u)
            },
            |dev| raw_transact(dev, tid, 0x00, 0x81, 0x02, &[]).is_ok_and(|r| r[0] == 0x02 && r[6] == 0x00 && r[7] == 0x81),
        )
        .with_context(|| {
            tf!(
                "{} trovato ma non risponde. Prova a chiudere Razer Synapse (anche dall'icona vicino all'orologio).",
                "{} found but not responding. Try closing Razer Synapse (also from the icon near the clock).",
                spec.name
            )
        })?;
        let mut spec = spec.clone();
        (spec.vid, spec.pid) = (vid, pid);
        Ok(Razer { dev, spec, cfg })
    }

    fn cmd(&self, class: u8, id: u8, size: u8, args: &[u8]) -> Result<[u8; LEN]> {
        self.cmd_for("", class, id, size, args)
    }

    /// Come `cmd`, con il transaction id specifico della funzione se il TOML lo indica.
    fn cmd_for(&self, feature: &str, class: u8, id: u8, size: u8, args: &[u8]) -> Result<[u8; LEN]> {
        let tid = self.cfg.transaction_ids.get(feature).copied().unwrap_or(self.cfg.transaction_id);
        let r = raw_transact(&self.dev, tid, class, id, size, args)?;
        if r[0] != 0x02 {
            bail!(tf!("comando {class:02x}/{id:02x}: risposta '{}' (0x{:02x})", "command {class:02x}/{id:02x}: reply '{}' (0x{:02x})", status_text(r[0]), r[0]));
        }
        if r[6] != class || r[7] != id {
            bail!(tf!(
                "comando {class:02x}/{id:02x}: risposta per un altro comando ({:02x}/{:02x})",
                "command {class:02x}/{id:02x}: reply to a different command ({:02x}/{:02x})",
                r[6],
                r[7]
            ));
        }
        Ok(r)
    }

    fn led(&self, zone: &str) -> Result<u8> {
        self.cfg.leds.get(zone).copied().with_context(|| tf!("zona '{zone}' senza ID LED nel file TOML", "zone '{zone}' has no LED ID in the TOML file"))
    }

    fn read_dpi(&self) -> Result<u16> {
        let r = self.cmd_for("dpi", 0x04, 0x85, 0x07, &[VARSTORE])?;
        Ok(u16::from_be_bytes([r[9], r[10]]))
    }

    fn read_stages(&self) -> Result<DpiStages> {
        if !self.cfg.dpi_stages {
            return Ok(DpiStages { stages: vec![self.read_dpi()?], active: 0 });
        }
        let r = self.cmd_for("dpi_stages", 0x04, 0x86, 0x26, &[VARSTORE])?;
        let (active_id, count) = (r[9], r[10] as usize);
        let mut stages = Vec::new();
        let mut active = 0;
        for i in 0..count.min(self.spec.caps.max_stages as usize) {
            let o = 11 + i * 7;
            if r[o] == active_id {
                active = i;
            }
            stages.push(u16::from_be_bytes([r[o + 1], r[o + 2]]));
        }
        if stages.is_empty() {
            bail!(t!("il mouse non riporta stadi DPI", "the mouse reports no DPI stages"));
        }
        Ok(DpiStages { stages, active })
    }

    fn read_polling(&self) -> Result<u16> {
        match self.cfg.polling {
            Polling::Standard => {
                let r = self.cmd_for("polling", 0x00, 0x85, 0x01, &[])?;
                Ok(match r[8] {
                    0x01 => 1000,
                    0x02 => 500,
                    0x08 => 125,
                    c => bail!(tf!("codice polling sconosciuto 0x{c:02x}", "unknown polling code 0x{c:02x}")),
                })
            }
            Polling::V2 | Polling::V2Dual => {
                let r = self.cmd_for("polling", 0x00, 0xC0, 0x01, &[])?;
                V2_CODES.iter().find(|(c, _)| *c == r[9]).map(|(_, hz)| *hz).context(t!("codice polling sconosciuto", "unknown polling code"))
            }
            Polling::None => bail!(t!("polling non configurabile", "polling not configurable")),
        }
    }

    fn read_effect(&self, led: u8) -> Result<Effect> {
        if self.cfg.lighting == Lighting::Classic {
            if self.cmd_for("lighting", 0x03, 0x80, 0x03, &[VARSTORE, led])?[10] == 0 {
                return Ok(Effect::Off);
            }
            let c = self.cmd_for("lighting", 0x03, 0x81, 0x05, &[VARSTORE, led])?;
            let color = Rgb(c[10], c[11], c[12]);
            return Ok(match self.cmd_for("lighting", 0x03, 0x82, 0x03, &[VARSTORE, led])?[10] {
                0x00 => Effect::Static { color },
                0x02 => Effect::Breathing { color },
                0x04 => Effect::Spectrum,
                c => Effect::Unknown { code: c },
            });
        }
        let r = self.cmd_for("lighting", 0x0F, 0x82, 0x0C, &[VARSTORE, led])?;
        let a = &r[8..];
        let color = Rgb(a[6], a[7], a[8]);
        Ok(match a[2] {
            0x00 => Effect::Off,
            0x01 => Effect::Static { color },
            0x02 if a[5] >= 1 => Effect::Breathing { color },
            0x03 => Effect::Spectrum,
            0x04 => Effect::Wave { reverse: a[3] == 0x01 },
            0x05 => Effect::Reactive { color },
            c => Effect::Unknown { code: c },
        })
    }

    fn read_brightness(&self, led: u8) -> Result<u8> {
        match self.cfg.lighting {
            Lighting::Classic => Ok(self.cmd_for("lighting", 0x03, 0x83, 0x03, &[VARSTORE, led])?[10]),
            _ => Ok(self.cmd_for("lighting", 0x0F, 0x84, 0x03, &[VARSTORE, led])?[10]),
        }
    }

    fn write_effect(&self, led: u8, effect: &Effect) -> Result<()> {
        if self.cfg.lighting == Lighting::Classic {
            let l = |id: u8, size: u8, args: &[u8]| self.cmd_for("lighting", 0x03, id, size, args).map(|_| ());
            let rgb = |c: Rgb| l(0x01, 0x05, &[VARSTORE, led, c.0, c.1, c.2]);
            let effect_id = match *effect {
                Effect::Off => return l(0x00, 0x03, &[VARSTORE, led, 0x00]),
                Effect::Static { color } => {
                    rgb(color)?;
                    0x00
                }
                Effect::Breathing { color } => {
                    rgb(color)?;
                    0x02
                }
                Effect::Spectrum => 0x04,
                ref e => bail!(tf!("effetto '{}' non disponibile sui Razer con LED classici", "effect '{}' not available on Razer mice with classic LEDs", e.name())),
            };
            l(0x00, 0x03, &[VARSTORE, led, 0x01])?;
            return l(0x02, 0x03, &[VARSTORE, led, effect_id]);
        }
        let (size, args): (u8, Vec<u8>) = match *effect {
            Effect::Off => (0x06, vec![VARSTORE, led, 0x00, 0, 0, 0]),
            Effect::Static { color: Rgb(r, g, b) } => (0x09, vec![VARSTORE, led, 0x01, 0, 0, 0x01, r, g, b]),
            Effect::Breathing { color: Rgb(r, g, b) } => (0x09, vec![VARSTORE, led, 0x02, 0x01, 0, 0x01, r, g, b]),
            Effect::Spectrum => (0x06, vec![VARSTORE, led, 0x03, 0, 0, 0]),
            Effect::Wave { reverse } => (0x06, vec![VARSTORE, led, 0x04, reverse as u8, 0x28, 0]),
            Effect::Reactive { color: Rgb(r, g, b) } => (0x09, vec![VARSTORE, led, 0x05, 0, 0x02, 0x01, r, g, b]),
            Effect::Unknown { code } => bail!(tf!("effetto sconosciuto 0x{code:02x}: non lo scrivo", "unknown effect 0x{code:02x}: not writing it")),
        };
        self.cmd_for("lighting", 0x0F, 0x02, size, &args).map(|_| ())
    }
}

impl MouseDriver for Razer {
    fn spec(&self) -> &DeviceSpec {
        &self.spec
    }

    fn identify(&mut self) -> Result<DeviceInfo> {
        let fw = self.cmd(0x00, 0x81, 0x02, &[]).ok().map(|r| format!("v{}.{}", r[8], r[9]));
        // Il numero di serie non lo leggiamo: finirebbe nel registro allegato ai report pubblici.
        Ok(DeviceInfo {
            name: self.spec.name.clone(),
            vid: self.spec.vid,
            pid: self.spec.pid,
            status: self.spec.status,
            firmware: fw,
            serial: None,
        })
    }

    fn read_state(&mut self) -> Result<MouseState> {
        let mut zones = Vec::new();
        if self.cfg.lighting != Lighting::None {
            for zone in self.spec.caps.zones.clone() {
                let led = self.led(&zone)?;
                let brightness = if self.cfg.brightness { self.read_brightness(led).ok() } else { None };
                zones.push(ZoneState { zone, effect: self.read_effect(led).ok(), brightness });
            }
        }
        Ok(MouseState {
            dpi: if self.cfg.dpi { self.read_dpi().ok() } else { None },
            dpi_stages: if self.cfg.dpi { self.read_stages().ok() } else { None },
            polling_hz: if self.cfg.polling == Polling::None { None } else { self.read_polling().ok() },
            zones,
            cached: false,
        })
    }

    fn set_dpi_stages(&mut self, stages: &[u16], active: usize) -> Result<()> {
        self.spec.caps.check_dpi_stages(stages, active)?;
        if !self.cfg.dpi {
            bail!(t!("DPI non configurabili su questo modello", "DPI not configurable on this model"));
        }
        if !self.cfg.dpi_stages {
            let [h, l] = stages[0].to_be_bytes();
            self.cmd_for("dpi", 0x04, 0x05, 0x07, &[VARSTORE, h, l, h, l, 0, 0])?;
            let back = self.read_dpi()?;
            if back != stages[0] {
                bail!(tf!("verifica fallita: DPI {back} invece di {}", "check failed: DPI {back} instead of {}", stages[0]));
            }
            return Ok(());
        }
        // ID degli stadi da 1, come li restituisce il mouse in lettura.
        let mut args = vec![VARSTORE, active as u8 + 1, stages.len() as u8];
        for (i, &d) in stages.iter().enumerate() {
            let [h, l] = d.to_be_bytes();
            args.extend_from_slice(&[i as u8 + 1, h, l, h, l, 0, 0]);
        }
        self.cmd_for("dpi_stages", 0x04, 0x06, 0x26, &args)?;
        // Il DPI corrente non cambia da solo: lo allineo allo stadio attivo.
        let [h, l] = stages[active].to_be_bytes();
        self.cmd_for("dpi", 0x04, 0x05, 0x07, &[VARSTORE, h, l, h, l, 0, 0])?;

        let back = self.read_stages()?;
        if back.stages != stages || back.active != active {
            bail!(tf!("verifica fallita: il mouse riporta stadi {:?} attivo {}", "check failed: the mouse reports stages {:?} active {}", back.stages, back.active + 1));
        }
        let dpi = self.read_dpi()?;
        if dpi != stages[active] {
            bail!(tf!("verifica fallita: DPI attuale {dpi} invece di {}", "check failed: current DPI {dpi} instead of {}", stages[active]));
        }
        Ok(())
    }

    fn set_polling(&mut self, hz: u16) -> Result<()> {
        self.spec.caps.check_polling(hz)?;
        match self.cfg.polling {
            Polling::Standard => {
                let code = match hz {
                    1000 => 0x01,
                    500 => 0x02,
                    125 => 0x08,
                    _ => bail!(tf!("polling {hz} Hz non gestito dal protocollo Razer standard", "polling {hz} Hz not supported by the standard Razer protocol")),
                };
                self.cmd_for("polling", 0x00, 0x05, 0x01, &[code])?;
            }
            Polling::V2 | Polling::V2Dual => {
                let code = V2_CODES.iter().find(|(_, h)| *h == hz).map(|(c, _)| *c).context(t!("polling non gestito", "polling rate not supported"))?;
                self.cmd_for("polling", 0x00, 0x40, 0x02, &[0x00, code])?;
                if self.cfg.polling == Polling::V2Dual {
                    self.cmd_for("polling", 0x00, 0x40, 0x02, &[0x01, code])?;
                }
            }
            Polling::None => bail!(t!("polling non configurabile su questo modello", "polling not configurable on this model")),
        }
        let back = self.read_polling()?;
        if back != hz {
            bail!(tf!("verifica fallita: il mouse riporta {back} Hz", "check failed: the mouse reports {back} Hz"));
        }
        Ok(())
    }

    fn set_lighting(&mut self, zone: &str, effect: &Effect) -> Result<()> {
        self.spec.caps.check_effect(zone, effect)?;
        let led = self.led(zone)?;
        self.write_effect(led, effect)?;
        let back = self.read_effect(led)?;
        if back.name() != effect.name() || back.color() != effect.color() {
            bail!(tf!("verifica fallita su {zone}: il mouse riporta {back}", "check failed on {zone}: the mouse reports {back}"));
        }
        Ok(())
    }

    fn set_brightness(&mut self, zone: &str, value: u8) -> Result<()> {
        if !self.cfg.brightness {
            bail!(t!("luminosità non configurabile su questo modello", "brightness not configurable on this model"));
        }
        let led = self.led(zone)?;
        match self.cfg.lighting {
            Lighting::Classic => self.cmd_for("lighting", 0x03, 0x03, 0x03, &[VARSTORE, led, value])?,
            _ => self.cmd_for("lighting", 0x0F, 0x04, 0x03, &[VARSTORE, led, value])?,
        };
        let back = self.read_brightness(led)?;
        if back != value {
            bail!(tf!("verifica fallita su {zone}: luminosità {back} invece di {value}", "check failed on {zone}: brightness {back} instead of {value}"));
        }
        Ok(())
    }
}

/// Probe di sola lettura per un mouse Razer che non è nel database (kit, Modalità A).
/// Prova ogni interfaccia e i transaction id noti, poi esegue solo comandi di lettura.
pub fn probe_unknown(api: &HidApi, vid: u16, pid: u16) -> serde_json::Value {
    use serde_json::json;
    let mut tried = Vec::new();
    for info in api.device_list().filter(|d| d.vendor_id() == vid && d.product_id() == pid) {
        let tag = format!("if{} {:04x}/{:02x}", info.interface_number(), info.usage_page(), info.usage());
        let Ok(dev) = info.open_device(api) else {
            tried.push(json!({ "interface": tag, "result": "non apribile" }));
            continue;
        };
        for tid in [0x1F, 0x3F, 0xFF] {
            let Ok(r) = raw_transact(&dev, tid, 0x00, 0x81, 0x02, &[]) else { continue };
            if r[0] != 0x02 {
                continue;
            }
            let hex = |r: &[u8; LEN]| r.iter().map(|b| format!("{b:02x}")).collect::<Vec<_>>().join(" ");
            let mut reads = serde_json::Map::new();
            for (name, class, id, size, args) in [
                ("firmware", 0x00, 0x81, 0x02, vec![]),
                ("polling", 0x00, 0x85, 0x01, vec![]),
                ("dpi", 0x04, 0x85, 0x07, vec![VARSTORE]),
                ("dpi_stages", 0x04, 0x86, 0x26, vec![VARSTORE]),
            ] {
                let v = match raw_transact(&dev, tid, class, id, size, &args) {
                    Ok(r) => json!({ "status": status_text(r[0]), "raw": hex(&r) }),
                    Err(e) => json!(e.to_string()),
                };
                reads.insert(name.into(), v);
            }
            let mut leds = serde_json::Map::new();
            for led in 0x00..=0x20u8 {
                if let Ok(r) = raw_transact(&dev, tid, 0x0F, 0x84, 0x03, &[VARSTORE, led]) {
                    if r[0] == 0x02 {
                        let eff = raw_transact(&dev, tid, 0x0F, 0x82, 0x0C, &[VARSTORE, led]).map(|e| hex(&e)).unwrap_or_default();
                        leds.insert(format!("0x{led:02x}"), json!({ "brightness": r[10], "effect_raw": eff }));
                    }
                }
            }
            return json!({ "interface": tag, "transaction_id": format!("0x{tid:02x}"), "reads": reads, "leds": leds, "tried": tried });
        }
        tried.push(json!({ "interface": tag, "result": "nessuna risposta" }));
    }
    json!({ "result": "nessuna interfaccia ha risposto", "tried": tried })
}
