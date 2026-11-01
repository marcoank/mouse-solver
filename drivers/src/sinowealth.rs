//! Driver SinoWealth (Glorious Model O/D/O Eternal, G-Wolves, Machenike, Marvo e molti mouse
//! con chip SinoWealth). Protocollo reimplementato leggendo libratbag (src/driver-sinowealth.c,
//! licenza MIT): nessun codice copiato.
//!
//! Tutta la configurazione sta in un blocco letto e scritto con feature report 0x04 (o 0x06 sui
//! modelli "lunghi"), comandato dal report 0x05. Non c'è comando di salvataggio: scrivere il blocco
//! con il byte 3 = dimensione - 8 lo salva nel mouse.
//!
//! Rischio principale su Windows: la dimensione reale del blocco (123..167 byte). Se Windows non la
//! restituisce in lettura e il firmware non è nella tabella del TOML, il mouse resta in sola lettura.

use anyhow::{bail, Context, Result};
use hidapi::{HidApi, HidDevice};
use mousecfg_core::*;
use serde::Deserialize;

const CMD: u8 = 0x05;
const CMD_FIRMWARE: u8 = 0x01;
const CMD_GET_CONFIG: u8 = 0x11;
const SIZE_MIN: usize = 123;
const SIZE_MAX: usize = 167;

// Offset nel blocco di configurazione (byte 0 = report ID).
const SENSOR: usize = 9;
const RATE_FLAGS: usize = 10;
const COUNT_ACTIVE: usize = 11;
const DISABLED: usize = 12;
const DPIS: usize = 13;
const EFFECT: usize = 53;
const CYCLE_MODE: usize = 54;
const STATIC_MODE: usize = 56;
const STATIC_COLOR: usize = 57;
const BREATH_MODE: usize = 60;
const BREATH_COUNT: usize = 61;
const BREATH_COLOR: usize = 62;

const RATES: [(u8, u16); 4] = [(1, 125), (2, 250), (3, 500), (4, 1000)];

#[derive(Debug, Clone, Deserialize)]
pub struct Firmware {
    /// 4 caratteri restituiti dal comando firmware (es. "V103").
    pub version: String,
    pub name: String,
    /// Ordine dei colori: "rgb", "rbg" o "none" (nessun LED).
    pub led: String,
    /// Dimensione del blocco se nota (123, 131, 167...).
    #[serde(default)]
    pub config_size: Option<usize>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct SinoConfig {
    pub firmware: Vec<Firmware>,
}

pub struct SinoWealth {
    dev: HidDevice,
    spec: DeviceSpec,
    report: u8,
    report_len: usize,
    fw: Option<Firmware>,
    fw_version: String,
    config_size: Option<usize>,
}

fn debug(dir: &str, b: &[u8]) {
    if mousecfg_core::trace::on() {
        mousecfg_core::trace!("{dir} {}", b.iter().take(64).map(|x| format!("{x:02x}")).collect::<Vec<_>>().join(" "));
    }
}

/// Comando breve sul report 0x05 (solo firmware e "prepara la lettura della configurazione").
fn command(dev: &HidDevice, cmd: u8) -> Result<[u8; 6]> {
    if ![CMD_FIRMWARE, CMD_GET_CONFIG].contains(&cmd) {
        bail!(tf!("comando SinoWealth 0x{cmd:02x} bloccato", "SinoWealth command 0x{cmd:02x} blocked"));
    }
    let req = [CMD, cmd, 0, 0, 0, 0];
    debug(">", &req);
    dev.send_feature_report(&req)?;
    let mut buf = [0u8; 6];
    buf[0] = CMD;
    if cmd == CMD_FIRMWARE {
        dev.get_feature_report(&mut buf)?;
        debug("<", &buf);
        if buf[1] != cmd {
            bail!(t!("risposta per un altro comando", "reply to a different command"));
        }
    }
    Ok(buf)
}

impl SinoWealth {
    pub fn open(api: &HidApi, spec: &DeviceSpec, vid: u16, pid: u16) -> Result<SinoWealth> {
        let cfg: SinoConfig = spec.section("sinowealth")?;
        for info in api.device_list().filter(|d| d.vendor_id() == vid && d.product_id() == pid) {
            let Ok(dev) = info.open_device(api) else { continue };
            // La collezione giusta è quella che dichiara il feature report 0x04 (o 0x06).
            let (report, len) = match (crate::hid::feature_len(&dev, 0x04), crate::hid::feature_len(&dev, 0x06)) {
                (Some(l), _) => (0x04, l),
                (None, Some(l)) => (0x06, l),
                _ => continue,
            };
            let Ok(r) = command(&dev, CMD_FIRMWARE) else { continue };
            let version = String::from_utf8_lossy(&r[2..6]).to_string();
            let fw = cfg.firmware.iter().find(|f| f.version == version).cloned();
            let mut spec = spec.clone();
            (spec.vid, spec.pid) = (vid, pid);
            if let Some(f) = &fw {
                spec.name = f.name.clone();
            }
            let mut s = SinoWealth { dev, spec, report, report_len: len + 1, fw, fw_version: version, config_size: None };
            s.config_size = s.detect_size();
            s.fill_caps();
            return Ok(s);
        }
        bail!(tf!(
            "{} trovato ma la collezione di configurazione non risponde. Chiudi il software del produttore e riprova.",
            "{} found but its configuration interface does not respond. Close the manufacturer's software and try again.",
            spec.name
        ))
    }

