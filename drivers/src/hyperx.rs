//! Driver HyperX. Protocolli reimplementati leggendo (nessun codice copiato):
//! - Pulsefire Dart: santeri3700/hyperx_pulsefire_dart_reverse_engineering (CC0) e OpenRGB
//!   (GPL-2.0, HyperXPulsefireDartController) per luci e salvataggio;
//! - Pulsefire Haste Wireless: evan-razzaque/open-pulsefire-haste (GPL-3.0), documento e catture;
//! - Pulsefire Saga Pro: notwaterbtl/hyperx-saga-control (MIT).
//!
//! Due famiglie di protocollo:
//! - "NGenuity2" (Dart, Haste Wireless): output report da 64 byte senza ID, formato
//!   [comando, sotto-comando, indice, n. byte, dati...]. Il Dart non ha una rilettura documentata
//!   in modo affidabile (i campi dei report 0x52/0x53 sono incerti o in conflitto tra le fonti):
//!   è di sola scrittura (core::cache) e dopo ogni scrittura si controlla solo che risponda.
//!   L'Haste Wireless rilegge DPI (report 0x53) e polling (report 0x50 03); le sue luci no.
//! - "tabella 0x32" (Saga Pro): output report con l'ID nel primo byte, nessuna rilettura
//!   documentata: sola scrittura (core::cache).
//!
//! Si inviano solo i pacchetti ammessi da `allowed`: niente firmware, pulsanti, macro,
//! abbinamento o ripristino. Il resto viene rifiutato prima di arrivare al mouse.
//!
//! Esclusi di proposito:
//! - Haste 2 Core Wireless: la fonte dice che le impostazioni non vengono salvate nel mouse.
//! - Surge, FPS Pro, Core, Raid, Haste (cavo): esiste solo il colore "live", niente salvataggio.
//! - Saga Pro, polling 125/250/500/2000 Hz: i codici sono dedotti da una formula, non catturati.
//! - Saga Pro, respirazione/spettro: non decodificati dalla fonte.
//! - Haste Wireless, respirazione/spettro: la tabella di 120 colori e il byte di modo sono
//!   marcati "could be wrong" dalla fonte.

use anyhow::{bail, Context, Result};
use hidapi::{HidApi, HidDevice};
use mousecfg_core::driver::pct;
use mousecfg_core::*;
use serde::Deserialize;
use std::thread::sleep;
use std::time::Duration;

#[derive(Debug, Clone, Deserialize)]
pub struct HyperxConfig {
    /// "dart" (Pulsefire Dart), "haste" (Pulsefire Haste Wireless) o "saga" (Pulsefire Saga Pro).
    pub family: String,
    /// Interfaccia USB di configurazione.
    pub interface: i32,
    /// Usage page della collezione di configurazione (famiglie dart e haste).
    #[serde(default)]
    pub usage_page: Option<u16>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Family {
    Dart,
    Haste,
    Saga,
}

impl HyperxConfig {
    fn family(&self) -> Result<Family> {
        Ok(match self.family.as_str() {
            "dart" => Family::Dart,
            "haste" => Family::Haste,
            "saga" => Family::Saga,
            f => bail!("famiglia HyperX '{f}' sconosciuta (dart, haste, saga)"),
        })
    }
}

/// Polling -> codice. NGenuity2: 0..3. Saga Pro: solo i valori visti nelle catture.
fn polling_code(f: Family, hz: u16) -> Option<u8> {
    match f {
        Family::Dart | Family::Haste => match hz {
            125 => Some(0),
            250 => Some(1),
            500 => Some(2),
            1000 => Some(3),
            _ => None,
        },
        Family::Saga => match hz {
            1000 => Some(0x08),
            4000 => Some(0x02),
            _ => None,
        },
    }
}

fn polling_hz(f: Family, code: u8) -> Option<u16> {
    [125, 250, 500, 1000, 4000].into_iter().find(|&hz| polling_code(f, hz) == Some(code))
}

/// DPI -> valore scritto nel mouse. Dart: DPI/50. Haste Wireless: DPI/100. Saga Pro: DPI/50 - 1.
fn dpi_raw(f: Family, dpi: u16) -> u16 {
    match f {
        Family::Dart => dpi / 50,
        Family::Haste => dpi / 100,
        Family::Saga => dpi / 50 - 1,
    }
}

fn raw_dpi(f: Family, raw: u16) -> u16 {
    match f {
        Family::Dart => raw * 50,
        Family::Haste => raw * 100,
        Family::Saga => (raw + 1) * 50,
    }
}

/// Zone e effetti che ogni famiglia sa salvare nel mouse.
fn family_zones(f: Family) -> &'static [&'static str] {
    match f {
        Family::Dart => &["logo", "scroll"],
        Family::Haste => &["scroll"],
        Family::Saga => &["mouse"],
    }
}

