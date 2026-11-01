//! Driver Corsair. Protocollo reimplementato leggendo ckb-next (src/daemon, GPL-2.0), le note
//! ckb-next/corsair-protocol (CC BY-SA 4.0) e OpenRGB (GPL-2.0) per il trasporto su Windows:
//! nessun codice copiato.
//!
//! Due famiglie, scelte dal TOML:
//! - "nxp" (M65, Sabre, Scimitar, Harpoon, Glaive, Katar, Ironclaw, Nightsword, Dark Core):
//!   pacchetti da 64 byte `07` (scrivi) / `0e` (leggi). Il byte 3 sceglie il valore "live" (00) o
//!   quello salvato nel mouse (01): scriviamo entrambi. Stadi DPI 1..5 (lo 0 è il cecchino, lasciato
//!   com'è), stadio attivo, colore fisso salvato per zona, polling. Dopo il cambio di polling il
//!   mouse si ricollega da solo.
//! - "bragi" (M55, Katar Pro, Dark Core Pro, M75...): solo il polling; stadi DPI e luci salvate non
//!   sono documentati da nessun progetto open source.

use anyhow::{bail, Context, Result};
use hidapi::{HidApi, HidDevice};
use mousecfg_core::*;
use serde::Deserialize;
use std::thread::sleep;
use std::time::Duration;

const LEN: usize = 64;

#[derive(Debug, Clone, Deserialize)]
pub struct CorsairConfig {
    /// "nxp" o "bragi"
    pub family: String,
    /// NXP: numero di zone con colore salvabile (0 = modelli "a file", luci non gestite).
    #[serde(default)]
    pub stored_zones: u8,
    /// Bragi: 0x08 col cavo, 0x09 per il mouse dietro al ricevitore.
    #[serde(default = "wired")]
    pub bragi_prefix: u8,
}

fn wired() -> u8 {
    0x08
}

const ZONE_NAMES: [&str; 4] = ["front", "logo", "wheel", "thumb"];

pub struct Corsair {
    dev: HidDevice,
    spec: DeviceSpec,
    cfg: CorsairConfig,
}

fn debug(dir: &str, b: &[u8]) {
    if mousecfg_core::trace::on() {
        mousecfg_core::trace!("{dir} {}", b.iter().take(16).map(|x| format!("{x:02x}")).collect::<Vec<_>>().join(" "));
    }
}

/// Comandi ammessi (primi byte del pacchetto).
fn allowed(cfg: &CorsairConfig, p: &[u8]) -> bool {
    match cfg.family.as_str() {
        "nxp" => matches!(
            p,
            [0x0e, 0x01, ..]                                    // identità/firmware/polling
            | [0x0e, 0x13, 0x02 | 0x05 | 0x10..=0x13 | 0xd0..=0xd5, ..] // letture DPI, maschera, colori
            | [0x07, 0x13, 0x02 | 0x05 | 0x10..=0x13 | 0xd0..=0xd5, ..] // scritture DPI, maschera, colori
            | [0x07, 0x0a, 0x00, 0x00, 1 | 2 | 4 | 8, ..]       // polling (valori validati)
            | [0x07, 0x04, 0x01, ..] // controllo hardware dei tasti DPI
        ),
        _ => matches!(p, [0x08 | 0x09, 0x02, 0x01, ..] | [0x08 | 0x09, 0x01, 0x01, 0x00, 1..=4, ..]),
    }
}

impl Corsair {
    pub fn open(api: &HidApi, spec: &DeviceSpec, vid: u16, pid: u16) -> Result<Corsair> {
        let cfg: CorsairConfig = spec.section("corsair")?;
        let page = if cfg.family == "nxp" { 0xFFC2 } else { 0xFF42 };
        let probe: Vec<u8> = match cfg.family.as_str() {
            "nxp" => vec![0x0e, 0x01],
            _ => vec![cfg.bragi_prefix, 0x02, 0x01, 0x00],
        };
        let dev = crate::open_first(api, vid, pid, |d| d.interface_number() == 1 && d.usage_page() == page, |dev| {
            transact(dev, &cfg, &probe).is_ok_and(|r| cfg.family == "nxp" || r[2] == 0)
        })
        .with_context(|| tf!("{} trovato ma non risponde. Chiudi iCUE e riprova.", "{} found but not responding. Close iCUE and try again.", spec.name))?;
        let mut spec = spec.clone();
        (spec.vid, spec.pid) = (vid, pid);
        Ok(Corsair { dev, spec, cfg })
    }

