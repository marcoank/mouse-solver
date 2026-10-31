//! Driver ASUS ROG (protocollo dei mouse recenti: Harpe, Keris, Gladius III...).
//! Reimplementato leggendo G-Helper (app/Peripherals/Mouse/AsusMouse.cs e Models/*.cs,
//! licenza GPL-3.0): nessun codice copiato.
//!
//! Pacchetti da 64 byte su report output, risposta su report input:
//! `[0]` report ID, `[1]` comando (0x12 leggi, 0x51 scrivi, 0x50 profilo/salva), `[2]` gruppo, `[3]` voce.
//! La risposta ripete i primi byte del comando; `FF AA` = comando non supportato.

use anyhow::{bail, Context, Result};
use hidapi::{HidApi, HidDevice};
use mousecfg_core::*;
use serde::Deserialize;
use std::collections::BTreeMap;

const POLLING: [u16; 7] = [125, 250, 500, 1000, 2000, 4000, 8000];

fn yes() -> bool {
    true
}

fn hundred() -> u8 {
    100
}

#[derive(Debug, Clone, Deserialize)]
pub struct AsusConfig {
    /// Interfaccia HID da provare per prima (le altre sono il ripiego).
    #[serde(default)]
    pub interface: Option<i32>,
    #[serde(default)]
    pub report_id: u8,
    #[serde(default = "default_packet")]
    pub packet_size: usize,
    /// Parte del percorso HID da preferire (es. "col03" per il ricevitore OMNI).
    #[serde(default)]
    pub path_contains: Option<String>,
    /// Il mouse ha DPI separati per X e Y (cambia il comando di lettura).
    #[serde(default)]
    pub dpi_xy: bool,
    /// Ogni stadio DPI ha un colore: va riscritto insieme al DPI.
    #[serde(default)]
    pub dpi_colors: bool,
    #[serde(default = "default_increment")]
    pub dpi_increment: u16,
    /// Funzioni disattivate se il modello usa pacchetti diversi (vedi tools/gen_asus.py).
    #[serde(default = "yes")]
    pub dpi: bool,
    #[serde(default = "yes")]
    pub polling: bool,
    #[serde(default = "yes")]
    pub lighting: bool,
    /// Il numero di stadi DPI si può cambiare (comando 0x51 0x31 0x0A).
    #[serde(default)]
    pub dpi_count_settable: bool,
    /// Lo stadio attivo si può scegliere (comando 0x51 0x31 0x09).
    #[serde(default = "yes")]
    pub dpi_active_settable: bool,
    /// Valore massimo della luminosità per questo modello (di solito 100).
    #[serde(default = "hundred")]
    pub brightness_max: u8,
    /// zona -> codice zona per la scrittura (logo 0, rotella 1, sotto 2).
    pub zones: BTreeMap<String, u8>,
}

fn default_packet() -> usize {
    64
}

fn default_increment() -> u16 {
    50
}

/// Prefissi dei comandi ammessi. Niente cambio profilo, tasti, firmware.
fn allowed(cmd: &[u8]) -> bool {
    matches!(
        cmd,
        [0x12, 0x00, ..]                         // profilo / stadio attivo / numero stadi
        | [0x12, 0x03, ..]                       // luci
        | [0x12, 0x04, 0x00..=0x03, ..]          // DPI, polling, colori DPI
        | [0x12, 0x07, ..]                       // batteria
        | [0x51, 0x31, 0x00..=0x03, ..]          // DPI di uno stadio
        | [0x51, 0x31, 0x04 | 0x09 | 0x0A, ..]   // polling, stadio attivo, numero stadi
        | [0x51, 0x28, ..]                       // luci
        | [0x50, 0x03, ..] // salva
    )
}