fn family_effects(f: Family) -> &'static [&'static str] {
    match f {
        Family::Dart => &["static", "breathing", "spectrum", "off"],
        Family::Haste | Family::Saga => &["static", "off"],
    }
}

/// Elenco dei soli pacchetti che il driver può inviare (payload senza il byte 0 di report ID
/// per NGenuity2, con l'ID in testa per la Saga Pro).
fn allowed(f: Family, p: &[u8]) -> bool {
    use Family::*;
    match (f, p) {
        // polling
        (Dart | Haste, [0xD0, 0x00, 0x00, 0x01, code, ..]) => *code <= 3,
        // DPI: stadio attivo, stadi abilitati, valore di uno stadio
        (Dart | Haste, [0xD3, 0x00 | 0x01, 0x00, 0x01, ..]) => true,
        (Dart | Haste, [0xD3, 0x02, 0..=4, 0x02, ..]) => true,
        // richiesta informazioni hardware (sola lettura)
        (Dart | Haste, [0x50, 0x00, ..]) => true,
        // Dart: effetto luce di logo (0x00) o rotella (0x10): fisso, spettro, respirazione
        (Dart, [0xD2, 0x00 | 0x10, 0x00 | 0x12 | 0x20, 0x08, ..]) => true,
        (Dart, [0xDE, 0xFF, ..]) => true,
        // Haste Wireless: "store LED settings", tabella colori in modo fisso, modo LED, salvataggio
        (Haste, [0xD2, 0x00, 0x40, 0x08, ..]) => true,
        (Haste, [0xDA, 0x01, 0..=5, 0x3C, ..]) => true,
        (Haste, [0xD9, 0x00, 0x00, 0x03, 0x55, 0x01, 0x23, ..]) => true,
        (Haste, [0xDB, 0x55, ..]) => true,
        (Haste, [0xDE, 0xFF | 0x03, ..]) => true,
        // Haste Wireless: letture impostazioni (0x50 03) e DPI (0x53)
        (Haste, [0x50, 0x03, ..] | [0x53, 0x00, ..]) => true,
        // Saga Pro: tabella DPI/polling (live o profilo), colore, luminosità, sequenze catturate
        (Saga, [0x32, 0x01, 0x00 | 0x01, 0x00, ..]) => true,
        (Saga, [0x44, 0x01..=0x04, ..]) => true,
        (Saga, [0x40, 0x01, 0x00 | 0x01, 0x00, ..]) => true,
        (Saga, [0x42, 0x02, 0x01, 0x00, 0x06, 0x32, ..]) => true,
        (Saga, [0x20, 0x05, 0x01, 0x00, 0x01, ..]) => true,
        (Saga, [0x22, 0x07, 0x01, 0x00, 0x01, ..]) => true,
        (Saga, [0x50, 0x01, 0x14, 0x01, ..]) => true,
        (Saga, [0x36, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x02, ..]) => true,
        _ => false,
    }
}

// ---------- pacchetti NGenuity2 (Dart, Haste Wireless) ----------

fn polling_packet(code: u8) -> Vec<u8> {
    vec![0xD0, 0x00, 0x00, 0x01, code]
}

/// Stadi abilitati: bit n = stadio n (il bit meno significativo è il primo stadio, come nella
/// tabella di santeri3700 e nella cattura dell'Haste: 5 stadi = 0x1F).
fn dpi_mask_packet(count: usize) -> Vec<u8> {
    vec![0xD3, 0x01, 0x00, 0x01, ((1u16 << count) - 1) as u8]
}

fn dpi_value_packet(stage: u8, raw: u16) -> Vec<u8> {
    let [lo, hi] = raw.to_le_bytes();
    vec![0xD3, 0x02, stage, 0x02, lo, hi]
}

fn dpi_active_packet(stage: u8) -> Vec<u8> {
    vec![0xD3, 0x00, 0x00, 0x01, stage]
}

const SAVE_ALL: [u8; 2] = [0xDE, 0xFF];
const SAVE_DPI: [u8; 2] = [0xDE, 0x03];

