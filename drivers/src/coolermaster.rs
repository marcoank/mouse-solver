//! Driver Cooler Master (solo luci). Protocollo reimplementato leggendo OpenRGB
//! (GPL-2.0, Controllers/CoolerMasterController: CMMMController, CMMM711Controller,
//! CMMM712Controller e MM712protocol.txt di Frans Meulenbroeks): nessun codice copiato.
//!
//! Stadi DPI e polling non sono documentati da nessuna fonte aperta: non si toccano.
//! Tre famiglie, tutte con output report da 64 byte senza ID (65 con lo 0 iniziale) e una
//! risposta per ogni comando:
//! - "mm" (MM530, MM720, MM730): init 41 80, modo 51 28, colore 51 <cmd>, salva 50 55;
//! - "mm711" (MM711): stessi comandi ma prima il colore e poi il modo;
//! - "mm712" (MM712): parametri 4C 81 04, modo 4C 81 08, salva 54 81 01. OpenRGB invia il
//!   salvataggio senza lo 0 del report ID (errore): qui si usa 00 54 81 01 come nel documento.
//!
//! L'effetto vale per tutto il mouse (il pacchetto non ha un selettore di zona). Dopo ogni
//! scrittura si rileggono modo, colore e luminosità e si confrontano con quelli inviati.

use anyhow::{bail, Context, Result};
use hidapi::{HidApi, HidDevice};
use mousecfg_core::*;
use serde::Deserialize;

