//! Driver SteelSeries guidato dai dati. I file devices/steelseries_*.toml sono generati da rivalcfg
//! (licenza WTFPL) con tools/gen_steelseries.py: contengono i comandi esatti e le tabelle
//! DPI -> codice, quindi questo driver non conosce i singoli modelli.
//!
//! Questi mouse accettano solo scritture: DPI, polling e colori non si possono rileggere.
//! Lo stato mostrato è quello dell'ultima scrittura (core::cache). L'unica lettura possibile
//! è la versione firmware (se il modello la offre), usata per capire se il mouse risponde.
//! I soli comandi inviabili sono quelli scritti nel TOML.

use anyhow::{bail, Context, Result};
use hidapi::{HidApi, HidDevice};
use mousecfg_core::*;
use serde::Deserialize;
use std::collections::BTreeMap;
use std::thread::sleep;
use std::time::Duration;

#[derive(Debug, Clone, Deserialize)]
pub struct Cmd {
    /// "output" o "feature"
    pub report: String,
    pub command: Vec<u8>,
    #[serde(default)]
    pub suffix: Vec<u8>,
    /// Lunghezza fissa del pacchetto (0 = nessuna).
    #[serde(default)]
    pub packet_length: usize,
    /// Byte da rileggere dopo il comando (versioni wireless).
    #[serde(default)]
    pub readback: usize,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Polling {
    pub cmd: Cmd,
    /// "Hz" -> byte
    pub values: BTreeMap<String, Vec<u8>>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Dpi {
    /// "presets": un comando per preset (es. Rival 310); "multi": un comando con tutti gli stadi.
    pub mode: String,
    #[serde(default)]
    pub cmds: Vec<Cmd>,
    #[serde(default)]
    pub cmd: Option<Cmd>,
    /// [DPI, codice]
    pub codes: Vec<[u32; 2]>,
    #[serde(default = "one")]
    pub code_bytes: usize,
    #[serde(default = "number")]
    pub count_mode: String,
    #[serde(default)]
    pub first_preset: u8,
    #[serde(default = "none")]
    pub xy: String,
    #[serde(default)]
    pub max_presets: usize,
}

fn one() -> usize {
    1
}
fn number() -> String {
    "number".into()
}
fn none() -> String {
    "none".into()
}

#[derive(Debug, Clone, Deserialize)]
pub struct EffectCmd {
    pub cmd: Cmd,
    /// nome effetto (static, breathing, spectrum) -> byte
    pub values: BTreeMap<String, Vec<u8>>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Brightness {
    pub cmd: Cmd,
    pub max: u32,
    #[serde(default = "one")]
    pub code_bytes: usize,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Zone {
    pub zone: String,
    /// rgbcolor | rgbgradient | rgbgradientv2
    pub kind: String,
    pub cmd: Cmd,
    #[serde(default)]
    pub led_id: Option<u8>,
    #[serde(default)]
    pub header: BTreeMap<String, toml_like::Value>,
}

/// Valori dell'intestazione: interi o liste di interi.
pub mod toml_like {
    use serde::Deserialize;
    #[derive(Debug, Clone, Deserialize)]
    #[serde(untagged)]
    pub enum Value {
        Int(usize),
        List(Vec<usize>),
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct SteelConfig {
    pub interface: i32,
    #[serde(default)]
    pub save: Option<Cmd>,
    #[serde(default)]
    pub firmware: Option<Cmd>,
    #[serde(default)]
    pub polling: Option<Polling>,
    #[serde(default)]
    pub dpi: Option<Dpi>,
    #[serde(default)]
    pub effect: Option<EffectCmd>,
    #[serde(default)]
    pub brightness: Option<Brightness>,
    #[serde(default)]
    pub zones: Vec<Zone>,
    /// Durata in ms del ciclo di colori per "spectrum".
    #[serde(default = "default_cycle")]
    pub spectrum_ms: u16,
}

fn default_cycle() -> u16 {
    3000
}

pub struct SteelSeries {
    dev: HidDevice,
    spec: DeviceSpec,
    cfg: SteelConfig,
}

fn send(dev: &HidDevice, c: &Cmd, payload: &[u8]) -> Result<()> {
    let mut data = vec![0x00]; // report ID
    data.extend_from_slice(&c.command);
    data.extend_from_slice(payload);
    data.extend_from_slice(&c.suffix);
    if c.packet_length > 0 && data.len() < c.packet_length + 1 {
        data.resize(c.packet_length + 1, 0);
    }
    if std::env::var_os("MOUSECFG_DEBUG").is_some() {
        eprintln!("> {} {:02x?}", c.report, &data[1..]);
    }
    match c.report.as_str() {
        "output" => dev.write(&data).map(|_| ())?,
        "feature" => dev.send_feature_report(&data)?,
        r => bail!("tipo di report '{r}' sconosciuto"),
    }
    if c.readback > 0 {
        let mut buf = vec![0u8; c.readback];
        let _ = dev.read_timeout(&mut buf, 200);
    }
    sleep(Duration::from_millis(20));
    Ok(())
}

fn firmware(dev: &HidDevice, c: &Cmd) -> Option<String> {
    send(dev, c, &[]).ok()?;
    let mut buf = [0u8; 8];
    let n = dev.read_timeout(&mut buf, 200).ok()?;
    (n >= 2).then(|| format!("v{}.{:02}", buf[0], buf[1]))
}

fn le(v: u32, n: usize) -> Vec<u8> {
    v.to_le_bytes()[..n].to_vec()
}

/// Colore fisso col formato "rgbgradient" (Rival 310, 600...): intestazione + colori.
fn gradient_packet(z: &Zone, stops: &[(u8, Rgb)], duration_ms: u16) -> Result<Vec<u8>> {
    let h = |k: &str| match z.header.get(k) {
        Some(toml_like::Value::Int(v)) => Ok(*v),
        _ => bail!("intestazione '{k}' mancante"),
    };
    let mut header = vec![0u8; h("header_length")?];
    let led = z.led_id.unwrap_or(0);
    match z.header.get("led_id_offsets") {
        Some(toml_like::Value::List(offs)) => offs.iter().for_each(|&o| header[o] = led),
        _ => header[0] = led,
    }
    let (doff, dlen) = (h("duration_offset")?, h("duration_length")?);
    header[doff..doff + dlen].copy_from_slice(&le(duration_ms as u32, dlen));
    header[h("repeat_offset")?] = (stops.len() == 1) as u8;
    header[h("triggers_offset")?] = 0;
    header[h("color_count_offset")?] = stops.len() as u8;

    let mut body = vec![stops[0].1 .0, stops[0].1 .1, stops[0].1 .2];
    let mut last = 0u8;
    for &(pos, c) in stops {
        body.extend_from_slice(&[c.0, c.1, c.2, pos - last]);
        last = pos;
    }
    header.extend(body);
    Ok(header)
}

/// Colore fisso col formato "rgbgradientv2" (Rival 500/700): un solo colore, nessuna sfumatura.
fn gradient_v2_static(z: &Zone, c: Rgb) -> Result<Vec<u8>> {
    let field = match z.header.get("color_field_length") {
        Some(toml_like::Value::Int(v)) => *v,
        _ => bail!("intestazione 'color_field_length' mancante"),
    };
    let dlen = match z.header.get("duration_length") {
        Some(toml_like::Value::Int(v)) => *v,
        _ => 2,
    };
    let mut p = vec![z.led_id.unwrap_or(0), 0x1D, 0x01, 0x02, 0x31, 0x51, 0xFF, 0xC8, 0x00];
    p.resize(field, 0);
    for ch in [c.0, c.1, c.2] {
        p.push((ch & 0x0F) << 4); // nibble basso nella parte alta
        p.push(ch >> 4); // nibble alto nella parte bassa
    }
    p.extend_from_slice(&[0xFF, 0x00]);
    p.extend(le(1500, 2)); // focal x
    p.extend(le(650, 2)); // focal y
    p.extend_from_slice(&[0x00, 0x00, 0x00, 0x00, 0x01, 0x00]);
    p.extend(le(0, 2)); // colori - 1
    p.extend(le(1000, dlen));
    Ok(p)
}

impl SteelSeries {
    pub fn open(api: &HidApi, spec: &DeviceSpec, vid: u16, pid: u16) -> Result<SteelSeries> {
        let cfg: SteelConfig = spec.section("steelseries")?;
        let iface = cfg.interface;
        let fw = cfg.firmware.clone();
        // Con il comando firmware verifico che l'interfaccia risponda; senza, prendo la prima
        // collezione dell'interfaccia giusta (come fa rivalcfg), preferendo quelle del produttore.
        let dev = crate::open_first(api, vid, pid, |d| d.interface_number() == iface && d.usage_page() >= 0xFF00, |dev| {
            fw.as_ref().is_none_or(|c| firmware(dev, c).is_some())
        });
        let dev = match dev {
            Some(d) => d,
            None if fw.is_some() => bail!("{} trovato ma non risponde. Prova a chiudere SteelSeries GG.", spec.name),
            None => api
                .device_list()
                .find(|d| d.vendor_id() == vid && d.product_id() == pid && d.interface_number() == iface)
                .context("interfaccia di controllo non trovata")?
                .open_device(api)?,
        };
        let mut spec = spec.clone();
        (spec.vid, spec.pid) = (vid, pid);
        Ok(SteelSeries { dev, spec, cfg })
    }

    fn finish(&self, update: impl FnOnce(&mut MouseState)) -> Result<()> {
        if let Some(c) = &self.cfg.save {
            send(&self.dev, c, &[])?;
        }
        // Non c'è read-back: dove possibile controllo almeno che il mouse risponda ancora.
        if let Some(c) = &self.cfg.firmware {
            if firmware(&self.dev, c).is_none() {
                bail!("il mouse non risponde dopo la scrittura");
            }
        }
        let mut s = cache::load(self.spec.vid, self.spec.pid);
        update(&mut s);
        cache::save(self.spec.vid, self.spec.pid, &s);
        Ok(())
    }

    fn code(&self, dpi: &Dpi, d: u16) -> Result<Vec<u8>> {
        let code = dpi.codes.iter().find(|c| c[0] == d as u32).with_context(|| format!("DPI {d} non disponibile"))?[1];
        Ok(le(code, dpi.code_bytes))
    }

    fn zone(&self, name: &str) -> Result<&Zone> {
        self.cfg.zones.iter().find(|z| z.zone == name).with_context(|| format!("zona '{name}' sconosciuta"))
    }
}

impl MouseDriver for SteelSeries {
    fn spec(&self) -> &DeviceSpec {
        &self.spec
    }

    fn identify(&mut self) -> Result<DeviceInfo> {
        Ok(DeviceInfo {
            name: self.spec.name.clone(),
            vid: self.spec.vid,
            pid: self.spec.pid,
            status: self.spec.status,
            firmware: self.cfg.firmware.as_ref().and_then(|c| firmware(&self.dev, c)),
            serial: None,
        })
    }

    fn read_state(&mut self) -> Result<MouseState> {
        let mut s = cache::load(self.spec.vid, self.spec.pid);
        for zone in &self.spec.caps.zones {
            if !s.zones.iter().any(|z| &z.zone == zone) {
                let brightness = self.cfg.brightness.as_ref().map(|_| 255);
                s.zones.push(ZoneState { zone: zone.clone(), effect: None, brightness });
            }
        }
        if self.cfg.brightness.is_none() {
            s.zones.iter_mut().for_each(|z| z.brightness = None);
        }
        Ok(s)
    }

    fn set_dpi_stages(&mut self, stages: &[u16], active: usize) -> Result<()> {
        self.spec.caps.check_dpi_stages(stages, active)?;
        let dpi = self.cfg.dpi.clone().context("DPI non configurabili su questo modello")?;
        if dpi.mode == "presets" {
            for (c, &d) in dpi.cmds.iter().zip(stages) {
                send(&self.dev, c, &self.code(&dpi, d)?)?;
            }
        } else {
            let c = dpi.cmd.as_ref().context("comando DPI mancante")?;
            let count = match dpi.count_mode.as_str() {
                "flag" => 0xFFu8 >> (8 - stages.len()),
                _ => stages.len() as u8,
            };
            let mut payload = vec![count, active as u8 + dpi.first_preset];
            let codes: Vec<Vec<u8>> = stages.iter().map(|&d| self.code(&dpi, d)).collect::<Result<_>>()?;
            match dpi.xy.as_str() {
                "xyxy" => codes.iter().for_each(|c| {
                    payload.extend(c);
                    payload.extend(c);
                }),
                "xxyy" => {
                    let mut padded = codes.clone();
                    padded.resize(dpi.max_presets.max(codes.len()), vec![0; dpi.code_bytes]);
                    padded.iter().for_each(|c| payload.extend(c));
                    padded.iter().for_each(|c| payload.extend(c));
                }
                _ => codes.iter().for_each(|c| payload.extend(c)),
            }
            send(&self.dev, c, &payload)?;
        }
        let stages = stages.to_vec();
        self.finish(|s| s.dpi_stages = Some(DpiStages { stages, active }))
    }

    fn set_polling(&mut self, hz: u16) -> Result<()> {
        self.spec.caps.check_polling(hz)?;
        let p = self.cfg.polling.as_ref().context("polling non configurabile")?;
        let v = p.values.get(&hz.to_string()).context("polling non gestito")?;
        send(&self.dev, &p.cmd, v)?;
        self.finish(|s| s.polling_hz = Some(hz))
    }

    fn set_lighting(&mut self, zone: &str, effect: &Effect) -> Result<()> {
        self.spec.caps.check_effect(zone, effect)?;
        let z = self.zone(zone)?.clone();
        let color = match effect {
            Effect::Off => Rgb(0, 0, 0),
            e => e.color().unwrap_or(Rgb(255, 255, 255)),
        };
        // Effetto globale (steady/breath/rainbow) se il mouse lo prevede.
        let mode = match effect {
            Effect::Off | Effect::Static { .. } => Some("static"),
            Effect::Breathing { .. } => Some("breathing"),
            Effect::Spectrum => Some("spectrum"),
            _ => None,
        };
        if let (Some(e), Some(m)) = (&self.cfg.effect, mode) {
            if let Some(v) = e.values.get(m) {
                send(&self.dev, &e.cmd, v)?;
            }
        }
        let gradient_spectrum = matches!(effect, Effect::Spectrum) && z.kind == "rgbgradient";
        if !matches!(effect, Effect::Spectrum) || gradient_spectrum {
            let payload = match z.kind.as_str() {
                "rgbcolor" => vec![color.0, color.1, color.2],
                "rgbgradient" if gradient_spectrum => gradient_packet(
                    &z,
                    &[(0, Rgb(255, 0, 0)), (84, Rgb(0, 255, 0)), (168, Rgb(0, 0, 255)), (255, Rgb(255, 0, 0))],
                    self.cfg.spectrum_ms,
                )?,
                "rgbgradient" => gradient_packet(&z, &[(0, color)], 1000)?,
                "rgbgradientv2" => gradient_v2_static(&z, color)?,
                k => bail!("formato colore '{k}' non gestito"),
            };
            send(&self.dev, &z.cmd, &payload)?;
        }
        let (zone, effect) = (zone.to_string(), effect.clone());
        self.finish(move |s| match s.zones.iter_mut().find(|z| z.zone == zone) {
            Some(z) => z.effect = Some(effect),
            None => s.zones.push(ZoneState { zone, effect: Some(effect), brightness: None }),
        })
    }

    fn set_brightness(&mut self, _zone: &str, value: u8) -> Result<()> {
        let b = self.cfg.brightness.as_ref().context("questo mouse non ha un comando per la luminosità: usa un colore più scuro")?;
        let v = (value as u32 * b.max + 127) / 255;
        send(&self.dev, &b.cmd, &le(v, b.code_bytes))?;
        // La luminosità è unica per tutto il mouse.
        self.finish(|s| s.zones.iter_mut().for_each(|z| z.brightness = Some(value)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rival310_zone() -> Zone {
        let text = r#"
            zone = "logo"
            kind = "rgbgradient"
            cmd = { report = "feature", command = [0x5B, 0x00] }
            led_id = 0x00
            header = { header_length = 26, led_id_offsets = [0], duration_offset = 1, duration_length = 2, repeat_offset = 17, triggers_offset = 21, color_count_offset = 25 }
        "#;
        toml::from_str(text).unwrap()
    }

    // Byte attesi calcolati a mano dalle regole di rivalcfg (rgbgradient.process_value).
    #[test]
    fn static_color_matches_rivalcfg() {
        let p = gradient_packet(&rival310_zone(), &[(0, Rgb(0xff, 0x18, 0x00))], 1000).unwrap();
        assert_eq!(&p[..3], &[0x00, 0xE8, 0x03]);
        assert_eq!(p[17], 0x01);
        assert_eq!(p[25], 0x01);
        assert_eq!(&p[26..], &[0xff, 0x18, 0x00, 0xff, 0x18, 0x00, 0x00]);
    }

    #[test]
    fn gradient_matches_rivalcfg_default() {
        let stops = [(0, Rgb(255, 0, 0)), (84, Rgb(0, 255, 0)), (168, Rgb(0, 0, 255)), (255, Rgb(255, 0, 0))];
        let p = gradient_packet(&rival310_zone(), &stops, 1000).unwrap();
        assert_eq!(p[17], 0x00);
        assert_eq!(p[25], 4);
        assert_eq!(&p[26..], &[255, 0, 0, 255, 0, 0, 0, 0, 255, 0, 84, 0, 0, 255, 84, 255, 0, 0, 87]);
    }
}