/// Effetto salvato del Dart per una zona (0x00 logo, 0x10 rotella). `bri` 0-100.
fn dart_led_packet(led: u8, effect: &Effect, bri: u8) -> Result<Vec<u8>> {
    const SPEED_MED: u8 = 0x32;
    let (mode, c, speed) = match effect {
        Effect::Static { color } => (0x00, *color, 0x00),
        Effect::Breathing { color } => (0x20, *color, SPEED_MED),
        Effect::Spectrum => (0x12, Rgb(255, 255, 255), SPEED_MED),
        Effect::Off => (0x00, Rgb(0, 0, 0), 0x00),
        e => bail!("effetto '{}' non supportato dal Dart", e.name()),
    };
    Ok(vec![0xD2, led, mode, 0x08, c.0, c.1, c.2, c.0, c.1, c.2, bri.min(100), speed])
}

/// Colore fisso salvato dell'Haste Wireless: stessa sequenza di NGenuity nelle catture
/// (store LED, 6 pacchetti di tabella colori in modo 0x01, modo LED, pacchetto 0xDB, salva).
/// Il colore va già moltiplicato per la luminosità.
fn haste_static_sequence(c: Rgb) -> Vec<Vec<u8>> {
    let mut seq = vec![vec![0xD2, 0x00, 0x40, 0x08, 0xFF, 0xFF, 0xFF, 0x00, 0x00, 0x00, 0x64, 0x00]];
    for k in 0..6u8 {
        let mut p = vec![0xDA, 0x01, k, 0x3C];
        if k == 0 {
            p.extend_from_slice(&[c.0, c.1, c.2]);
        }
        seq.push(p);
    }
    seq.push(vec![0xD9, 0x00, 0x00, 0x03, 0x55, 0x01, 0x23]);
    seq.push(vec![0xDB, 0x55]);
    seq.push(SAVE_ALL.to_vec());
    seq
}

/// Colore moltiplicato per la luminosità (0-100), come richiesto dalla tabella dell'Haste.
fn scale(c: Rgb, pct100: u8) -> Rgb {
    let f = |v: u8| (v as u16 * pct100.min(100) as u16 / 100) as u8;
    Rgb(f(c.0), f(c.1), f(c.2))
}

/// Report 0x53 dell'Haste Wireless: [4] stadio attivo, [5] stadi abilitati, [12..22] 5 valori u16.
fn decode_haste_dpi(r: &[u8]) -> Option<DpiStages> {
    if r.len() < 22 || r[0] != 0x53 {
        return None;
    }
    let mut stages = Vec::new();
    let mut active = 0;
    for i in 0..5 {
        if r[5] & (1 << i) == 0 {
            continue;
        }
        if i == r[4] as usize {
            active = stages.len();
        }
        stages.push(raw_dpi(Family::Haste, u16::from_le_bytes([r[12 + 2 * i], r[13 + 2 * i]])));
    }
    (!stages.is_empty()).then_some(DpiStages { stages, active })
}

// ---------- pacchetti Saga Pro ----------

/// Colori degli stadi DPI scritti nella tabella (quelli predefiniti della fonte).
const SAGA_STAGE_COLORS: [Rgb; 4] = [Rgb(0xFF, 0, 0), Rgb(0, 0, 0xFF), Rgb(0xFF, 0xFF, 0), Rgb(0, 0xFF, 0)];

/// Tabella 0x32: 32 01 PP 00 RR 0F AA + 4 x (codice lo, hi, R, G, B). PP 0 = live, 1 = profilo.
fn saga_dpi_table(stages: &[u16], active: usize, poll: u8, profile: bool) -> Vec<u8> {
    let mut p = vec![0x32, 0x01, profile as u8, 0x00, poll, 0x0F, active as u8];
    for (&d, c) in stages.iter().zip(SAGA_STAGE_COLORS) {
        let [lo, hi] = dpi_raw(Family::Saga, d).to_le_bytes();
        p.extend_from_slice(&[lo, hi, c.0, c.1, c.2]);
    }
    p
}

/// Polling: sequenza di NGenuity come documentata (live, tabella profilo, conferme).
fn saga_polling_sequence(stages: &[u16], active: usize, poll: u8) -> Vec<Vec<u8>> {
    vec![
        saga_dpi_table(stages, active, poll, false),
        vec![0x42, 0x02, 0x01, 0x00, 0x06, 0x32],
        saga_dpi_table(stages, active, poll, true),
        vec![0x20, 0x05, 0x01, 0x00, 0x01],
        vec![0x22, 0x07, 0x01, 0x00, 0x01],
        vec![0x50, 0x01, 0x14, 0x01],
        vec![0x36, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x02],
    ]
}