#[derive(Debug, Clone, Deserialize)]
pub struct CoolerMasterConfig {
    /// "mm" (MM530/MM720/MM730), "mm711" o "mm712".
    pub family: String,
    /// Famiglia "mm": byte di comando del colore, 0x2C (MM530) o 0x2B (MM720, MM730).
    #[serde(default)]
    pub command: Option<u8>,
    /// Interfaccia USB e usage page della collezione di controllo.
    pub interface: i32,
    pub usage_page: u16,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Family {
    /// MM530/MM720/MM730 con il loro byte di comando.
    Mm(u8),
    Mm711,
    Mm712,
}

impl CoolerMasterConfig {
    fn family(&self) -> Result<Family> {
        Ok(match self.family.as_str() {
            "mm" => match self.command {
                Some(c @ (0x2B | 0x2C)) => Family::Mm(c),
                _ => bail!(mousecfg_core::t!("famiglia mm: serve command = 0x2B o 0x2C", "mm family: command = 0x2B or 0x2C is required")),
            },
            "mm711" => Family::Mm711,
            "mm712" => Family::Mm712,
            f => bail!(mousecfg_core::tf!("famiglia Cooler Master '{f}' sconosciuta (mm, mm711, mm712)", "unknown Cooler Master family '{f}' (mm, mm711, mm712)")),
        })
    }
}

const ZONE: &str = "mouse";
const TIMEOUT_MS: i32 = 250;
/// Lo spettro accetta luminosità fino a 0x7F (OpenRGB, CM_MM_ARGB_BRIGHTNESS_MAX_SPECTRUM).
const SPECTRUM_MAX: u8 = 0x7F;

// Codici dei modi.
const MM_STATIC: u8 = 0x00;
const MM_BREATHING: u8 = 0x01;
const MM_SPECTRUM: u8 = 0x02;
const MM_OFF: u8 = 0xFE;
const MM712_OFF: u8 = 0x03;

/// Velocità "normale" degli effetti animati di ciascuna famiglia.
fn normal_speed(f: Family) -> u8 {
    match f {
        Family::Mm(_) => 0x31,
        Family::Mm711 => 0x38,
        Family::Mm712 => 0x02,
    }
}

fn off_code(f: Family) -> u8 {
    match f {
        Family::Mm712 => MM712_OFF,
        _ => MM_OFF,
    }
}

/// Codice del modo, colore e luminosità grezza da scrivere per un effetto (luminosità 0-255).
fn encode(f: Family, effect: &Effect, bri: u8) -> Result<(u8, Rgb, u8)> {
    Ok(match effect {
        Effect::Static { color } => (MM_STATIC, *color, bri),
        Effect::Breathing { color } => (MM_BREATHING, *color, bri),
        Effect::Spectrum => (MM_SPECTRUM, Rgb(0, 0, 0), ((bri as u16 * SPECTRUM_MAX as u16 + 127) / 255) as u8),
        Effect::Off => (off_code(f), Rgb(0, 0, 0), 0),
        e => bail!(mousecfg_core::tf!("effetto '{}' non supportato", "effect '{}' not supported", e.name())),
    })
}

/// Effetto e luminosità (0-255) da modo, colore e luminosità letti.
fn decode(f: Family, mode: u8, c: Rgb, bri: u8) -> (Effect, Option<u8>) {
    match mode {
        MM_STATIC => (Effect::Static { color: c }, Some(bri)),
        MM_BREATHING => (Effect::Breathing { color: c }, Some(bri)),
        MM_SPECTRUM => (Effect::Spectrum, Some(((bri.min(SPECTRUM_MAX) as u16 * 255 + 63) / SPECTRUM_MAX as u16) as u8)),
        m if m == off_code(f) => (Effect::Off, None),
        code => (Effect::Unknown { code }, None),
    }
}

/// Pacchetto da 65 byte: 0x00 (nessun report ID) + dati, il resto a zero.
fn packet(bytes: &[u8]) -> Vec<u8> {
    let mut p = vec![0u8];
    p.extend_from_slice(bytes);
    p.resize(65, 0);
    p
}

fn init_packet(f: Family) -> Vec<u8> {
    match f {
        Family::Mm712 => packet(&[0x44, 0x81, 0x02]),
        _ => packet(&[0x41, 0x80]),
    }
}

fn save_packet(f: Family) -> Vec<u8> {
    match f {
        Family::Mm712 => packet(&[0x54, 0x81, 0x01]),
        _ => packet(&[0x50, 0x55]),
    }
}

/// Famiglie mm/mm711: seleziona il modo.
fn apply_packet(mode: u8) -> Vec<u8> {
    packet(&[0x51, 0x28, 0x00, 0x00, mode])
}

/// Famiglie mm/mm711: colore, velocità e luminosità del modo.
fn colour_packet(f: Family, mode: u8, speed: u8, bri: u8, c: Rgb) -> Vec<u8> {
    let (cmd, nfi1) = match f {
        Family::Mm(cmd) => (cmd, if mode == MM_STATIC || mode == MM_SPECTRUM { 0x00 } else { 0x20 }),
        _ => (0x2B, 0x20),
    };
    let mut p = packet(&[0x51, cmd, 0x00, 0x00, mode, speed, nfi1, 0xFF, 0xFF, bri, c.0, c.1, c.2]);
    if let Family::Mm(_) = f {
        // byte 14-16 a zero (secondo colore), dal 17 in poi 0xFF come fa OpenRGB
        p[17..].fill(0xFF);
    }
    p
}

/// MM712: parametri del modo (non cambia il modo attivo).
fn mm712_params_packet(mode: u8, bri: u8, speed: u8, c: Rgb) -> Vec<u8> {
    packet(&[0x4C, 0x81, 0x04, mode, 0xFF, bri, speed, c.0, c.1, c.2, 0xFF])
}

fn mm712_mode_packet(mode: u8) -> Vec<u8> {
    packet(&[0x4C, 0x81, 0x08, mode])
}

/// Sequenza completa per scrivere un effetto e salvarlo nella memoria del mouse.
fn write_sequence(f: Family, mode: u8, c: Rgb, bri: u8) -> Vec<Vec<u8>> {
    let speed = normal_speed(f);
    let mut seq = Vec::new();
    match f {
        Family::Mm(_) => {
            seq.push(init_packet(f));
            seq.push(apply_packet(mode));
            // spento: solo init e modo (OpenRGB non manda il pacchetto colore)
            if mode != MM_OFF {
                seq.push(colour_packet(f, mode, speed, bri, c));
            }
        }
        Family::Mm711 => {
            seq.push(colour_packet(f, mode, speed, bri, c));
            seq.push(apply_packet(mode));
        }
        Family::Mm712 => {
            if mode != MM712_OFF {
                seq.push(mm712_params_packet(mode, bri, speed, c));
            }
            seq.push(mm712_mode_packet(mode));
        }
    }
    seq.push(save_packet(f));
    seq
}

/// Elenco dei soli pacchetti che il driver può inviare (con lo 0 iniziale).
fn allowed(f: Family, p: &[u8]) -> bool {
    if p.len() != 65 || p[0] != 0x00 {
        return false;
    }
    let mode_ok = |m: u8| matches!(m, MM_STATIC | MM_BREATHING | MM_SPECTRUM) || m == off_code(f);
    match (f, &p[1..]) {
        (Family::Mm(_) | Family::Mm711, [0x41, 0x80, ..]) => true,
        (Family::Mm(_) | Family::Mm711, [0x51, 0x28, 0x00, 0x00, m, ..]) => mode_ok(*m),
        (Family::Mm(cmd), [0x51, c, 0x00, 0x00, m, ..]) => c == &cmd && mode_ok(*m),
        (Family::Mm711, [0x51, 0x2B, 0x00, 0x00, m, ..]) => mode_ok(*m),
        (Family::Mm(_) | Family::Mm711, [0x50, 0x55, ..]) => true,
        // letture: modo e colore
        (Family::Mm(_) | Family::Mm711, [0x52, 0x28, ..]) => true,
        (Family::Mm(cmd), [0x52, c, ..]) => c == &cmd,
        (Family::Mm711, [0x52, 0x2B, ..]) => true,
        (Family::Mm712, [0x44, 0x81, 0x02, ..]) => true,
        (Family::Mm712, [0x4C, 0x81, 0x04 | 0x08, m, ..]) => mode_ok(*m),
        (Family::Mm712, [0x4C, 0x81, 0x07, ..]) => true,
        (Family::Mm712, [0x4C, 0x81, 0x03, m, ..]) => *m <= MM_SPECTRUM,
        (Family::Mm712, [0x54, 0x81, 0x01, ..]) => true,
        _ => false,
    }
}

fn debug() -> bool {
    mousecfg_core::trace::on()
}

/// Stato grezzo letto: modo, colore, luminosità.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Raw {
    mode: u8,
    color: Rgb,
    bri: u8,
}