    fn tx(&self, payload: &[u8]) -> Result<[u8; LEN]> {
        transact(&self.dev, &self.cfg, payload)
    }

    fn bragi_get(&self, prop: u8) -> Result<u32> {
        let r = self.tx(&[self.cfg.bragi_prefix, 0x02, prop, 0x00])?;
        if r[2] != 0 {
            bail!(tf!("il mouse risponde errore {}", "the mouse replies with error {}", r[2]));
        }
        Ok(u32::from_le_bytes([r[3], r[4], r[5], r[6]]))
    }

    // ------------------------------------------------------------ NXP

    fn get(&self, sub: u8) -> Result<[u8; LEN]> {
        self.tx(&[0x0e, 0x13, sub, 0x01])
    }

    fn stage_dpi(&self, n: u8) -> Result<(u16, [u8; 3])> {
        let r = self.get(0xd0 | n)?;
        Ok((u16::from_le_bytes([r[5], r[6]]), [r[9], r[10], r[11]]))
    }

    fn nxp_state(&self) -> Result<MouseState> {
        let mask = self.get(0x05)?[4];
        let current = self.get(0x02)?[4];
        let mut stages = Vec::new();
        let mut active = 0;
        for n in 1..=5u8 {
            if mask & (1 << n) == 0 {
                continue;
            }
            if n == current {
                active = stages.len();
            }
            stages.push(self.stage_dpi(n)?.0);
        }
        let rate = self.tx(&[0x0e, 0x01])?[16];
        let zones = (0..self.cfg.stored_zones)
            .filter_map(|z| {
                let r = self.get(0x10 + z).ok()?;
                let c = Rgb(r[4], r[5], r[6]);
                let effect = if c == Rgb(0, 0, 0) { Effect::Off } else { Effect::Static { color: c } };
                Some(ZoneState { zone: ZONE_NAMES[z as usize].into(), effect: Some(effect), brightness: None })
            })
            .collect();
        Ok(MouseState {
            dpi: stages.get(active).copied(),
            dpi_stages: (!stages.is_empty()).then(|| DpiStages { stages, active }),
            polling_hz: [1u8, 2, 4, 8].contains(&rate).then(|| 1000 / rate as u16),
            zones,
            cached: false,
        })
    }
}

/// Invia un pacchetto e aspetta la risposta che ripete i primi byte (scartando gli eventi 01/03).
fn transact(dev: &HidDevice, cfg: &CorsairConfig, payload: &[u8]) -> Result<[u8; LEN]> {
    if !allowed(cfg, payload) {
        bail!(tf!(
            "comando Corsair {:02x?} bloccato: non è nella lista dei comandi sicuri",
            "Corsair command {:02x?} blocked: not in the list of safe commands",
            &payload[..payload.len().min(4)]
        ));
    }
    let mut pkt = [0u8; LEN + 1];
    pkt[1..1 + payload.len()].copy_from_slice(payload);
    debug(">", &pkt[1..]);
    let mut junk = [0u8; LEN + 1];
    while dev.read_timeout(&mut junk, 0).unwrap_or(0) > 0 {}
    dev.write(&pkt)?;
    sleep(Duration::from_millis(10)); // i mouse NXP si "ingolfano" senza pausa
    let echo = payload.len().min(if cfg.family == "nxp" { 4 } else { 2 });
    for _ in 0..20 {
        let mut buf = [0u8; LEN + 1];
        let n = dev.read_timeout(&mut buf, 300)?;
        if n == 0 {
            break;
        }
        // senza report ID Windows lo toglie: i dati partono da 0
        let r = if n > LEN { &buf[1..] } else { &buf[..LEN] };
        debug("<", r);
        if r[..echo] == payload[..echo] {
            let mut out = [0u8; LEN];
            out.copy_from_slice(&r[..LEN]);
            return Ok(out);
        }
    }
    let is_write = payload[0] == 0x07 || (payload[0] != 0x0e && payload[1] == 0x01);
    if is_write {
        return Ok([0u8; LEN]); // alcune scritture non hanno risposta: la verifica è la rilettura
    }
    bail!(t!("nessuna risposta dal mouse", "no response from the mouse"))
}

impl MouseDriver for Corsair {
    fn spec(&self) -> &DeviceSpec {
        &self.spec
    }