/// Colore fisso salvato: la sequenza "rgb-only" che la fonte ha provato con spegnimenti e
/// riaccensioni. `intensity` 0-255.
fn saga_rgb_sequence(c: Rgb, intensity: u8) -> Vec<Vec<u8>> {
    vec![
        vec![0x44, 0x01, 0x01, 0x00],
        vec![0x44, 0x02, 0x00, 0x00, c.0, c.1, c.2],
        vec![0x40, 0x01, 0x00, 0x00, intensity],
        vec![0x44, 0x03, 0x01, 0x01, 0x00, 0x01],
        vec![0x44, 0x04, 0x00, 0x00, c.0, c.1, c.2],
        vec![0x40, 0x01, 0x01, 0x00, intensity],
        vec![0x50, 0x01, 0x14, 0x01],
        vec![0x36, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x02],
    ]
}

/// ID degli output report dichiarati in un descrittore HID.
fn output_report_ids(desc: &[u8]) -> Vec<u8> {
    let mut id = 0u8;
    let mut ids = Vec::new();
    let mut i = 0;
    while i < desc.len() {
        let prefix = desc[i];
        let len = match prefix & 0x03 {
            3 => 4,
            n => n as usize,
        };
        match prefix & 0xFC {
            0x84 => id = *desc.get(i + 1).unwrap_or(&0),
            0x90 if !ids.contains(&id) => ids.push(id),
            _ => {}
        }
        i += 1 + len;
    }
    ids
}

fn debug() -> bool {
    std::env::var_os("MOUSECFG_DEBUG").is_some()
}

pub struct HyperX {
    dev: HidDevice,
    spec: DeviceSpec,
    family: Family,
}

/// Apre il mouse e restituisce il driver.
pub fn open(api: &HidApi, spec: &DeviceSpec, vid: u16, pid: u16) -> Result<Box<dyn MouseDriver>> {
    let cfg: HyperxConfig = spec.section("hyperx")?;
    let family = cfg.family()?;
    let mut spec = spec.clone();
    (spec.vid, spec.pid) = (vid, pid);
    // Solo le collezioni dell'interfaccia di configurazione: alle altre non si scrive mai.
    let candidates = api.device_list().filter(|d| {
        d.vendor_id() == vid
            && d.product_id() == pid
            && d.interface_number() == cfg.interface
            && cfg.usage_page.is_none_or(|u| d.usage_page() == u)
    });
    for info in candidates {
        let Ok(dev) = info.open_device(api) else { continue };
        let drv = HyperX { dev, spec: spec.clone(), family };
        let ok = match family {
            // La collezione giusta risponde alla richiesta di informazioni (sola lettura).
            Family::Dart | Family::Haste => drv.hardware_info().is_ok(),
            // La collezione giusta dichiara l'output report 0x32.
            Family::Saga => {
                let mut buf = [0u8; 4096];
                drv.dev.get_report_descriptor(&mut buf).is_ok_and(|n| output_report_ids(&buf[..n]).contains(&0x32))
            }
        };
        if ok {
            return Ok(Box::new(drv));
        }
    }
    bail!("{} trovato ma non risponde. Chiudi HyperX NGenuity e riprova.", spec.name)
}

/// Controlla che la sezione del driver nel file TOML sia valida (usato dai test del database).
pub fn check_config(spec: &DeviceSpec) -> Result<()> {
    let cfg: HyperxConfig = spec.section("hyperx")?;
    let f = cfg.family()?;
    let c = &spec.caps;
    if f != Family::Saga && cfg.usage_page.is_none() {
        bail!("manca usage_page");
    }
    let (unit, max, stages) = match f {
        Family::Dart => (50, 16000, 1..=5),
        Family::Haste => (100, 16000, 1..=5),
        Family::Saga => (50, 26000, 4..=4),
    };
    if c.dpi_min < unit || c.dpi_max > max || c.dpi_step % unit != 0 || c.dpi_min % unit != 0 {
        bail!("limiti DPI non validi per la famiglia {}", cfg.family);
    }
    if !stages.contains(&c.min_stages) || !stages.contains(&c.max_stages) {
        bail!("numero di stadi DPI non valido per la famiglia {}", cfg.family);
    }
    if let Some(hz) = c.polling.iter().find(|&&hz| polling_code(f, hz).is_none()) {
        bail!("polling {hz} Hz non gestito dalla famiglia {}", cfg.family);
    }
    if let Some(z) = c.zones.iter().find(|z| !family_zones(f).contains(&z.as_str())) {
        bail!("zona '{z}' non gestita dalla famiglia {}", cfg.family);
    }
    if let Some(e) = c.effects.iter().find(|e| !family_effects(f).contains(&e.as_str())) {
        bail!("effetto '{e}' non gestito dalla famiglia {}", cfg.family);
    }
    Ok(())
}