    /// Dimensione reale del blocco: dalla lettura se Windows la restituisce, altrimenti dalla tabella.
    fn detect_size(&self) -> Option<usize> {
        if let Ok((n, _)) = self.read_raw() {
            if (SIZE_MIN..=SIZE_MAX).contains(&n) {
                return Some(n);
            }
        }
        self.fw.as_ref().and_then(|f| f.config_size)
    }

    fn read_raw(&self) -> Result<(usize, Vec<u8>)> {
        command(&self.dev, CMD_GET_CONFIG)?;
        let mut buf = vec![0u8; self.report_len];
        buf[0] = self.report;
        let n = self.dev.get_feature_report(&mut buf)?;
        debug("<", &buf[..n.min(buf.len())]);
        if n < SIZE_MIN || buf[1] != CMD_GET_CONFIG {
            bail!(tf!("blocco di configurazione non valido ({n} byte)", "invalid configuration block ({n} bytes)"));
        }
        Ok((n, buf))
    }

    fn read(&self) -> Result<Vec<u8>> {
        self.read_raw().map(|(_, b)| b)
    }

    fn sensor_plus_one(cfg: &[u8]) -> bool {
        // PMW3360 (0x06) e PMW3327 (0x0e) codificano DPI/100 - 1, gli altri DPI/100.
        matches!(cfg[SENSOR], 0x06 | 0x0e)
    }

    fn fill_caps(&mut self) {
        let Ok(cfg) = self.read() else { return };
        let max = match cfg[SENSOR] {
            0x0e => 10200,
            0x08 => 7200,
            0x06 => 12000,
            0x0f => 16000,
            _ => 2000,
        };
        let no_leds = self.led_order().is_none() || cfg[EFFECT] == 0xFF;
        let caps = &mut self.spec.caps;
        caps.dpi_max = max;
        if no_leds {
            caps.zones.clear();
            caps.effects.clear();
        }
    }

    /// Ordine dei colori: Some(true) = RGB, Some(false) = RBG, None = nessun LED.
    fn led_order(&self) -> Option<bool> {
        match self.fw.as_ref().map(|f| f.led.as_str()) {
            Some("none") => None,
            Some("rgb") => Some(true),
            _ => Some(false), // libratbag usa RBG come ripiego
        }
    }

    fn decode(&self, cfg: &[u8]) -> MouseState {
        decode(cfg, self.led_order(), !self.spec.caps.zones.is_empty())
    }