fn transact(dev: &HidDevice, cfg: &AsusConfig, cmd: &[u8]) -> Result<Vec<u8>> {
    if !allowed(cmd) {
        bail!("comando {:02x?} bloccato: non è nella lista dei comandi sicuri", cmd);
    }
    let mut pkt = vec![0u8; cfg.packet_size];
    pkt[0] = cfg.report_id;
    pkt[1..1 + cmd.len()].copy_from_slice(cmd);

    // Svuota eventuali risposte vecchie.
    let mut junk = [0u8; 128];
    while dev.read_timeout(&mut junk, 1).unwrap_or(0) > 0 {}

    dev.write(&pkt)?;
    let mut resp = Vec::new();
    for _ in 0..10 {
        let mut buf = vec![0u8; cfg.packet_size + 1];
        let n = dev.read_timeout(&mut buf, 300)?;
        if n == 0 {
            bail!("nessuna risposta (il mouse è spento o in standby?)");
        }
        // hidapi su Windows toglie il report ID 0: lo rimetto per avere gli stessi indici di G-Helper.
        resp = if cfg.report_id == 0 { [&[0u8][..], &buf[..n]].concat() } else { buf[..n].to_vec() };
        resp.resize(cfg.packet_size.max(32), 0);
        if resp[1] == 0xFF && resp[2] == 0xAA {
            bail!("il mouse non supporta il comando {:02x?}", cmd);
        }
        if resp[1..4].iter().all(|&b| b == 0) {
            bail!("risposta vuota (il mouse è spento o in standby?)");
        }
        let need = cmd.len().min(3);
        if resp[1..1 + need] == cmd[..need] {
            return Ok(resp);
        }
    }
    bail!("risposta non corrispondente al comando {:02x?}: {:02x?}", cmd, &resp[..8])
}

pub struct Asus {
    dev: HidDevice,
    spec: DeviceSpec,
    cfg: AsusConfig,
}

#[derive(Clone, Copy)]
struct Light {
    mode: u8,
    brightness: u8, // 0-100
    color: Rgb,
}

impl Asus {
    pub fn open(api: &HidApi, spec: &DeviceSpec, vid: u16, pid: u16) -> Result<Asus> {
        let cfg: AsusConfig = spec.section("asus")?;
        let c = cfg.clone();
        let dev = crate::open_first(
            api,
            vid,
            pid,
            |d| {
                c.interface.is_none_or(|i| d.interface_number() == i)
                    && c.path_contains.as_ref().is_none_or(|p| d.path().to_string_lossy().to_lowercase().contains(p))
            },
            |dev| transact(dev, &c, &[0x12, 0x00]).is_ok(),
        )
        .with_context(|| format!("{} trovato ma non risponde. Prova a chiudere Armoury Crate; se è wireless, muovilo per svegliarlo.", spec.name))?;
        let mut spec = spec.clone();
        (spec.vid, spec.pid) = (vid, pid);
        Ok(Asus { dev, spec, cfg })
    }

    fn tx(&self, cmd: &[u8]) -> Result<Vec<u8>> {
        transact(&self.dev, &self.cfg, cmd)
    }

    fn save(&self) -> Result<()> {
        self.tx(&[0x50, 0x03]).map(|_| ())
    }

    /// (indice stadio attivo, numero di stadi)
    fn read_profile(&self) -> Result<(usize, usize)> {
        let r = self.tx(&[0x12, 0x00, 0x00])?;
        let max = self.spec.caps.max_stages as usize;
        let count = match r[19] as usize {
            n if self.cfg.dpi_count_settable && (self.spec.caps.min_stages as usize..=max).contains(&n) => n,
            _ => max,
        };
        if !self.cfg.dpi_active_settable {
            return Ok((0, count));
        }
        // G-Helper conta gli stadi da 1; uno 0 lo tratto come primo stadio.
        let active = (r[12] as usize).saturating_sub(1).min(count - 1);
        Ok((active, count))
    }

    fn read_dpis(&self, count: usize) -> Result<Vec<u16>> {
        let xy = self.cfg.dpi_xy;
        let r = self.tx(&[0x12, 0x04, if xy { 0x02 } else { 0x00 }])?;
        let inc = self.cfg.dpi_increment;
        Ok((0..count)
            .map(|i| {
                let o = 5 + i * if xy { 4 } else { 2 };
                u16::from_le_bytes([r[o], r[o + 1]]) * inc + inc
            })
            .collect())
    }

    fn read_dpi_colors(&self) -> Vec<Rgb> {
        let max = self.spec.caps.max_stages as usize;
        match self.cfg.dpi_colors.then(|| self.tx(&[0x12, 0x04, 0x03])) {
            Some(Ok(r)) => (0..max).map(|i| Rgb(r[5 + i * 3], r[6 + i * 3], r[7 + i * 3])).collect(),
            _ => vec![Rgb(255, 255, 255); max],
        }
    }

    fn read_polling(&self) -> Result<u16> {
        let r = self.tx(&[0x12, 0x04, 0x00])?;
        POLLING.get((r[13] & 0x07) as usize).copied().context("codice polling sconosciuto")
    }