impl HyperX {
    fn send(&self, payload: &[u8]) -> Result<()> {
        if payload.is_empty() || !allowed(self.family, payload) {
            bail!("comando {:02x?} non consentito: non viene inviato", &payload[..payload.len().min(8)]);
        }
        let data = match self.family {
            // l'ID del report è il primo byte del pacchetto
            Family::Saga => {
                let mut d = payload.to_vec();
                d.resize(64, 0);
                d
            }
            // report senza ID: byte 0 = 0x00, poi 64 byte di dati
            _ => {
                let mut d = vec![0u8];
                d.extend_from_slice(payload);
                d.resize(65, 0);
                d
            }
        };
        if debug() {
            eprintln!("> {:02x?}", &payload);
        }
        self.dev.write(&data).context("scrittura sul mouse non riuscita")?;
        sleep(Duration::from_millis(if self.family == Family::Saga { 40 } else { 10 }));
        Ok(())
    }

    fn send_all(&self, packets: &[Vec<u8>]) -> Result<()> {
        // prima controllo tutto: o parte l'intera sequenza o niente
        if let Some(p) = packets.iter().find(|p| p.is_empty() || !allowed(self.family, p)) {
            bail!("comando {:02x?} non consentito: non viene inviato", &p[..p.len().min(8)]);
        }
        packets.iter().try_for_each(|p| self.send(p))
    }

    /// Invia una richiesta NGenuity2 e aspetta il report che inizia con `id`, `sub`.
    fn request(&self, payload: &[u8], id: u8, sub: u8) -> Result<Vec<u8>> {
        let mut buf = [0u8; 65];
        while self.dev.read_timeout(&mut buf, 0).unwrap_or(0) > 0 {}
        self.send(payload)?;
        for _ in 0..15 {
            let n = self.dev.read_timeout(&mut buf, 100)?;
            if n == 0 {
                continue;
            }
            // alcuni sistemi lasciano lo 0 del report ID in testa
            let r = if n == 65 && buf[0] == 0 { &buf[1..n] } else { &buf[..n] };
            if debug() {
                eprintln!("< {:02x?}", r);
            }
            if r.len() >= 2 && r[0] == id && r[1] == sub {
                return Ok(r.to_vec());
            }
        }
        bail!("il mouse non risponde (è acceso? NGenuity è chiuso?)")
    }

    fn hardware_info(&self) -> Result<Vec<u8>> {
        self.request(&[0x50, 0x00], 0x50, 0x00)
    }

    fn read_haste_dpi(&self) -> Result<Option<DpiStages>> {
        let r = self.request(&[0x53, 0x00], 0x53, 0x00)?;
        Ok(decode_haste_dpi(&r))
    }

    fn read_haste_polling(&self) -> Result<Option<u16>> {
        let r = self.request(&[0x50, 0x03], 0x50, 0x03)?;
        Ok(r.get(53).and_then(|&c| polling_hz(Family::Haste, c)))
    }

    /// Dopo una scrittura senza rilettura: controlla che il mouse risponda e aggiorna la cache.
    fn finish(&self, update: impl FnOnce(&mut MouseState)) -> Result<()> {
        if self.family != Family::Saga && self.hardware_info().is_err() {
            bail!("il mouse non risponde dopo la scrittura");
        }
        let mut s = cache::load(self.spec.vid, self.spec.pid);
        update(&mut s);
        cache::save(self.spec.vid, self.spec.pid, &s);
        Ok(())
    }

    fn cached_zone(&self, zone: &str) -> (Option<Effect>, u8) {
        let s = cache::load(self.spec.vid, self.spec.pid);
        let z = s.zones.iter().find(|z| z.zone == zone);
        (z.and_then(|z| z.effect.clone()), z.and_then(|z| z.brightness).unwrap_or(255))
    }

    /// Scrive l'effetto `effect` con luminosità `bri` (0-255) e aggiorna la cache.
    fn write_lighting(&self, zone: &str, effect: &Effect, bri: u8) -> Result<()> {
        match self.family {
            Family::Dart => {
                let led = match zone {
                    "logo" => 0x00,
                    "scroll" => 0x10,
                    z => bail!("zona '{z}' sconosciuta"),
                };
                self.send_all(&[dart_led_packet(led, effect, pct(bri))?, SAVE_ALL.to_vec()])?;
            }
            Family::Haste => {
                let c = match effect {
                    Effect::Static { color } => *color,
                    Effect::Off => Rgb(0, 0, 0),
                    e => bail!("effetto '{}' non supportato", e.name()),
                };
                self.send_all(&haste_static_sequence(scale(c, pct(bri))))?;
            }
            Family::Saga => {
                let c = match effect {
                    Effect::Static { color } => *color,
                    Effect::Off => Rgb(0, 0, 0),
                    e => bail!("effetto '{}' non supportato", e.name()),
                };
                self.send_all(&saga_rgb_sequence(c, bri))?;
            }
        }
        let (zone, effect) = (zone.to_string(), effect.clone());
        self.finish(move |s| match s.zones.iter_mut().find(|z| z.zone == zone) {
            Some(z) => {
                z.effect = Some(effect);
                z.brightness = Some(bri);
            }
            None => s.zones.push(ZoneState { zone, effect: Some(effect), brightness: Some(bri) }),
        })
    }
}