pub struct CoolerMaster {
    dev: HidDevice,
    spec: DeviceSpec,
    family: Family,
}

/// Apre il mouse e restituisce il driver.
pub fn open(api: &HidApi, spec: &DeviceSpec, vid: u16, pid: u16) -> Result<Box<dyn MouseDriver>> {
    let cfg: CoolerMasterConfig = spec.section("coolermaster")?;
    let family = cfg.family()?;
    let mut spec = spec.clone();
    (spec.vid, spec.pid) = (vid, pid);
    // Solo la collezione di controllo: alle altre interfacce non si scrive mai.
    let candidates = api.device_list().filter(|d| {
        d.vendor_id() == vid && d.product_id() == pid && d.interface_number() == cfg.interface && d.usage_page() == cfg.usage_page
    });
    for info in candidates {
        let Ok(dev) = info.open_device(api) else { continue };
        let drv = CoolerMaster { dev, spec: spec.clone(), family };
        // Come OpenRGB: init in modalità normale, poi lettura dello stato.
        if drv.command(&init_packet(family)).is_ok() && drv.read_raw().is_ok() {
            return Ok(Box::new(drv));
        }
    }
    bail!(mousecfg_core::tf!("{} trovato ma non risponde. Chiudi MasterPlus+ e riprova.", "{} found but not responding. Close MasterPlus+ and try again.", spec.name))
}

/// Controlla che la sezione del driver nel file TOML sia valida (usato dai test del database).
pub fn check_config(spec: &DeviceSpec) -> Result<()> {
    let cfg: CoolerMasterConfig = spec.section("coolermaster")?;
    cfg.family()?;
    let c = &spec.caps;
    if c.max_stages != 0 || !c.polling.is_empty() {
        bail!(mousecfg_core::t!("su Cooler Master DPI e polling non sono documentati: max_stages = 0, polling = []", "on Cooler Master, DPI and polling are not documented: max_stages = 0, polling = []"));
    }
    if c.zones != [ZONE] {
        bail!(mousecfg_core::tf!("l'unica zona è \"{ZONE}\"", "the only zone is \"{ZONE}\""));
    }
    if let Some(e) = c.effects.iter().find(|e| !["static", "breathing", "spectrum", "off"].contains(&e.as_str())) {
        bail!(mousecfg_core::tf!("effetto '{e}' non gestito", "effect '{e}' not supported"));
    }
    Ok(())
}