    /// Legge, modifica, scrive e verifica rileggendo.
    fn modify(&mut self, change: impl FnOnce(&Self, &mut Vec<u8>) -> Result<()>) -> Result<()> {
        let size = self.config_size.with_context(|| {
            tf!(
                "firmware {} non ancora nella tabella: per sicurezza questo mouse è in sola lettura. \
                 Manda il report di mouse-solver-kit per aggiungerlo.",
                "firmware {} is not in the table yet: to be safe this mouse is read-only. \
                 Send the mouse-solver-kit report to get it added.",
                self.fw_version
            )
        })?;
        let mut cfg = self.read()?;
        change(self, &mut cfg)?;
        cfg[0] = self.report;
        cfg[1] = CMD_GET_CONFIG;
        cfg[3] = (size - 8) as u8;
        debug(">", &cfg);
        self.dev.send_feature_report(&cfg)?;
        std::thread::sleep(std::time::Duration::from_millis(50));
        let back = self.read()?;
        // Confronto solo la parte significativa del blocco (il byte 3 vale 0 in lettura).
        let same = |a: &[u8], b: &[u8]| a[4..size] == b[4..size] && a[2] == b[2];
        if !same(&back, &cfg) {
            bail!(t!("verifica fallita: il mouse non ha salvato il blocco di configurazione come inviato", "check failed: the mouse did not save the configuration block as sent"));
        }
        Ok(())
    }
}

/// Colore memorizzato: `rgb` = Some(true) ordine R G B, altrimenti R B G.
fn color_at(cfg: &[u8], o: usize, rgb: Option<bool>) -> Rgb {
    match rgb {
        Some(true) => Rgb(cfg[o], cfg[o + 1], cfg[o + 2]),
        _ => Rgb(cfg[o], cfg[o + 2], cfg[o + 1]),
    }
}

fn put_color(cfg: &mut [u8], o: usize, c: Rgb, rgb: Option<bool>) {
    let b = match rgb {
        Some(true) => [c.0, c.1, c.2],
        _ => [c.0, c.2, c.1],
    };
    cfg[o..o + 3].copy_from_slice(&b);
}

fn decode(cfg: &[u8], rgb: Option<bool>, has_leds: bool) -> MouseState {
    let plus = SinoWealth::sensor_plus_one(cfg);
    let raw_to_dpi = |r: u8| (r as u16 + plus as u16) * 100;
    let xy = cfg[RATE_FLAGS] & 0x80 != 0;
    let active_n = (cfg[COUNT_ACTIVE] >> 4) as usize;
    let mut stages = Vec::new();
    let mut active = 0;
    for slot in 0..8 {
        if cfg[DISABLED] & (1 << slot) != 0 {
            continue;
        }
        let raw = if xy { cfg[DPIS + slot * 2] } else { cfg[DPIS + slot] };
        stages.push(raw_to_dpi(raw));
        if stages.len() == active_n {
            active = stages.len() - 1;
        }
    }
    let polling = RATES.iter().find(|(c, _)| *c == cfg[RATE_FLAGS] & 0x0F).map(|(_, hz)| *hz);
    let mut zones = Vec::new();
    if has_leds {
        let bri = |o: usize| ((cfg[o] >> 4) as u16 * 64).min(255) as u8;
        let (effect, brightness) = match cfg[EFFECT] {
            0x00 => (Effect::Off, None),
            0x01 => (Effect::Spectrum, Some(bri(CYCLE_MODE))),
            0x02 => (Effect::Static { color: color_at(cfg, STATIC_COLOR, rgb) }, Some(bri(STATIC_MODE))),
            0x03 => (Effect::Breathing { color: color_at(cfg, BREATH_COLOR, rgb) }, Some(bri(BREATH_MODE))),
            c => (Effect::Unknown { code: c }, None),
        };
        zones.push(ZoneState { zone: "mouse".into(), effect: Some(effect), brightness });
    }
    MouseState {
        dpi: stages.get(active).copied(),
        dpi_stages: (!stages.is_empty()).then(|| DpiStages { stages, active }),
        polling_hz: polling,
        zones,
        cached: false,
    }
}

impl MouseDriver for SinoWealth {
    fn spec(&self) -> &DeviceSpec {
        &self.spec
    }

    fn identify(&mut self) -> Result<DeviceInfo> {
        Ok(DeviceInfo {
            name: self.spec.name.clone(),
            vid: self.spec.vid,
            pid: self.spec.pid,
            status: self.spec.status,
            firmware: Some(self.fw_version.clone()),
            serial: None,
        })
    }

    fn read_state(&mut self) -> Result<MouseState> {
        Ok(self.decode(&self.read()?))
    }

    fn set_dpi_stages(&mut self, stages: &[u16], active: usize) -> Result<()> {
        self.spec.caps.check_dpi_stages(stages, active)?;
        let stages = stages.to_vec();
        self.modify(|_, cfg| {
            let plus = Self::sensor_plus_one(cfg);
            cfg[RATE_FLAGS] &= !0x80; // X e Y uguali
            for (i, &d) in stages.iter().enumerate() {
                cfg[DPIS + i] = (d / 100 - plus as u16) as u8;
            }
            cfg[DISABLED] = !((1u16 << stages.len()) - 1) as u8;
            cfg[COUNT_ACTIVE] = ((active as u8 + 1) << 4) | stages.len() as u8;
            Ok(())
        })
    }