impl MouseDriver for HyperX {
    fn spec(&self) -> &DeviceSpec {
        &self.spec
    }

    fn identify(&mut self) -> Result<DeviceInfo> {
        // Report 0x50: [8..12] versione firmware, la cifra più significativa in [11].
        let firmware = match self.family {
            Family::Saga => None,
            _ => self.hardware_info().ok().filter(|r| r.len() >= 12).map(|r| format!("v{}.{}.{}.{}", r[11], r[10], r[9], r[8])),
        };
        Ok(DeviceInfo {
            name: self.spec.name.clone(),
            vid: self.spec.vid,
            pid: self.spec.pid,
            status: self.spec.status,
            firmware,
            serial: None,
        })
    }

    fn read_state(&mut self) -> Result<MouseState> {
        let mut s = cache::load(self.spec.vid, self.spec.pid);
        if self.family == Family::Haste {
            // DPI e polling si rileggono dal mouse; le luci restano quelle scritte da qui.
            s.dpi_stages = self.read_haste_dpi()?;
            s.polling_hz = self.read_haste_polling()?;
            s.cached = false;
        }
        s.dpi = s.dpi_stages.as_ref().and_then(|d| d.stages.get(d.active).copied());
        for zone in &self.spec.caps.zones {
            if !s.zones.iter().any(|z| &z.zone == zone) {
                s.zones.push(ZoneState { zone: zone.clone(), effect: None, brightness: Some(255) });
            }
        }
        s.zones.retain(|z| self.spec.caps.zones.contains(&z.zone));
        Ok(s)
    }

    fn set_dpi_stages(&mut self, stages: &[u16], active: usize) -> Result<()> {
        self.spec.caps.check_dpi_stages(stages, active)?;
        let f = self.family;
        if f == Family::Saga {
            // La tabella contiene anche il polling: riscrivo quello noto (1000 Hz se mai impostato).
            let s = cache::load(self.spec.vid, self.spec.pid);
            let poll = s.polling_hz.and_then(|hz| polling_code(f, hz)).unwrap_or(0x08);
            // La fonte salva i DPI con il solo pacchetto "profilo" (provato: resta dopo il riavvio).
            self.send_all(&[saga_dpi_table(stages, active, poll, true)])?;
        } else {
            let mut seq = vec![dpi_mask_packet(stages.len())];
            for (i, &d) in stages.iter().enumerate() {
                seq.push(dpi_value_packet(i as u8, dpi_raw(f, d)));
            }
            seq.push(dpi_active_packet(active as u8));
            // Dart: solo "DE FF" salva (santeri3700). Haste: "DE 03" come nella cattura di NGenuity.
            seq.push(if f == Family::Dart { SAVE_ALL.to_vec() } else { SAVE_DPI.to_vec() });
            self.send_all(&seq)?;
        }
        let want = DpiStages { stages: stages.to_vec(), active };
        if f == Family::Haste {
            sleep(Duration::from_millis(100));
            if self.read_haste_dpi()?.as_ref() != Some(&want) {
                bail!("verifica fallita: il mouse non ha salvato gli stadi DPI come inviati");
            }
            return Ok(());
        }
        self.finish(|s| s.dpi_stages = Some(want))
    }

    fn set_polling(&mut self, hz: u16) -> Result<()> {
        self.spec.caps.check_polling(hz)?;
        let f = self.family;
        let code = polling_code(f, hz).context("polling non gestito")?;
        if f == Family::Saga {
            let s = cache::load(self.spec.vid, self.spec.pid);
            let Some(d) = s.dpi_stages.filter(|d| d.stages.len() == 4) else {
                bail!("su questo mouse polling e DPI si scrivono insieme: imposta prima gli stadi DPI");
            };
            self.send_all(&saga_polling_sequence(&d.stages, d.active, code))?;
        } else {
            // Non è documentato se D0 da solo resta in memoria: salvo con "DE FF".
            self.send_all(&[polling_packet(code), SAVE_ALL.to_vec()])?;
        }
        if f == Family::Haste {
            sleep(Duration::from_millis(100));
            if self.read_haste_polling()? != Some(hz) {
                bail!("verifica fallita: il polling letto non è {hz} Hz");
            }
            return Ok(());
        }
        self.finish(|s| s.polling_hz = Some(hz))
    }