impl CoolerMaster {
    fn write(&self, p: &[u8]) -> Result<()> {
        if !allowed(self.family, p) {
            bail!(mousecfg_core::tf!("comando {:02x?} non consentito: non viene inviato", "command {:02x?} not allowed: it will not be sent", &p[1..p.len().min(9)]));
        }
        if debug() {
            mousecfg_core::trace!("> {:02x?}", &p[1..17]);
        }
        self.dev.write(p).context(mousecfg_core::t!("scrittura sul mouse non riuscita", "writing to the mouse failed"))?;
        Ok(())
    }

    fn drain(&self) {
        let mut buf = [0u8; 65];
        while self.dev.read_timeout(&mut buf, 0).unwrap_or(0) > 0 {}
    }

    /// Invia un comando e legge la sua risposta (come OpenRGB, che non la interpreta).
    fn command(&self, p: &[u8]) -> Result<()> {
        self.drain();
        self.write(p)?;
        let mut buf = [0u8; 65];
        let n = self.dev.read_timeout(&mut buf, TIMEOUT_MS)?;
        if debug() && n > 0 {
            mousecfg_core::trace!("< {:02x?}", &buf[..n.min(16)]);
        }
        Ok(())
    }

    /// Invia una lettura e aspetta la risposta riconosciuta da `matches`.
    fn request(&self, p: &[u8], matches: impl Fn(&[u8]) -> bool) -> Result<Vec<u8>> {
        self.drain();
        self.write(p)?;
        let mut buf = [0u8; 65];
        for _ in 0..5 {
            let n = self.dev.read_timeout(&mut buf, TIMEOUT_MS)?;
            if n == 0 {
                break;
            }
            // le risposte arrivano senza lo 0 del report ID; se c'è, lo tolgo
            let r = if n == 65 && buf[0] == 0 { &buf[1..n] } else { &buf[..n] };
            if debug() {
                mousecfg_core::trace!("< {:02x?}", &r[..r.len().min(16)]);
            }
            if matches(r) {
                return Ok(r.to_vec());
            }
        }
        bail!(mousecfg_core::t!("il mouse non risponde alla lettura", "the mouse does not respond to the read"))
    }

    fn read_raw(&self) -> Result<Raw> {
        match self.family {
            Family::Mm(_) | Family::Mm711 => {
                let cmd = if let Family::Mm(c) = self.family { c } else { 0x2B };
                // risposte: [1] = sotto-comando, modo in [4], luminosità in [9], RGB in [10..13]
                let m = self.request(&packet(&[0x52, 0x28]), |r| r.len() >= 5 && r[1] == 0x28)?;
                let c = self.request(&packet(&[0x52, cmd]), |r| r.len() >= 13 && r[1] == cmd)?;
                Ok(Raw { mode: m[4], color: Rgb(c[10], c[11], c[12]), bri: c[9] })
            }
            Family::Mm712 => {
                // 4D 81 07 <modo>; 4D 81 03 ?? <lum> <vel> R G B
                let m = self.request(&packet(&[0x4C, 0x81, 0x07]), |r| r.len() >= 4 && r[..3] == [0x4D, 0x81, 0x07])?;
                let mode = m[3];
                if mode > MM_SPECTRUM {
                    return Ok(Raw { mode, color: Rgb(0, 0, 0), bri: 0 });
                }
                let c = self.request(&packet(&[0x4C, 0x81, 0x03, mode]), |r| r.len() >= 9 && r[..3] == [0x4D, 0x81, 0x03])?;
                Ok(Raw { mode, color: Rgb(c[6], c[7], c[8]), bri: c[4] })
            }
        }
    }

    fn state(&self) -> Result<(Effect, Option<u8>)> {
        let r = self.read_raw()?;
        Ok(decode(self.family, r.mode, r.color, r.bri))
    }