    fn set_polling(&mut self, hz: u16) -> Result<()> {
        self.spec.caps.check_polling(hz)?;
        let code = RATES.iter().find(|(_, h)| *h == hz).map(|(c, _)| *c).context(t!("polling non gestito", "polling rate not supported"))?;
        self.modify(|_, cfg| {
            cfg[RATE_FLAGS] = (cfg[RATE_FLAGS] & 0xF0) | code;
            Ok(())
        })
    }

    fn set_lighting(&mut self, zone: &str, effect: &Effect) -> Result<()> {
        self.spec.caps.check_effect(zone, effect)?;
        let effect = effect.clone();
        self.modify(|s, cfg| {
            match effect {
                Effect::Off => cfg[EFFECT] = 0x00,
                Effect::Static { color } => {
                    cfg[EFFECT] = 0x02;
                    put_color(cfg, STATIC_COLOR, color, s.led_order());
                    if cfg[STATIC_MODE] >> 4 == 0 {
                        cfg[STATIC_MODE] |= 0x40; // luminosità massima se era 0
                    }
                }
                Effect::Breathing { color } => {
                    cfg[EFFECT] = 0x03;
                    cfg[BREATH_COUNT] = 1;
                    put_color(cfg, BREATH_COLOR, color, s.led_order());
                    if cfg[BREATH_MODE] & 0x0F == 0 {
                        cfg[BREATH_MODE] |= 0x02; // velocità media
                    }
                    if cfg[BREATH_MODE] >> 4 == 0 {
                        cfg[BREATH_MODE] |= 0x40;
                    }
                }
                Effect::Spectrum => {
                    cfg[EFFECT] = 0x01;
                    if cfg[CYCLE_MODE] & 0x0F == 0 {
                        cfg[CYCLE_MODE] |= 0x02;
                    }
                    if cfg[CYCLE_MODE] >> 4 == 0 {
                        cfg[CYCLE_MODE] |= 0x40;
                    }
                }
                ref e => bail!(tf!("effetto '{}' non gestito su questo mouse", "effect '{}' not supported on this mouse", e.name())),
            }
            Ok(())
        })
    }

    fn set_brightness(&mut self, _zone: &str, value: u8) -> Result<()> {
        let raw = ((value as u16 + 1) / 64) as u8; // 0..4
        self.modify(|_, cfg| {
            let o = match cfg[EFFECT] {
                0x01 => CYCLE_MODE,
                0x02 => STATIC_MODE,
                0x03 => BREATH_MODE,
                _ => bail!(t!("imposta prima un effetto (fisso, respiro o ciclo)", "set an effect first (static, breathing or cycle)")),
            };
            cfg[o] = (cfg[o] & 0x0F) | (raw << 4);
            Ok(())
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dpi_slots_decode() {
        // 3 stadi attivi (slot 0..2), attivo il secondo, sensore PMW3360 (DPI/100 - 1).
        let mut cfg = vec![0u8; 131];
        cfg[1] = CMD_GET_CONFIG;
        cfg[SENSOR] = 0x06;
        cfg[RATE_FLAGS] = 0x04;
        cfg[COUNT_ACTIVE] = (2 << 4) | 3;
        cfg[DISABLED] = 0xF8;
        cfg[DPIS..DPIS + 3].copy_from_slice(&[3, 7, 15]);
        cfg[EFFECT] = 0x02;
        cfg[STATIC_MODE] = 0x41;
        cfg[STATIC_COLOR..STATIC_COLOR + 3].copy_from_slice(&[0xff, 0x00, 0x80]); // R B G
        let st = decode(&cfg, Some(false), true);
        assert_eq!(st.dpi_stages.unwrap(), DpiStages { stages: vec![400, 800, 1600], active: 1 });
        assert_eq!(st.polling_hz, Some(1000));
        assert_eq!(st.zones[0].effect, Some(Effect::Static { color: Rgb(0xff, 0x80, 0x00) }));
        assert_eq!(st.zones[0].brightness, Some(255));
    }
}