    fn zone_index(&self, zone: &str) -> Result<u8> {
        self.spec.caps.zones.iter().position(|z| z == zone).map(|i| i as u8).with_context(|| format!("zona '{zone}' sconosciuta"))
    }

    fn read_light(&self, zone: &str) -> Result<Light> {
        let r = self.tx(&[0x12, 0x03, self.zone_index(zone)?])?;
        Ok(Light { mode: r[5], brightness: r[6].min(self.cfg.brightness_max), color: Rgb(r[7], r[8], r[9]) })
    }

    fn write_light(&self, zone: &str, l: Light) -> Result<()> {
        let code = *self.cfg.zones.get(zone).with_context(|| format!("zona '{zone}' senza codice nel TOML"))?;
        let Rgb(r, g, b) = l.color;
        let speed = if l.mode == 0x03 { 0x07 } else { 0x00 }; // velocità media solo per l'arcobaleno
        self.tx(&[0x51, 0x28, code, 0x00, l.mode, l.brightness.min(self.cfg.brightness_max), r, g, b, 0x00, 0x00, speed])?;
        self.save()
    }
}

fn effect_of(l: Light) -> Effect {
    let color = l.color;
    match l.mode {
        0x00 => Effect::Static { color },
        0x01 => Effect::Breathing { color },
        0x02 => Effect::Spectrum, // "color cycle"
        0x03 => Effect::Wave { reverse: false }, // "rainbow"
        0x04 => Effect::Reactive { color },
        0xF0 => Effect::Off,
        c => Effect::Unknown { code: c },
    }
}

impl MouseDriver for Asus {
    fn spec(&self) -> &DeviceSpec {
        &self.spec
    }

    fn identify(&mut self) -> Result<DeviceInfo> {
        Ok(DeviceInfo {
            name: self.spec.name.clone(),
            vid: self.spec.vid,
            pid: self.spec.pid,
            status: self.spec.status,
            firmware: None,
            serial: None,
        })
    }

    fn read_state(&mut self) -> Result<MouseState> {
        let stages = if self.cfg.dpi {
            self.read_profile().ok().and_then(|(active, count)| self.read_dpis(count).ok().map(|stages| DpiStages { stages, active }))
        } else {
            None
        };
        let max_b = self.cfg.brightness_max.max(1) as u16;
        let zones = if !self.cfg.lighting { Vec::new() } else { self
            .spec
            .caps
            .zones
            .iter()
            .map(|z| {
                let l = self.read_light(z).ok();
                ZoneState {
                    zone: z.clone(),
                    effect: l.map(effect_of),
                    brightness: l.map(|l| ((l.brightness as u16 * 255 + max_b / 2) / max_b) as u8),
                }
            })
            .collect() };
        Ok(MouseState {
            dpi: stages.as_ref().map(|s| s.stages[s.active]),
            dpi_stages: stages,
            polling_hz: if self.cfg.polling { self.read_polling().ok() } else { None },
            zones,
            cached: false,
        })
    }

    fn set_dpi_stages(&mut self, stages: &[u16], active: usize) -> Result<()> {
        let caps = self.spec.caps.clone();
        caps.check_dpi_stages(stages, active)?;
        if !self.cfg.dpi {
            bail!("DPI non configurabili su questo modello");
        }
        let inc = self.cfg.dpi_increment;
        if stages.iter().any(|d| d % inc != 0) {
            bail!("i DPI devono essere multipli di {inc}");
        }
        let colors = self.read_dpi_colors();
        let (_, count) = self.read_profile()?;
        if count != stages.len() {
            if !self.cfg.dpi_count_settable {
                bail!("su questo modello il numero di stadi è fisso ({count})");
            }
            self.tx(&[0x51, 0x31, 0x0A, 0x00, stages.len() as u8])?;
        }
        for (i, &d) in stages.iter().enumerate() {
            let [lo, hi] = ((d - inc) / inc).to_le_bytes();
            let mut cmd = vec![0x51, 0x31, i as u8, 0x00, lo, hi];
            if self.cfg.dpi_colors {
                let c = colors[i];
                cmd.extend_from_slice(&[c.0, c.1, c.2]);
            }
            self.tx(&cmd)?;
        }
        if self.cfg.dpi_active_settable {
            self.tx(&[0x51, 0x31, 0x09, 0x00, active as u8 + 1])?;
        }
        self.save()?;

        let (back_active, back_count) = self.read_profile()?;
        let back = self.read_dpis(back_count)?;
        if back != stages || (self.cfg.dpi_active_settable && back_active != active) {
            bail!("verifica fallita: il mouse riporta stadi {back:?} attivo {}", back_active + 1);
        }
        Ok(())
    }