    fn identify(&mut self) -> Result<DeviceInfo> {
        let firmware = match self.cfg.family.as_str() {
            "nxp" => self.tx(&[0x0e, 0x01]).ok().map(|r| format!("v{:x}.{:02x}", r[9], r[8])),
            _ => None,
        };
        Ok(DeviceInfo { name: self.spec.name.clone(), vid: self.spec.vid, pid: self.spec.pid, status: self.spec.status, firmware, serial: None })
    }

    fn read_state(&mut self) -> Result<MouseState> {
        match self.cfg.family.as_str() {
            "nxp" => self.nxp_state(),
            _ => {
                let v = self.bragi_get(0x01)?;
                Ok(MouseState { polling_hz: [125, 250, 500, 1000].get((v as usize).wrapping_sub(1)).copied(), ..Default::default() })
            }
        }
    }

    fn set_dpi_stages(&mut self, stages: &[u16], active: usize) -> Result<()> {
        self.spec.caps.check_dpi_stages(stages, active)?;
        if self.cfg.family != "nxp" {
            bail!(t!("su questo modello gli stadi DPI salvati non sono documentati", "on this model the saved DPI stages are not documented"));
        }
        let old_mask = self.get(0x05)?[4];
        let mask = (old_mask & 0x01) | (((1u8 << stages.len()) - 1) << 1); // il cecchino resta com'era
        for (i, &d) in stages.iter().enumerate() {
            let n = i as u8 + 1;
            let (_, color) = self.stage_dpi(n)?; // il colore dell'indicatore DPI resta quello salvato
            let [xl, xh] = d.to_le_bytes();
            for target in [0x00, 0x01] {
                self.tx(&[0x07, 0x13, 0xd0 | n, target, 0x00, xl, xh, xl, xh, color[0], color[1], color[2]])?;
            }
        }
        for target in [0x00, 0x01] {
            self.tx(&[0x07, 0x13, 0x05, target, mask])?;
            self.tx(&[0x07, 0x13, 0x02, target, active as u8 + 1])?;
        }
        self.tx(&[0x07, 0x04, 0x01])?; // tasto DPI gestito dal mouse
        let back = self.nxp_state()?.dpi_stages.context(t!("rilettura fallita", "read-back failed"))?;
        if back.stages != stages || back.active != active {
            bail!(tf!("verifica fallita: il mouse riporta stadi {:?} attivo {}", "check failed: the mouse reports stages {:?} active {}", back.stages, back.active + 1));
        }
        Ok(())
    }

    fn set_polling(&mut self, hz: u16) -> Result<()> {
        self.spec.caps.check_polling(hz)?;
        if self.cfg.family == "nxp" {
            let ms = (1000 / hz) as u8;
            self.tx(&[0x07, 0x0a, 0x00, 0x00, ms])?;
            // Il mouse si scollega e ricollega: l'handle non vale più. La verifica arriva rileggendo.
            sleep(Duration::from_millis(2500));
            return Ok(());
        }
        let v = match hz {
            125 => 1,
            250 => 2,
            500 => 3,
            1000 => 4,
            _ => bail!(tf!("polling {hz} Hz non gestito", "polling {hz} Hz not supported")),
        };
        self.tx(&[self.cfg.bragi_prefix, 0x01, 0x01, 0x00, v, 0x00])?;
        if self.bragi_get(0x01)? != v as u32 {
            bail!(t!("verifica fallita: il mouse non ha accettato il polling", "check failed: the mouse did not accept the polling rate"));
        }
        Ok(())
    }

    fn set_lighting(&mut self, zone: &str, effect: &Effect) -> Result<()> {
        self.spec.caps.check_effect(zone, effect)?;
        let z = ZONE_NAMES.iter().position(|n| *n == zone).context(t!("zona sconosciuta", "unknown zone"))? as u8;
        let c = match *effect {
            Effect::Off => Rgb(0, 0, 0),
            Effect::Static { color } => color,
            ref e => bail!(tf!("su questi Corsair si può salvare solo un colore fisso, non '{}'", "on these Corsair mice only a static color can be saved, not '{}'", e.name())),
        };
        self.tx(&[0x07, 0x13, 0x10 + z, 0x01, c.0, c.1, c.2])?;
        let r = self.get(0x10 + z)?;
        if Rgb(r[4], r[5], r[6]) != c {
            bail!(tf!("verifica fallita su {zone}", "check failed on {zone}"));
        }
        Ok(())
    }

    fn set_brightness(&mut self, _: &str, _: u8) -> Result<()> {
        bail!(t!("questo mouse non ha un comando per la luminosità: usa un colore più scuro", "this mouse has no brightness command: use a darker color"))
    }
}