    /// Scrive, salva e rilegge per verifica.
    fn write_effect(&self, effect: &Effect, bri: u8) -> Result<()> {
        let f = self.family;
        let (mode, color, raw_bri) = encode(f, effect, bri)?;
        let seq = write_sequence(f, mode, color, raw_bri);
        // prima controllo tutta la sequenza: o parte intera o niente
        if let Some(p) = seq.iter().find(|p| !allowed(f, p)) {
            bail!(mousecfg_core::tf!("comando {:02x?} non consentito: non viene inviato", "command {:02x?} not allowed: it will not be sent", &p[1..9]));
        }
        for p in &seq {
            self.command(p)?;
        }
        let got = self.read_raw()?;
        let ok = got.mode == mode
            && match effect {
                Effect::Off => true,
                Effect::Spectrum => got.bri == raw_bri,
                _ => got.bri == raw_bri && got.color == color,
            };
        if !ok {
            bail!(mousecfg_core::t!("verifica fallita: il mouse non ha salvato le luci come inviate", "verification failed: the mouse did not save the lighting as sent"));
        }
        Ok(())
    }
}

impl MouseDriver for CoolerMaster {
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
        let (effect, brightness) = self.state()?;
        Ok(MouseState {
            zones: vec![ZoneState { zone: ZONE.into(), effect: Some(effect), brightness }],
            ..Default::default()
        })
    }

    fn set_dpi_stages(&mut self, _stages: &[u16], _active: usize) -> Result<()> {
        bail!(mousecfg_core::t!("su questo mouse i DPI non sono documentati: si possono cambiare solo le luci", "DPI is not documented on this mouse: only the lighting can be changed"))
    }

    fn set_polling(&mut self, _hz: u16) -> Result<()> {
        bail!(mousecfg_core::t!("su questo mouse il polling non è documentato: si possono cambiare solo le luci", "polling is not documented on this mouse: only the lighting can be changed"))
    }

    fn set_lighting(&mut self, zone: &str, effect: &Effect) -> Result<()> {
        self.spec.caps.check_effect(zone, effect)?;
        // tengo la luminosità attuale (piena se il mouse è spento o in un modo sconosciuto)
        let (_, bri) = self.state()?;
        self.write_effect(effect, bri.unwrap_or(255))
    }

    fn set_brightness(&mut self, zone: &str, value: u8) -> Result<()> {
        if zone != ZONE {
            bail!(mousecfg_core::tf!("zona '{zone}' non esiste", "zone '{zone}' does not exist"));
        }
        match self.state()?.0 {
            // a luci spente non c'è luminosità da cambiare
            Effect::Off => Ok(()),
            Effect::Unknown { code } => bail!(mousecfg_core::tf!("effetto attuale sconosciuto (0x{code:02x}): scegli prima un effetto", "current effect unknown (0x{code:02x}): choose an effect first")),
            e => self.write_effect(&e, value),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mm720_static_matches_openrgb_layout() {
        let f = Family::Mm(0x2B);
        let seq = write_sequence(f, MM_STATIC, Rgb(0x11, 0x22, 0x33), 0xFF);
        assert_eq!(seq.len(), 4);
        assert_eq!(&seq[0][..3], &[0x00, 0x41, 0x80]);
        assert_eq!(&seq[1][..6], &[0x00, 0x51, 0x28, 0x00, 0x00, 0x00]);
        assert_eq!(&seq[2][..17], &[0x00, 0x51, 0x2B, 0x00, 0x00, 0x00, 0x31, 0x00, 0xFF, 0xFF, 0xFF, 0x11, 0x22, 0x33, 0, 0, 0]);
        assert!(seq[2][17..].iter().all(|&b| b == 0xFF));
        assert_eq!(&seq[3][..4], &[0x00, 0x50, 0x55, 0x00]);
        assert!(seq.iter().all(|p| p.len() == 65 && allowed(f, p)));
    }

    #[test]
    fn mm530_breathing_and_off() {
        let f = Family::Mm(0x2C);
        let seq = write_sequence(f, MM_BREATHING, Rgb(1, 2, 3), 0x80);
        assert_eq!(&seq[2][1..8], &[0x51, 0x2C, 0x00, 0x00, 0x01, 0x31, 0x20]);
        // spento: init, modo 0xFE, salva
        let off = write_sequence(f, MM_OFF, Rgb(0, 0, 0), 0);
        assert_eq!(off.len(), 3);
        assert_eq!(&off[1][1..6], &[0x51, 0x28, 0x00, 0x00, 0xFE]);
        assert!(!allowed(f, &packet(&[0x51, 0x2B, 0x00, 0x00, 0x00])));
    }

    #[test]
    fn mm711_colour_then_apply() {
        let f = Family::Mm711;
        let seq = write_sequence(f, MM_SPECTRUM, Rgb(0, 0, 0), 0x7F);
        assert_eq!(seq.len(), 3);
        assert_eq!(&seq[0][1..14], &[0x51, 0x2B, 0x00, 0x00, 0x02, 0x38, 0x20, 0xFF, 0xFF, 0x7F, 0, 0, 0]);
        assert!(seq[0][14..].iter().all(|&b| b == 0)); // niente riempimento 0xFF qui
        assert_eq!(&seq[1][1..6], &[0x51, 0x28, 0x00, 0x00, 0x02]);
        assert_eq!(&seq[2][1..3], &[0x50, 0x55]);
    }

    #[test]
    fn mm712_matches_protocol_document() {
        let f = Family::Mm712;
        // MM712protocol.txt: 00 4c 81 04 00 ff ff 02 00 ff 00 (verde, fisso)
        let seq = write_sequence(f, MM_STATIC, Rgb(0x00, 0xFF, 0x00), 0xFF);
        assert_eq!(&seq[0][..12], &[0x00, 0x4C, 0x81, 0x04, 0x00, 0xFF, 0xFF, 0x02, 0x00, 0xFF, 0x00, 0xFF]);
        assert_eq!(&seq[1][..5], &[0x00, 0x4C, 0x81, 0x08, 0x00]);
        // salvataggio con lo 0 del report ID (OpenRGB lo omette per errore)
        assert_eq!(&seq[2][..5], &[0x00, 0x54, 0x81, 0x01, 0x00]);
        let off = write_sequence(f, MM712_OFF, Rgb(0, 0, 0), 0);
        assert_eq!(off.len(), 2);
        assert_eq!(&off[0][..5], &[0x00, 0x4C, 0x81, 0x08, 0x03]);
        assert!(seq.iter().chain(&off).all(|p| allowed(f, p)));
    }

    #[test]
    fn spectrum_brightness_is_capped() {
        assert_eq!(encode(Family::Mm711, &Effect::Spectrum, 255).unwrap().2, 0x7F);
        assert_eq!(decode(Family::Mm711, MM_SPECTRUM, Rgb(0, 0, 0), 0x7F).1, Some(255));
        assert_eq!(encode(Family::Mm(0x2B), &Effect::Static { color: Rgb(1, 1, 1) }, 200).unwrap(), (0, Rgb(1, 1, 1), 200));
        assert_eq!(decode(Family::Mm712, 0x03, Rgb(0, 0, 0), 0).0, Effect::Off);
        assert_eq!(decode(Family::Mm712, 0x04, Rgb(0, 0, 0), 0).0, Effect::Unknown { code: 4 });
        assert_eq!(decode(Family::Mm(0x2B), 0xFE, Rgb(0, 0, 0), 0).0, Effect::Off);
        assert!(encode(Family::Mm711, &Effect::Wave { reverse: false }, 255).is_err());
    }

    #[test]
    fn allowlist_refuses_everything_else() {
        for f in [Family::Mm(0x2B), Family::Mm711, Family::Mm712] {
            // modo personalizzato/diretto, profili, stato diretto, salvataggio errato di OpenRGB
            for p in [
                packet(&[0x51, 0xA8, 0x00, 0x00]),
                packet(&[0x51, 0x28, 0x00, 0x00, 0xB0]),
                packet(&[0x44, 0x81, 0x01, 0x02]),
                packet(&[0x5A, 0x81, 0x02]),
                packet(&[0x46, 0x81]),
                [0x54, 0x81, 0x01].iter().copied().chain(std::iter::repeat(0)).take(65).collect(),
                packet(&[0x4C, 0x81, 0x08, 0x04]),
            ] {
                assert!(!allowed(f, &p), "{f:?} {:02x?}", &p[..6]);
            }
        }
        assert!(!allowed(Family::Mm712, &packet(&[0x41, 0x80])));
        assert!(!allowed(Family::Mm(0x2B), &packet(&[0x54, 0x81, 0x01])));
        assert!(!allowed(Family::Mm(0x2B), &[0x00, 0x50, 0x55])); // lunghezza sbagliata
    }
}