    fn set_polling(&mut self, hz: u16) -> Result<()> {
        self.spec.caps.check_polling(hz)?;
        if !self.cfg.polling {
            bail!("polling non configurabile su questo modello");
        }
        let code = POLLING.iter().position(|&p| p == hz).context("polling non gestito")? as u8;
        self.tx(&[0x51, 0x31, 0x04, 0x00, code])?;
        self.save()?;
        let back = self.read_polling()?;
        if back != hz {
            bail!("verifica fallita: il mouse riporta {back} Hz");
        }
        Ok(())
    }

    fn set_lighting(&mut self, zone: &str, effect: &Effect) -> Result<()> {
        self.spec.caps.check_effect(zone, effect)?;
        let mut l = self.read_light(zone)?;
        l.mode = match effect {
            Effect::Static { .. } => 0x00,
            Effect::Breathing { .. } => 0x01,
            Effect::Spectrum => 0x02,
            Effect::Wave { .. } => 0x03,
            Effect::Reactive { .. } => 0x04,
            Effect::Off => 0xF0,
            Effect::Unknown { code } => bail!("effetto sconosciuto 0x{code:02x}: non lo scrivo"),
        };
        if let Some(c) = effect.color() {
            l.color = c;
        }
        self.write_light(zone, l)?;
        let back = effect_of(self.read_light(zone)?);
        if back.name() != effect.name() || back.color() != effect.color() {
            bail!("verifica fallita su {zone}: il mouse riporta {back}");
        }
        Ok(())
    }

    fn set_brightness(&mut self, zone: &str, value: u8) -> Result<()> {
        let mut l = self.read_light(zone)?;
        let max = self.cfg.brightness_max as u16;
        l.brightness = ((value as u16 * max + 127) / 255) as u8;
        self.write_light(zone, l)?;
        let back = self.read_light(zone)?.brightness;
        if back != l.brightness {
            bail!("verifica fallita su {zone}: luminosità {back} invece di {}", l.brightness);
        }
        Ok(())
    }
}

/// Probe di sola lettura per un mouse ASUS non nel database (kit, Modalità A).
pub fn probe_unknown(api: &HidApi, vid: u16, pid: u16) -> serde_json::Value {
    use serde_json::json;
    let mut out = Vec::new();
    for info in api.device_list().filter(|d| d.vendor_id() == vid && d.product_id() == pid) {
        let tag = format!("if{} {:04x}/{:02x}", info.interface_number(), info.usage_page(), info.usage());
        let Ok(dev) = info.open_device(api) else { continue };
        for (report_id, packet_size) in [(0x00, 64), (0x00, 65), (0x03, 64)] {
            let cfg = AsusConfig {
                interface: Some(info.interface_number()),
                report_id,
                packet_size,
                path_contains: None,
                dpi_xy: false,
                dpi_colors: false,
                dpi_increment: 50,
                dpi: true,
                polling: true,
                lighting: true,
                dpi_count_settable: false,
                dpi_active_settable: true,
                brightness_max: 100,
                zones: BTreeMap::new(),
            };
            if transact(&dev, &cfg, &[0x12, 0x00]).is_err() {
                continue;
            }
            let mut reads = serde_json::Map::new();
            for cmd in [&[0x12, 0x00][..], &[0x12, 0x04, 0x00], &[0x12, 0x04, 0x02], &[0x12, 0x04, 0x03], &[0x12, 0x03, 0x00], &[0x12, 0x03, 0x01], &[0x12, 0x03, 0x02], &[0x12, 0x07]] {
                let v = match transact(&dev, &cfg, cmd) {
                    Ok(r) => json!(r.iter().map(|b| format!("{b:02x}")).collect::<Vec<_>>().join(" ")),
                    Err(e) => json!(e.to_string()),
                };
                reads.insert(format!("{cmd:02x?}"), v);
            }
            out.push(json!({ "interface": tag, "report_id": report_id, "packet_size": packet_size, "reads": reads }));
            break;
        }
    }
    json!(out)
}
