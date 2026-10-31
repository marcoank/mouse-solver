//! Driver Roccat (Kone XTD, Kone Pure, Kone EMP). Protocollo reimplementato leggendo libratbag
//! (src/driver-roccat*.c, licenza MIT): nessun codice copiato.
//!
//! Solo feature report: 0x04 seleziona cosa leggere e dice se il mouse è pronto, 0x05 è il profilo
//! attivo, 0x06 contiene DPI, polling e (EMP) luci del profilo. Scrivere 0x06 lo salva nel mouse.
//! Si modifica solo il report 0x06 del profilo attivo e si rilegge per verifica.

use anyhow::{bail, Context, Result};
use hidapi::{HidApi, HidDevice};
use mousecfg_core::*;
use serde::Deserialize;
use std::thread::sleep;
use std::time::Duration;

const SELECT: u8 = 0x04;
const PROFILE: u8 = 0x05;
const SETTINGS: u8 = 0x06;
const RATES: [u16; 4] = [125, 250, 500, 1000];

#[derive(Debug, Clone, Deserialize)]
pub struct RoccatConfig {
    /// Lunghezza del report 0x06 compreso l'ID (XTD 43, Pure 31, EMP 41).
    pub settings_len: usize,
    /// "xtd" (XTD e Pure) o "emp": cambia la posizione di stadio attivo e polling.
    pub layout: String,
    /// "sum" = somma u16 negli ultimi 2 byte; "auto" = ricalcolata solo se quella letta è valida (Pure).
    pub checksum: String,
    /// "div50" (DPI/50) o "minus100" ((DPI-100)/100).
    pub dpi_encoding: String,
}

pub struct Roccat {
    dev: HidDevice,
    spec: DeviceSpec,
    cfg: RoccatConfig,
}

struct Offsets {
    current: usize,
    yres: usize,
    rate: usize,
}

impl RoccatConfig {
    fn offsets(&self) -> Offsets {
        match self.layout.as_str() {
            "emp" => Offsets { current: 17, yres: 12, rate: 18 },
            _ => Offsets { current: 12, yres: 13, rate: 19 },
        }
    }

    fn dpi_to_raw(&self, d: u16) -> u8 {
        match self.dpi_encoding.as_str() {
            "minus100" => ((d - 100) / 100) as u8,
            _ => (d / 50) as u8,
        }
    }

    fn raw_to_dpi(&self, r: u8) -> u16 {
        match self.dpi_encoding.as_str() {
            "minus100" => r as u16 * 100 + 100,
            _ => r as u16 * 50,
        }
    }
}

fn sum(buf: &[u8]) -> u16 {
    buf.iter().fold(0u16, |a, &b| a.wrapping_add(b as u16))
}

fn checksum_ok(buf: &[u8]) -> bool {
    let n = buf.len();
    sum(&buf[..n - 2]) == u16::from_le_bytes([buf[n - 2], buf[n - 1]])
}

fn wait_ready(dev: &HidDevice) -> Result<()> {
    sleep(Duration::from_millis(10));
    for _ in 0..20 {
        let mut buf = [SELECT, 0, 0];
        dev.get_feature_report(&mut buf)?;
        match buf[1] {
            0x01 => return Ok(()),
            0x02 => bail!("il mouse ha rifiutato il comando"),
            0x03 => sleep(Duration::from_millis(100)),
            _ => sleep(Duration::from_millis(10)),
        }
    }
    bail!("il mouse non risponde (timeout)")
}

impl Roccat {
    pub fn open(api: &HidApi, spec: &DeviceSpec, vid: u16, pid: u16) -> Result<Roccat> {
        let cfg: RoccatConfig = spec.section("roccat")?;
        for info in api.device_list().filter(|d| d.vendor_id() == vid && d.product_id() == pid) {
            let Ok(dev) = info.open_device(api) else { continue };
            // La collezione giusta dichiara il report delle impostazioni.
            if crate::hid::feature_len(&dev, SETTINGS).is_none() {
                continue;
            }
            let mut spec = spec.clone();
            (spec.vid, spec.pid) = (vid, pid);
            let r = Roccat { dev, spec, cfg: cfg.clone() };
            if r.read().is_ok() {
                return Ok(r);
            }
        }
        bail!("{} trovato ma non risponde. Chiudi Roccat Swarm e riprova.", spec.name)
    }

    fn active_profile(&self) -> Result<u8> {
        let mut buf = [PROFILE, 0, 0];
        self.dev.get_feature_report(&mut buf)?;
        Ok(buf[2].min(4))
    }

    fn read(&self) -> Result<Vec<u8>> {
        let p = self.active_profile()?;
        self.dev.send_feature_report(&[SELECT, p, 0x80])?;
        wait_ready(&self.dev)?;
        let mut buf = vec![0u8; self.cfg.settings_len];
        buf[0] = SETTINGS;
        let n = self.dev.get_feature_report(&mut buf)?;
        if n < self.cfg.settings_len || buf[0] != SETTINGS {
            bail!("report impostazioni non valido ({n} byte)");
        }
        Ok(buf)
    }