    fn set_lighting(&mut self, zone: &str, effect: &Effect) -> Result<()> {
        self.spec.caps.check_effect(zone, effect)?;
        let (_, bri) = self.cached_zone(zone);
        self.write_lighting(zone, effect, bri)
    }

    fn set_brightness(&mut self, zone: &str, value: u8) -> Result<()> {
        if !self.spec.caps.zones.iter().any(|z| z == zone) {
            bail!("zona '{zone}' non esiste");
        }
        // La luminosità viaggia nello stesso pacchetto dell'effetto: serve sapere quale riscrivere.
        let (effect, _) = self.cached_zone(zone);
        let effect = effect.context("scegli prima un effetto per questa zona")?;
        self.write_lighting(zone, &effect, value)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dart_dpi_matches_santeri_example() {
        // "16000 DPI / 50 STEP = 320 ... RESULT = 0x40 0x01"
        assert_eq!(dpi_value_packet(0, dpi_raw(Family::Dart, 16000)), vec![0xD3, 0x02, 0x00, 0x02, 0x40, 0x01]);
        assert_eq!(raw_dpi(Family::Dart, 320), 16000);
    }

    #[test]
    fn haste_dpi_matches_capture() {
        // cattura dpi_profiles_5_profiles: 500 dpi -> 05, 6400 dpi -> 40
        assert_eq!(dpi_value_packet(0, dpi_raw(Family::Haste, 500)), vec![0xD3, 0x02, 0x00, 0x02, 0x05, 0x00]);
        assert_eq!(dpi_value_packet(4, dpi_raw(Family::Haste, 6400)), vec![0xD3, 0x02, 0x04, 0x02, 0x40, 0x00]);
        assert_eq!(dpi_mask_packet(5), vec![0xD3, 0x01, 0x00, 0x01, 0x1F]);
        assert_eq!(dpi_mask_packet(2), vec![0xD3, 0x01, 0x00, 0x01, 0x03]);
        assert_eq!(dpi_active_packet(0), vec![0xD3, 0x00, 0x00, 0x01, 0x00]);
    }

    #[test]
    fn polling_matches_capture() {
        assert_eq!(polling_packet(polling_code(Family::Haste, 250).unwrap()), vec![0xD0, 0x00, 0x00, 0x01, 0x01]);
        assert_eq!(polling_code(Family::Dart, 1000), Some(3));
        assert_eq!(polling_code(Family::Saga, 1000), Some(0x08));
        assert_eq!(polling_code(Family::Saga, 4000), Some(0x02));
        assert_eq!(polling_code(Family::Saga, 500), None);
        assert_eq!(polling_hz(Family::Haste, 3), Some(1000));
    }

    #[test]
    fn dart_led_matches_openrgb_layout() {
        let p = dart_led_packet(0x00, &Effect::Static { color: Rgb(0xFF, 0x10, 0x20) }, 100).unwrap();
        assert_eq!(p, vec![0xD2, 0x00, 0x00, 0x08, 0xFF, 0x10, 0x20, 0xFF, 0x10, 0x20, 0x64, 0x00]);
        let p = dart_led_packet(0x10, &Effect::Breathing { color: Rgb(1, 2, 3) }, 50).unwrap();
        assert_eq!(&p[..4], &[0xD2, 0x10, 0x20, 0x08]);
        assert_eq!(&p[10..], &[50, 0x32]);
        assert_eq!(dart_led_packet(0x00, &Effect::Spectrum, 100).unwrap()[2], 0x12);
        assert!(dart_led_packet(0x00, &Effect::Wave { reverse: false }, 100).is_err());
    }

    #[test]
    fn haste_static_follows_capture_sequence() {
        let seq = haste_static_sequence(scale(Rgb(0xFF, 0x00, 0x80), 50));
        assert_eq!(seq.len(), 10);
        // primo pacchetto identico alla cattura 50_speed_red_breathing
        assert_eq!(seq[0], vec![0xD2, 0x00, 0x40, 0x08, 0xFF, 0xFF, 0xFF, 0x00, 0x00, 0x00, 0x64, 0x00]);
        assert_eq!(seq[1], vec![0xDA, 0x01, 0x00, 0x3C, 0x7F, 0x00, 0x40]);
        assert_eq!(seq[6], vec![0xDA, 0x01, 0x05, 0x3C]);
        // coda identica alla cattura red_fade_50_speed_led (modo 0x01)
        assert_eq!(seq[7], vec![0xD9, 0x00, 0x00, 0x03, 0x55, 0x01, 0x23]);
        assert_eq!(seq[8], vec![0xDB, 0x55]);
        assert_eq!(seq[9], vec![0xDE, 0xFF]);
        assert!(seq.iter().all(|p| allowed(Family::Haste, p)));
    }

    #[test]
    fn haste_dpi_report_decodes() {
        let mut r = vec![0u8; 64];
        r[0] = 0x53;
        r[3] = 0x21;
        r[4] = 1; // stadio attivo
        r[5] = 0x07; // tre stadi
        for (i, v) in [4u16, 8, 16].iter().enumerate() {
            r[12 + 2 * i..14 + 2 * i].copy_from_slice(&v.to_le_bytes());
        }
        assert_eq!(decode_haste_dpi(&r), Some(DpiStages { stages: vec![400, 800, 1600], active: 1 }));
        r[0] = 0x52;
        assert_eq!(decode_haste_dpi(&r), None);
    }

    #[test]
    fn saga_dpi_table_matches_source() {
        // protocol-notes.md: 400 -> 07, 800 -> 0f, 1600 -> 1f, 3200 -> 3f
        let p = saga_dpi_table(&[400, 800, 1600, 3200], 2, 0x08, true);
        assert_eq!(&p[..7], &[0x32, 0x01, 0x01, 0x00, 0x08, 0x0F, 0x02]);
        assert_eq!(&p[7..12], &[0x07, 0x00, 0xFF, 0x00, 0x00]);
        assert_eq!(&p[12..14], &[0x0F, 0x00]);
        assert_eq!(&p[17..19], &[0x1F, 0x00]);
        assert_eq!(&p[22..24], &[0x3F, 0x00]);
        assert_eq!(p.len(), 27);
        assert_eq!(dpi_raw(Family::Saga, 26000), 519);
        assert_eq!(saga_dpi_table(&[400, 800, 1600, 3200], 0, 0x08, false)[2], 0x00);
    }

    #[test]
    fn saga_sequences_are_allowed() {
        let rgb = saga_rgb_sequence(Rgb(1, 2, 3), 0xFF);
        assert_eq!(rgb.len(), 8);
        assert_eq!(rgb[1], vec![0x44, 0x02, 0x00, 0x00, 1, 2, 3]);
        assert_eq!(rgb[5], vec![0x40, 0x01, 0x01, 0x00, 0xFF]);
        assert!(rgb.iter().all(|p| allowed(Family::Saga, p)));
        let poll = saga_polling_sequence(&[400, 800, 1600, 3200], 0, 0x02);
        assert_eq!(poll.len(), 7);
        assert!(poll.iter().all(|p| allowed(Family::Saga, p)));
    }

    #[test]
    fn allowlist_refuses_everything_else() {
        // pulsanti, macro, soglia batteria, salvataggio sconosciuto, tabella LED animata
        for p in [&[0xD4, 0x00, 0x01, 0x02][..], &[0xD6, 0x02], &[0xD5, 0x02], &[0xD1, 0, 0, 1, 5], &[0xDE, 0x00], &[0xDA, 0x41, 0x00, 0x3C]] {
            assert!(!allowed(Family::Dart, p) && !allowed(Family::Haste, p), "{p:02x?}");
        }
        assert!(!allowed(Family::Dart, &[0xD0, 0, 0, 1, 4]));
        assert!(!allowed(Family::Haste, &[0xD2, 0x00, 0x00, 0x08])); // colore "live"
        assert!(!allowed(Family::Saga, &[0x36, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x03]));
        assert!(!allowed(Family::Saga, &[0x50, 0x02]));
        assert!(!allowed(Family::Saga, &[0xD0, 0, 0, 1, 3]));
        assert!(!allowed(Family::Dart, &[0x32, 0x01, 0x01, 0x00]));
    }

    #[test]
    fn output_ids_from_descriptor() {
        // collezione vendor con output report 0x32 (63 byte) e feature report 0x50
        let d = [
            0x06, 0x00, 0xFF, 0x09, 0x01, 0xA1, 0x01, 0x85, 0x32, 0x75, 0x08, 0x95, 0x3F, 0x91, 0x02, 0x85, 0x50, 0x95,
            0x3F, 0xB1, 0x02, 0xC0,
        ];
        assert_eq!(output_report_ids(&d), vec![0x32]);
    }
}