    fn modify(&mut self, change: impl FnOnce(&RoccatConfig, &mut Vec<u8>) -> Result<()>) -> Result<()> {
        let mut buf = self.read()?;
        let had_checksum = checksum_ok(&buf);
        change(&self.cfg, &mut buf)?;
        if self.cfg.checksum == "sum" || had_checksum {
            let n = buf.len();
            let c = sum(&buf[..n - 2]);
            buf[n - 2..].copy_from_slice(&c.to_le_bytes());
        }
        self.dev.send_feature_report(&buf)?;
        wait_ready(&self.dev)?;
        if self.read()? != buf {
            bail!("verifica fallita: il mouse non ha salvato le impostazioni come inviate");
        }
        Ok(())
    }

    fn decode(&self, buf: &[u8]) -> MouseState {
        let o = self.cfg.offsets();
        let mut stages = Vec::new();
        let mut active = 0;
        for i in 0..5 {
            if buf[6] & (1 << i) == 0 {
                continue;
            }
            if i == buf[o.current] as usize {
                active = stages.len();
            }
            stages.push(self.cfg.raw_to_dpi(buf[7 + i]));
        }
        let mut zones = Vec::new();
        if self.cfg.layout == "emp" {
            let color = Rgb(buf[24], buf[25], buf[26]);
            let effect = if buf[19] & 0x0F == 0 {
                Effect::Off
            } else if buf[20] != 0 {
                Effect::Spectrum
            } else if buf[21] == 0x03 {
                Effect::Breathing { color }
            } else {
                Effect::Static { color }
            };
            zones.push(ZoneState { zone: "mouse".into(), effect: Some(effect), brightness: None });
        }
        MouseState {
            dpi: stages.get(active).copied(),
            dpi_stages: (!stages.is_empty()).then(|| DpiStages { stages, active }),
            polling_hz: RATES.get(buf[o.rate] as usize).copied(),
            zones,
            cached: false,
        }
    }
}

impl MouseDriver for Roccat {
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
        Ok(self.decode(&self.read()?))
    }

    fn set_dpi_stages(&mut self, stages: &[u16], active: usize) -> Result<()> {
        self.spec.caps.check_dpi_stages(stages, active)?;
        let stages = stages.to_vec();
        self.modify(|cfg, buf| {
            let o = cfg.offsets();
            for (i, &d) in stages.iter().enumerate() {
                buf[7 + i] = cfg.dpi_to_raw(d);
                buf[o.yres + i] = cfg.dpi_to_raw(d);
            }
            buf[6] = ((1u16 << stages.len()) - 1) as u8; // stadi oltre l'ultimo disattivati, DPI vecchi lasciati
            buf[o.current] = active as u8;
            Ok(())
        })
    }

    fn set_polling(&mut self, hz: u16) -> Result<()> {
        self.spec.caps.check_polling(hz)?;
        let code = RATES.iter().position(|&r| r == hz).context("polling non gestito")? as u8;
        self.modify(|cfg, buf| {
            buf[cfg.offsets().rate] = code;
            Ok(())
        })
    }

    fn set_lighting(&mut self, zone: &str, effect: &Effect) -> Result<()> {
        self.spec.caps.check_effect(zone, effect)?;
        if self.cfg.layout != "emp" {
            bail!("questo modello non ha luci configurabili");
        }
        let effect = effect.clone();
        self.modify(|_, buf| {
            let (status, flow, kind) = match effect {
                Effect::Off => (0xF0, 0, 0x01),
                Effect::Static { .. } => (0xFF, 0, 0x01),
                Effect::Breathing { .. } => (0xFF, 0, 0x03),
                Effect::Spectrum => (0xFF, 1, 0x01),
                ref e => bail!("effetto '{}' non gestito", e.name()),
            };
            buf[19] = status;
            buf[20] = flow;
            buf[21] = kind;
            if !(1..=3).contains(&buf[22]) {
                buf[22] = 2;
            }
            if let Some(c) = effect.color() {
                for led in 0..4 {
                    buf[23 + led * 4..27 + led * 4].copy_from_slice(&[0x1E, c.0, c.1, c.2]);
                }
            }
            Ok(())
        })
    }

    fn set_brightness(&mut self, _: &str, _: u8) -> Result<()> {
        bail!("questo mouse non ha un comando per la luminosità: usa un colore più scuro")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn checksum_is_le_sum() {
        let mut b = vec![0x06, 0x29, 0, 0, 6, 6, 0x1f, 1, 2, 3, 4, 5];
        let c = sum(&b);
        b.extend_from_slice(&c.to_le_bytes());
        assert!(checksum_ok(&b));
        let cfg = RoccatConfig { settings_len: 41, layout: "emp".into(), checksum: "sum".into(), dpi_encoding: "minus100".into() };
        assert_eq!(cfg.dpi_to_raw(1600), 15);
        assert_eq!(cfg.raw_to_dpi(15), 1600);
    }
}
