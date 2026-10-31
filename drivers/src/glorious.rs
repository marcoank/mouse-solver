//! Driver Glorious per i mouse senza chip SinoWealth. Sotto lo stesso marchio ci sono tre protocolli
//! che non hanno niente in comune; il TOML sceglie quale con `family`:
//!
//! - "g2" (VID 093A: Model O 2 / I 2 / O 2 Mini / D 2 Wireless, chip Pixart). Feature report 0x03 da
//!   64 byte, SOLO SCRITTURA: il mouse non restituisce mai le impostazioni. Reimplementato leggendo
//!   danexpo/o2ctl, lccpianoman/glor e Krodity/glorious-i2 (licenza MIT): nessun codice copiato.
//! - "g3" (VID 3794: Model D3 / O3 Wireless). Feature report 0x03 da 64 byte; ogni comando ha una
//!   lettura (comando | 0x80), quindi ogni scrittura viene riletta. Protocollo ricavato dai fatti di
//!   Virel0/glorious-linux (GPL-3.0): nessun codice copiato.
//! - "gp" (VID 258A: serie PRO e prima generazione Model O/D Wireless). Feature report senza ID da 64
//!   byte, SOLO SCRITTURA (si leggono solo firmware e batteria). Mappa "classic" da korkje/mxw (MIT,
//!   provata sui Model O/D Wireless); mappa "core2" (O 2 / D 2 PRO 4K/8K) dalla cattura USB di
//!   OpenMouse mouse-protocol (AGPL, solo fatti) e da GloriousAutoPollingRate (MIT).
//!
//! Nessuna famiglia ha un comando di salvataggio: ogni scrittura finisce nella memoria del mouse.
//! G2 e GP scrivono in un profilo interno (1-3) e il profilo attivo non si può leggere: prima della
//! prima scrittura il driver attiva il profilo 1 e scrive sempre lì.
//! Per G2 e GP lo stato mostrato è quello dell'ultima scrittura (core::cache, cached = true).
//! Comandi ammessi: solo quelli elencati in ogni famiglia; tutto il resto (firmware, pairing, tasti,
//! macro, reset) viene rifiutato prima di arrivare al mouse.

use anyhow::{bail, ensure, Context, Result};
use hidapi::{HidApi, HidDevice};
use mousecfg_core::driver::{from_pct, pct};
use mousecfg_core::*;
use serde::Deserialize;
use std::thread::sleep;
use std::time::{Duration, Instant};

const ZONE: &str = "mouse";
const EFFECTS: [&str; 4] = ["static", "breathing", "spectrum", "off"];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Family {
    G2,
    G3,
    Gp,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Link {
    /// Cavo USB.
    Cable,
    /// Ricevitore 2.4 GHz.
    Dongle,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Map {
    /// Prima generazione Model O/D Wireless e (secondo Glorious CORE) i PRO 1K.
    Classic,
    /// O 2 PRO / D 2 PRO 4K/8K.
    Core2,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GloriousConfig {
    pub family: Family,
    pub link: Link,
    /// Solo famiglia "gp": quale mappa di registri usa il modello.
    #[serde(default)]
    pub map: Option<Map>,
    /// Voci "ricevitore": PID dello stesso mouse col cavo. Serve a condividere l'ultimo stato scritto
    /// e (G2) a riconoscere quando il cavo è collegato, perché allora il mouse ignora il ricevitore.
    #[serde(default)]
    pub cable_pid: Option<u16>,
}

/// Apre il mouse e restituisce il driver.
pub fn open(api: &HidApi, spec: &DeviceSpec, vid: u16, pid: u16) -> Result<Box<dyn MouseDriver>> {
    let cfg: GloriousConfig = spec.section("glorious")?;
    let mut spec = spec.clone();
    (spec.vid, spec.pid) = (vid, pid);
    Ok(match cfg.family {
        Family::G2 => Box::new(G2::open(api, spec, cfg)?),
        Family::G3 => Box::new(G3::open(api, spec, cfg)?),
        Family::Gp => Box::new(Gp::open(api, spec, cfg)?),
    })
}

/// Controlla che la sezione del driver nel file TOML sia valida (usato dai test del database).
pub fn check_config(spec: &DeviceSpec) -> Result<()> {
    let cfg: GloriousConfig = spec.section("glorious")?;
    let c = &spec.caps;
    let subset = |vals: &[u16], ok: &[u16], what: &str| -> Result<()> {
        match vals.iter().find(|v| !ok.contains(v)) {
            Some(v) => bail!("{what} {v} non gestito da questa famiglia"),
            None => Ok(()),
        }
    };
    ensure!(c.zones.iter().all(|z| z == ZONE), "l'unica zona possibile è '{ZONE}'");
    ensure!(c.effects.iter().all(|e| EFFECTS.contains(&e.as_str())), "effetti possibili: {EFFECTS:?}");
    ensure!(c.zones.is_empty() == c.effects.is_empty(), "zone ed effetti vanno insieme");
    ensure!(c.dpi_values.is_empty(), "dpi_values non serve");
    ensure!(cfg.cable_pid.is_none() || cfg.link == Link::Dongle, "cable_pid solo per le voci ricevitore");
    match cfg.family {
        Family::G2 => {
            ensure!(cfg.map.is_none(), "map vale solo per la famiglia gp");
            // Le fonti confermano solo 4-6 stadi.
            ensure!(c.min_stages >= 4 && c.max_stages <= 6, "G2: stadi da 4 a 6");
            ensure!(c.dpi_min >= 100 && c.dpi_max <= 26000 && c.dpi_step % 50 == 0, "G2: DPI 100-26000 a passi di 50");
            subset(&c.polling, &G2_RATES.map(|r| r.1), "polling")?;
        }
        Family::G3 => {
            ensure!(cfg.map.is_none(), "map vale solo per la famiglia gp");
            ensure!(cfg.cable_pid.is_none(), "G3: cable_pid non serve (le impostazioni si rileggono)");
            ensure!(c.min_stages >= 1 && c.max_stages as usize <= G3_SLOTS, "G3: al massimo 6 stadi");
            ensure!(c.dpi_min >= 50 && c.dpi_max <= 26000, "G3: DPI 50-26000");
            let rates: &[u16] = match cfg.link {
                Link::Cable => &G3_RATES,
                Link::Dongle => &G3_RATES[..G3_RATES.len() - 1],
            };
            subset(&c.polling, rates, "polling")?;
        }
        Family::Gp => {
            let map = cfg.map.context("GP: manca map = \"classic\" o \"core2\"")?;
            ensure!(c.dpi_min >= 100, "GP: DPI minimo 100");
            match map {
                Map::Classic => {
                    // mxw scrive sempre esattamente 4 stadi (lunghezza fissa 0x12).
                    ensure!(c.min_stages == 4 && c.max_stages == 4, "GP classic: esattamente 4 stadi");
                    ensure!(c.dpi_max <= 19000, "GP classic: DPI massimo 19000");
                    subset(&c.polling, &GP_CLASSIC_RATES.map(|r| r.1), "polling")?;
                }
                Map::Core2 => {
                    ensure!(c.zones.is_empty(), "GP core2: luci non verificate, nessuna zona");
                    ensure!(c.max_stages <= 6 && c.dpi_max <= 26000, "GP core2: al massimo 6 stadi, DPI 26000");
                    subset(&c.polling, &GP_CORE2_RATES.map(|r| r.0), "polling")?;
                }
            }
        }
    }
    Ok(())
}

fn debug(dir: &str, b: &[u8]) {
    if std::env::var_os("MOUSECFG_DEBUG").is_some() {
        eprintln!("{dir} {}", b.iter().map(|x| format!("{x:02x}")).collect::<Vec<_>>().join(" "));
    }
}

fn ms(n: u64) {
    sleep(Duration::from_millis(n));
}

/// Stato dalla cache (mouse di sola scrittura), con la zona sempre presente.
fn cached_state(vid: u16, pid: u16, caps: &Capabilities) -> MouseState {
    let mut s = cache::load(vid, pid);
    for zone in &caps.zones {
        if !s.zones.iter().any(|z| &z.zone == zone) {
            s.zones.push(ZoneState { zone: zone.clone(), effect: None, brightness: None });
        }
    }
    s.zones.retain(|z| caps.zones.contains(&z.zone));
    s.dpi = s.dpi_stages.as_ref().and_then(|d| d.stages.get(d.active).copied());
    s
}

fn zone_mut<'a>(s: &'a mut MouseState, zone: &str) -> &'a mut ZoneState {
    if let Some(i) = s.zones.iter().position(|z| z.zone == zone) {
        return &mut s.zones[i];
    }
    s.zones.push(ZoneState { zone: zone.to_string(), effect: None, brightness: None });
    s.zones.last_mut().unwrap()
}

fn check_zone(caps: &Capabilities, zone: &str) -> Result<()> {
    if !caps.zones.iter().any(|z| z == zone) {
        bail!("zona '{zone}' non esiste su questo mouse");
    }
    Ok(())
}

// ---------------------------------------------------------------------------------------------
// Famiglia G2 (Pixart, VID 093A)
// ---------------------------------------------------------------------------------------------

const G2_REPORT: u8 = 0x03;
const G2_LEN: usize = 64;
const G2_CMD_PROFILE: u8 = 0x01;
const G2_CMD_LIGHTING: u8 = 0x02;
const G2_CMD_PERF: u8 = 0x04;
/// Gli unici comandi inviabili: profilo, luci, prestazioni. Tasti (0x03), macro (0x05), luci in
/// standby (0x06), batteria (0x08) e firmware non servono e sono bloccati.
const G2_ALLOWED: [u8; 3] = [G2_CMD_PROFILE, G2_CMD_LIGHTING, G2_CMD_PERF];
const G2_ROUTE_DONGLE: u8 = 0xFB;
const G2_ROUTE_CABLE: u8 = 0xFA;
/// Profilo interno in cui scriviamo (il profilo attivo non si può leggere).
const G2_PROFILE: u8 = 1;
const G2_RATES: [(u8, u16); 4] = [(1, 125), (2, 250), (3, 500), (4, 1000)];
const G2_DEFAULT_HZ: u16 = 1000;
// Il blocco prestazioni va riscritto intero e il mouse non lo restituisce: per i campi che l'app non
// gestisce si scrivono i valori di fabbrica di Glorious CORE (lift-off 1 mm, debounce 2 ms, motion
// sync spento, colori degli stadi), gli stessi di o2ctl.
const G2_LOD: u8 = 1;
const G2_DEBOUNCE: u8 = 2;
const G2_MOTION_SYNC: u8 = 0;
const G2_STAGE_COLORS: [[u8; 3]; 6] =
    [[0xFF, 0xFF, 0x00], [0x00, 0x00, 0xFF], [0xFF, 0x00, 0x00], [0x00, 0xFF, 0x00], [0xFF, 0x00, 0xFF], [0x00, 0xFF, 0xFF]];
/// Velocità degli effetti: livello 12 su 20 (60%, valore di CORE).
const G2_SPEED: u8 = 12;
/// Pause (ms) prese dalle fonti: il firmware scarta in silenzio i pacchetti che arrivano troppo presto.
const G2_FRAGMENT_MS: u64 = 150;

type G2Packet = [u8; G2_LEN];

fn g2_packet(cmd: u8, route: u8) -> G2Packet {
    let mut p = [0u8; G2_LEN];
    (p[0], p[1], p[2]) = (G2_REPORT, cmd, route);
    p
}

fn g2_route(cable: bool) -> u8 {
    if cable {
        G2_ROUTE_CABLE
    } else {
        G2_ROUTE_DONGLE
    }
}

/// DPI -> valore scritto (DPI/50, u16 little endian).
fn g2_encode_dpi(dpi: u16) -> [u8; 2] {
    (dpi / 50).to_le_bytes()
}

/// Livello 1..20 dalla luminosità 0-255. Mai 0: secondo gloriousctl un byte a 0 fa tornare il mouse
/// al profilo salvato invece di spegnere i LED (per spegnere si usa l'effetto "off").
fn g2_level(v: u8) -> u8 {
    ((v as u16 * 20 + 127) / 255).max(1) as u8
}

fn g2_profile_packet(profile: u8, cable: bool) -> G2Packet {
    let mut p = g2_packet(G2_CMD_PROFILE, g2_route(cable));
    p[3] = profile;
    p
}

/// Pacchetti del blocco prestazioni (cmd 0x04): stadio attivo, numero stadi, polling e DPI.
fn g2_perf_packets(profile: u8, stages: &[u16], active: usize, hz: u16, cable: bool) -> Result<Vec<G2Packet>> {
    ensure!((4..=6).contains(&stages.len()) && active < stages.len(), "G2: servono da 4 a 6 stadi");
    let code = G2_RATES.iter().find(|r| r.1 == hz).with_context(|| format!("polling {hz} Hz non gestito"))?.0;
    let mut d = [0u8; 64];
    (d[0], d[1], d[2], d[3], d[4], d[5]) = (active as u8, stages.len() as u8, G2_LOD, G2_DEBOUNCE, code, G2_MOTION_SYNC);
    // Ricevitore: 5 byte per stadio da 6; cavo: 8 byte per stadio da 12 (3 byte a zero in più).
    let (offset, stride) = if cable { (12, 8) } else { (6, 5) };
    for (i, &dpi) in stages.iter().enumerate() {
        ensure!((100..=26000).contains(&dpi), "DPI {dpi} fuori dai limiti");
        let at = offset + stride * i;
        d[at..at + 2].copy_from_slice(&g2_encode_dpi(dpi));
        d[at + 2..at + 5].copy_from_slice(&G2_STAGE_COLORS[i]);
    }
    if cable {
        let mut p = g2_packet(G2_CMD_PERF, G2_ROUTE_CABLE);
        p[3] = profile;
        p[4..64].copy_from_slice(&d[0..60]);
        return Ok(vec![p]);
    }
    let chunks = [(0, 11), (11, 21), (21, 31), (31, 41)];
    Ok(chunks
        .iter()
        .enumerate()
        .map(|(i, &(a, b))| {
            let mut p = g2_packet(G2_CMD_PERF, G2_ROUTE_DONGLE);
            (p[3], p[4]) = (i as u8, profile);
            p[5..5 + b - a].copy_from_slice(&d[a..b]);
            p
        })
        .collect())
}

/// Codice effetto e colori usati. Spectrum = effetto "Glorious" (arcobaleno di fabbrica), senza colori
/// come in o2ctl (provato su hardware).
fn g2_effect(effect: &Effect) -> Result<(u8, Vec<Rgb>)> {
    Ok(match effect {
        Effect::Off => (0x00, vec![]),
        Effect::Spectrum => (0x01, vec![]),
        Effect::Static { color } => (0x04, vec![*color]),
        Effect::Breathing { color } => (0x05, vec![*color]),
        e => bail!("effetto '{}' non supportato", e.name()),
    })
}

/// Pacchetti del blocco luci (cmd 0x02). `level` = luminosità 1..20, scritta sia per il cavo sia per
/// la batteria.
fn g2_light_packets(profile: u8, effect: &Effect, level: u8, cable: bool) -> Result<Vec<G2Packet>> {
    let (id, colors) = g2_effect(effect)?;
    let mut d = [0u8; 64];
    (d[0], d[1], d[2], d[3], d[4]) = (G2_SPEED, level, colors.len() as u8, G2_SPEED, level);
    let offset = if cable { 11 } else { 5 };
    for (i, c) in colors.iter().enumerate() {
        d[offset + 3 * i..offset + 3 * i + 3].copy_from_slice(&[c.0, c.1, c.2]);
    }
    if cable {
        let mut p = g2_packet(G2_CMD_LIGHTING, G2_ROUTE_CABLE);
        (p[3], p[4]) = (profile, id);
        p[5..34].copy_from_slice(&d[0..29]);
        return Ok(vec![p]);
    }
    let chunks = [(0, 8), (8, 17), (17, 26)];
    Ok(chunks
        .iter()
        .enumerate()
        .map(|(i, &(a, b))| {
            let mut p = g2_packet(G2_CMD_LIGHTING, G2_ROUTE_DONGLE);
            (p[3], p[4], p[5]) = (i as u8, profile, id);
            p[6..6 + b - a].copy_from_slice(&d[a..b]);
            p
        })
        .collect())
}

pub struct G2 {
    dev: HidDevice,
    spec: DeviceSpec,
    cable: bool,
    cache_pid: u16,
    profile_ready: bool,
    last_write: Option<Instant>,
}

impl G2 {
    fn open(api: &HidApi, spec: DeviceSpec, cfg: GloriousConfig) -> Result<G2> {
        let (vid, pid) = (spec.vid, spec.pid);
        if let Some(cable_pid) = cfg.cable_pid {
            if api.device_list().any(|d| d.vendor_id() == vid && d.product_id() == cable_pid) {
                bail!("{}: il mouse è collegato anche col cavo e così ignora i comandi dal ricevitore. Usa la voce \"cavo\".", spec.name);
            }
        }
        // Interfaccia 1, collezione del produttore (0xFF00) che dichiara il feature report 0x03 da 63 byte.
        let mut cands: Vec<_> = api
            .device_list()
            .filter(|d| d.vendor_id() == vid && d.product_id() == pid && d.interface_number() == 1 && d.usage_page() == 0xFF00)
            .collect();
        cands.sort_by_key(|d| d.usage() != 0x0001);
        let dev = cands
            .iter()
            .filter_map(|d| d.open_device(api).ok())
            .find(|dev| crate::hid::feature_len(dev, G2_REPORT) == Some(G2_LEN - 1))
            .with_context(|| format!("{}: interfaccia di configurazione non trovata. Chiudi Glorious CORE e riprova.", spec.name))?;
        Ok(G2 {
            dev,
            cable: cfg.link == Link::Cable,
            cache_pid: cfg.cable_pid.unwrap_or(pid),
            spec,
            profile_ready: false,
            last_write: None,
        })
    }

    /// Aspetta che dall'ultima scrittura siano passati almeno `gap` ms (il ricevitore inoltra i
    /// pacchetti via radio dopo che la chiamata è tornata).
    fn quiet(&self, gap: u64) {
        if let Some(t) = self.last_write {
            let gap = Duration::from_millis(gap);
            if t.elapsed() < gap {
                sleep(gap - t.elapsed());
            }
        }
    }

    fn send(&mut self, p: &G2Packet, after_ms: u64) -> Result<()> {
        if p[0] != G2_REPORT || !G2_ALLOWED.contains(&p[1]) || p[2] != g2_route(self.cable) {
            bail!("comando Glorious G2 {:02x} {:02x} bloccato", p[1], p[2]);
        }
        debug(">", p);
        self.dev.send_feature_report(p).context("scrittura sul mouse non riuscita")?;
        self.last_write = Some(Instant::now());
        ms(after_ms);
        Ok(())
    }

    /// Scrive un blocco completo nel profilo 1, attivandolo prima della prima scrittura.
    fn write_block(&mut self, packets: &[G2Packet]) -> Result<()> {
        if !self.profile_ready {
            // Un cambio di profilo inviato subito dopo altre scritture viene perso: 1 s di pausa prima
            // (ricevitore) e 1 s dopo; col cavo bastano tempi più brevi (o2ctl, glor).
            self.quiet(if self.cable { 200 } else { 1000 });
            self.send(&g2_profile_packet(G2_PROFILE, self.cable), if self.cable { 550 } else { 1000 })?;
            self.profile_ready = true;
        }
        self.quiet(if self.cable { 50 } else { 300 });
        for p in packets {
            self.send(p, G2_FRAGMENT_MS)?;
        }
        Ok(())
    }

    fn state(&self) -> MouseState {
        cached_state(self.spec.vid, self.cache_pid, &self.spec.caps)
    }

    fn save(&self, s: &mut MouseState) {
        s.dpi = s.dpi_stages.as_ref().and_then(|d| d.stages.get(d.active).copied());
        cache::save(self.spec.vid, self.cache_pid, s);
    }
}

impl MouseDriver for G2 {
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
        Ok(self.state())
    }

    fn set_dpi_stages(&mut self, stages: &[u16], active: usize) -> Result<()> {
        self.spec.caps.check_dpi_stages(stages, active)?;
        let mut s = self.state();
        // Il polling sta nello stesso blocco: se non lo conosciamo scriviamo quello di fabbrica.
        let hz = s.polling_hz.unwrap_or(G2_DEFAULT_HZ);
        let packets = g2_perf_packets(G2_PROFILE, stages, active, hz, self.cable)?;
        // Nessuna rilettura possibile: il mouse non restituisce le impostazioni.
        self.write_block(&packets)?;
        s.dpi_stages = Some(DpiStages { stages: stages.to_vec(), active });
        s.polling_hz = Some(hz);
        self.save(&mut s);
        Ok(())
    }

    fn set_polling(&mut self, hz: u16) -> Result<()> {
        self.spec.caps.check_polling(hz)?;
        let mut s = self.state();
        let d = s
            .dpi_stages
            .clone()
            .context("questo mouse non permette di rileggere le impostazioni: imposta prima gli stadi DPI (il polling si scrive insieme a loro)")?;
        let packets = g2_perf_packets(G2_PROFILE, &d.stages, d.active, hz, self.cable)?;
        self.write_block(&packets)?;
        s.polling_hz = Some(hz);
        self.save(&mut s);
        Ok(())
    }

    fn set_lighting(&mut self, zone: &str, effect: &Effect) -> Result<()> {
        self.spec.caps.check_effect(zone, effect)?;
        let mut s = self.state();
        let brightness = zone_mut(&mut s, zone).brightness.unwrap_or(255);
        let packets = g2_light_packets(G2_PROFILE, effect, g2_level(brightness), self.cable)?;
        self.write_block(&packets)?;
        let z = zone_mut(&mut s, zone);
        (z.effect, z.brightness) = (Some(effect.clone()), Some(brightness));
        self.save(&mut s);
        Ok(())
    }

    fn set_brightness(&mut self, zone: &str, value: u8) -> Result<()> {
        check_zone(&self.spec.caps, zone)?;
        let mut s = self.state();
        let effect = match zone_mut(&mut s, zone).effect.clone() {
            Some(e) if !matches!(e, Effect::Unknown { .. }) => e,
            _ => bail!("questo mouse non permette di rileggere le luci: scegli prima un effetto (la luminosità si scrive insieme)"),
        };
        let packets = g2_light_packets(G2_PROFILE, &effect, g2_level(value), self.cable)?;
        self.write_block(&packets)?;
        zone_mut(&mut s, zone).brightness = Some(value);
        self.save(&mut s);
        Ok(())
    }
}

// ---------------------------------------------------------------------------------------------
// Famiglia G3 (VID 3794: Model D3 / O3 Wireless)
// ---------------------------------------------------------------------------------------------

const G3_REPORT: u8 = 0x03;
const G3_LEN: usize = 64;
/// Destinatario "mouse" (il ricevitore lo inoltra via radio). 0xAA (il ricevitore stesso) non si usa.
const G3_MOUSE: u8 = 0xBB;
const G3_POLL_CABLE: u8 = 0x0D;
const G3_POLL_WIRELESS: u8 = 0x0E;
const G3_DPI_STAGE: u8 = 0x0F;
const G3_DPI_ACTIVE: u8 = 0x10;
const G3_LIGHTING: u8 = 0x14;
const G3_GET: u8 = 0x80;
const G3_FIRMWARE: u8 = 0x80;
/// Scritture ammesse: solo polling, stadi DPI, stadio attivo e luci.
const G3_WRITES: [u8; 5] = [G3_POLL_CABLE, G3_POLL_WIRELESS, G3_DPI_STAGE, G3_DPI_ACTIVE, G3_LIGHTING];
/// Letture ammesse: firmware e le letture dei comandi qui sopra.
const G3_READS: [u8; 6] = [
    G3_FIRMWARE,
    G3_POLL_CABLE | G3_GET,
    G3_POLL_WIRELESS | G3_GET,
    G3_DPI_STAGE | G3_GET,
    G3_DPI_ACTIVE | G3_GET,
    G3_LIGHTING | G3_GET,
];
const G3_SLOTS: usize = 6;
/// Il 8000 Hz sul ricevitore non è verificato: le voci ricevitore si fermano a 4000.
const G3_RATES: [u16; 7] = [125, 250, 500, 1000, 2000, 4000, 8000];
/// Colori per gli stadi che erano spenti (nessun colore da conservare).
const G3_STAGE_COLORS: [[u8; 3]; 6] =
    [[0xFF, 0x00, 0x00], [0x00, 0xFF, 0x00], [0x00, 0x00, 0xFF], [0xFF, 0xFF, 0x00], [0xFF, 0x00, 0xFF], [0x00, 0xFF, 0xFF]];
// Codici effetto (Glorious CORE).
const G3_FX_OFF: u8 = 0;
const G3_FX_RAINBOW: u8 = 1;
const G3_FX_BREATHING_SINGLE: u8 = 2;
const G3_FX_SOLID: u8 = 5;

fn g3_frame(cmd: u8, payload: &[u8]) -> Result<[u8; G3_LEN]> {
    if !G3_WRITES.contains(&cmd) && !G3_READS.contains(&cmd) {
        bail!("comando Glorious G3 0x{cmd:02x} bloccato");
    }
    ensure!(payload.len() <= G3_LEN - 4, "dati troppo lunghi");
    let mut buf = [0u8; G3_LEN];
    buf[..4].copy_from_slice(&[G3_REPORT, G3_MOUSE, cmd, payload.len() as u8]);
    buf[4..4 + payload.len()].copy_from_slice(payload);
    Ok(buf)
}

/// Dati della risposta a `cmd`; `None` se la risposta non riporta il comando (mouse in standby: il
/// ricevitore risponde con un report vuoto).
fn g3_reply(buf: &[u8], cmd: u8) -> Option<Vec<u8>> {
    if buf.len() < 4 || buf[2] != cmd {
        return None;
    }
    let len = (buf[3] as usize).min(buf.len() - 4);
    let mut v = buf[4..4 + len].to_vec();
    v.resize(G3_LEN - 4, 0);
    Some(v)
}

/// Dati per il comando 0x0F: slot, DPI big endian, colore due volte. DPI 0 = slot spento.
fn g3_stage_payload(slot: u8, dpi: Option<u16>, rgb: [u8; 3]) -> [u8; 9] {
    let mut p = [0u8; 9];
    p[0] = slot;
    if let Some(d) = dpi {
        p[1..3].copy_from_slice(&d.to_be_bytes());
        p[3..6].copy_from_slice(&rgb);
        p[6..9].copy_from_slice(&rgb);
    }
    p
}

/// Stadi accesi (DPI != 0) in ordine e posizione dello slot attivo tra questi.
fn g3_stages(slots: &[u16], active_slot: usize) -> Option<DpiStages> {
    let stages: Vec<u16> = slots.iter().copied().filter(|&d| d != 0).collect();
    if stages.is_empty() {
        return None;
    }
    let active = match slots.get(active_slot) {
        Some(&d) if d != 0 => slots[..active_slot].iter().filter(|&&d| d != 0).count(),
        _ => 0,
    };
    Some(DpiStages { stages, active })
}

/// Effetto dai dati letti con 0x94: [effetto, lum. cavo, lum. wireless, velocità BE16, 0x08, colore singolo, R, G, B].
fn g3_decode_effect(v: &[u8]) -> Effect {
    let color = Rgb(v[7], v[8], v[9]);
    match (v[0], v[6]) {
        (G3_FX_OFF, _) => Effect::Off,
        (G3_FX_RAINBOW, _) => Effect::Spectrum,
        (G3_FX_SOLID, 1) => Effect::Static { color },
        (G3_FX_BREATHING_SINGLE, 1) => Effect::Breathing { color },
        (code, _) => Effect::Unknown { code },
    }
}

/// Dati per il comando 0x14 partendo da quelli letti: cambia solo effetto e colore, conserva luminosità
/// e velocità (minimo 10, come CORE). Il byte 5 vale sempre 0x08 in tutte le scritture note.
fn g3_light_payload(cur: &[u8], effect: &Effect) -> Result<[u8; 10]> {
    let mut p = [0u8; 10];
    p.copy_from_slice(&cur[..10]);
    let speed = u16::from_be_bytes([p[3], p[4]]).max(10);
    p[3..5].copy_from_slice(&speed.to_be_bytes());
    p[5] = 0x08;
    let (code, color) = match effect {
        Effect::Off => (G3_FX_OFF, None),
        Effect::Spectrum => (G3_FX_RAINBOW, None),
        Effect::Static { color } => (G3_FX_SOLID, Some(*color)),
        Effect::Breathing { color } => (G3_FX_BREATHING_SINGLE, Some(*color)),
        e => bail!("effetto '{}' non supportato", e.name()),
    };
    p[0] = code;
    p[6] = color.is_some() as u8;
    if let Some(c) = color {
        p[7..10].copy_from_slice(&[c.0, c.1, c.2]);
    }
    Ok(p)
}

pub struct G3 {
    dev: HidDevice,
    spec: DeviceSpec,
    wireless: bool,
}

const G3_ASLEEP: &str = "nessuna risposta dal mouse: probabilmente è in standby, muovilo e riprova";

fn g3_query(dev: &HidDevice, cmd: u8, payload: &[u8], wait_ms: u64) -> Result<Vec<u8>> {
    ensure!(cmd & G3_GET != 0, "0x{cmd:02x} non è una lettura");
    let req = g3_frame(cmd, payload)?;
    debug(">", &req);
    dev.send_feature_report(&req)?;
    ms(wait_ms);
    let mut buf = [0u8; G3_LEN];
    buf[0] = G3_REPORT;
    let n = dev.get_feature_report(&mut buf).context(G3_ASLEEP)?;
    debug("<", &buf[..n]);
    g3_reply(&buf[..n], cmd).context(G3_ASLEEP)
}

impl G3 {
    fn open(api: &HidApi, spec: DeviceSpec, cfg: GloriousConfig) -> Result<G3> {
        let (vid, pid) = (spec.vid, spec.pid);
        let dev = crate::open_first(
            api,
            vid,
            pid,
            |d| d.interface_number() == 0 && d.usage_page() == 0xFF00 && d.usage() == 0x0001,
            |dev| {
                crate::hid::feature_len(dev, G3_REPORT) == Some(G3_LEN - 1)
                    && g3_query(dev, G3_DPI_ACTIVE | G3_GET, &[], 100).is_ok()
            },
        )
        .with_context(|| format!("{} trovato ma non risponde: muovi il mouse per svegliarlo e chiudi Glorious CORE.", spec.name))?;
        Ok(G3 { dev, spec, wireless: cfg.link == Link::Dongle })
    }

    fn query(&self, cmd: u8, payload: &[u8]) -> Result<Vec<u8>> {
        g3_query(&self.dev, cmd, payload, 100)
    }

    fn send(&self, cmd: u8, payload: &[u8], after_ms: u64) -> Result<()> {
        ensure!(G3_WRITES.contains(&cmd), "comando Glorious G3 0x{cmd:02x} bloccato");
        let buf = g3_frame(cmd, payload)?;
        debug(">", &buf);
        self.dev.send_feature_report(&buf).context("scrittura sul mouse non riuscita")?;
        ms(after_ms);
        Ok(())
    }

    fn poll_cmd(&self) -> u8 {
        if self.wireless {
            G3_POLL_WIRELESS
        } else {
            G3_POLL_CABLE
        }
    }

    /// I 6 slot: (DPI, colore).
    fn slots(&self) -> Result<Vec<(u16, [u8; 3])>> {
        (0..G3_SLOTS as u8)
            .map(|i| {
                let v = self.query(G3_DPI_STAGE | G3_GET, &[i])?;
                Ok((u16::from_be_bytes([v[1], v[2]]), [v[3], v[4], v[5]]))
            })
            .collect()
    }

    fn active_slot(&self) -> Result<usize> {
        Ok(self.query(G3_DPI_ACTIVE | G3_GET, &[])?[0] as usize)
    }

    fn lighting(&self) -> Result<Vec<u8>> {
        self.query(G3_LIGHTING | G3_GET, &[])
    }

    fn brightness_of(&self, v: &[u8]) -> u8 {
        from_pct(if self.wireless { v[2] } else { v[1] })
    }
}

impl MouseDriver for G3 {
    fn spec(&self) -> &DeviceSpec {
        &self.spec
    }

    fn identify(&mut self) -> Result<DeviceInfo> {
        let firmware = self.query(G3_FIRMWARE, &[]).ok().map(|v| format!("{}.{}.{}", v[0], v[1], v[2]));
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
        let slots: Vec<u16> = self.slots()?.iter().map(|s| s.0).collect();
        let dpi_stages = g3_stages(&slots, self.active_slot()?);
        let v = self.query(self.poll_cmd() | G3_GET, &[])?;
        let polling_hz = Some(u16::from_be_bytes([v[0], v[1]])).filter(|hz| G3_RATES.contains(hz));
        let l = self.lighting()?;
        let zones = self
            .spec
            .caps
            .zones
            .iter()
            .map(|z| ZoneState { zone: z.clone(), effect: Some(g3_decode_effect(&l)), brightness: Some(self.brightness_of(&l)) })
            .collect();
        Ok(MouseState {
            dpi: dpi_stages.as_ref().map(|d| d.stages[d.active]),
            dpi_stages,
            polling_hz,
            zones,
            cached: false,
        })
    }

    fn set_dpi_stages(&mut self, stages: &[u16], active: usize) -> Result<()> {
        self.spec.caps.check_dpi_stages(stages, active)?;
        ensure!(stages.len() <= G3_SLOTS, "al massimo {G3_SLOTS} stadi");
        // Conservo i colori degli stadi esistenti.
        let old = self.slots()?;
        // Come CORE: prima lo stadio attivo, poi tutti e 6 gli slot (quelli in più spenti), 30 ms tra uno e l'altro.
        self.send(G3_DPI_ACTIVE, &[active as u8], 30)?;
        for i in 0..G3_SLOTS {
            let rgb = match old[i] {
                (d, c) if d != 0 && c != [0, 0, 0] => c,
                _ => G3_STAGE_COLORS[i],
            };
            self.send(G3_DPI_STAGE, &g3_stage_payload(i as u8, stages.get(i).copied(), rgb), 30)?;
        }
        let slots: Vec<u16> = self.slots()?.iter().map(|s| s.0).collect();
        let got = g3_stages(&slots, self.active_slot()?);
        let want = DpiStages { stages: stages.to_vec(), active };
        if got.as_ref() != Some(&want) {
            bail!("verifica fallita: il mouse riporta {got:?} invece di {want:?}");
        }
        Ok(())
    }

    fn set_polling(&mut self, hz: u16) -> Result<()> {
        self.spec.caps.check_polling(hz)?;
        self.send(self.poll_cmd(), &hz.to_be_bytes(), 200)?;
        let v = self.query(self.poll_cmd() | G3_GET, &[])?;
        let got = u16::from_be_bytes([v[0], v[1]]);
        if got != hz {
            bail!("verifica fallita: il mouse riporta {got} Hz invece di {hz}");
        }
        Ok(())
    }

    fn set_lighting(&mut self, zone: &str, effect: &Effect) -> Result<()> {
        self.spec.caps.check_effect(zone, effect)?;
        let cur = self.lighting()?;
        let p = g3_light_payload(&cur, effect)?;
        self.send(G3_LIGHTING, &p, 30)?;
        let got = self.lighting()?;
        let same = got[0] == p[0] && got[1..3] == p[1..3] && got[6] == p[6] && (p[6] == 0 || got[7..10] == p[7..10]);
        if !same {
            bail!("verifica fallita: luci rilette diverse da quelle scritte");
        }
        Ok(())
    }

    fn set_brightness(&mut self, zone: &str, value: u8) -> Result<()> {
        check_zone(&self.spec.caps, zone)?;
        let cur = self.lighting()?;
        let mut p = [0u8; 10];
        p.copy_from_slice(&cur[..10]);
        // Una sola luminosità nell'app: la scrivo sia per il cavo sia per il wireless (0-100%).
        (p[1], p[2]) = (pct(value), pct(value));
        let speed = u16::from_be_bytes([p[3], p[4]]).max(10);
        p[3..5].copy_from_slice(&speed.to_be_bytes());
        p[5] = 0x08;
        self.send(G3_LIGHTING, &p, 30)?;
        let got = self.lighting()?;
        if got[1..3] != p[1..3] {
            bail!("verifica fallita: luminosità riletta {}% invece di {}%", got[1], p[1]);
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------------------------
// Famiglia GP (VID 258A, busta "V2Pro": mappa classic e core2)
// ---------------------------------------------------------------------------------------------

const GP_LEN: usize = 65;
/// Destinatario "mouse"; 0x00 (il ricevitore) serve a CORE solo per il suo firmware e non si usa.
const GP_TARGET: u8 = 0x02;
const GP_PROFILE: u8 = 1;
// (banco, registro)
const GP_REG_PROFILE: (u8, u8) = (0, 0x05);
const GP_REG_FIRMWARE: (u8, u8) = (0, 0x81);
const GP_REG_POLL_CLASSIC: (u8, u8) = (1, 0x00);
const GP_REG_DPI: (u8, u8) = (1, 0x01);
const GP_REG_ACTIVE: (u8, u8) = (1, 0x02);
const GP_REG_POLL_CORE2: (u8, u8) = (1, 0x0A);
const GP_REG_LED_EFFECT: (u8, u8) = (2, 0x00);
const GP_REG_LED_BRIGHTNESS: (u8, u8) = (2, 0x02);
/// Scritture ammesse per mappa. Le luci della mappa core2 non sono mai state catturate: escluse.
const GP_CLASSIC_WRITES: [(u8, u8); 6] =
    [GP_REG_PROFILE, GP_REG_POLL_CLASSIC, GP_REG_DPI, GP_REG_ACTIVE, GP_REG_LED_EFFECT, GP_REG_LED_BRIGHTNESS];
const GP_CORE2_WRITES: [(u8, u8); 4] = [GP_REG_PROFILE, GP_REG_DPI, GP_REG_ACTIVE, GP_REG_POLL_CORE2];
/// classic: intervallo in ms.
const GP_CLASSIC_RATES: [(u8, u16); 4] = [(8, 125), (4, 250), (2, 500), (1, 1000)];
/// core2: (Hz, codice). Via radio il massimo è 4000: per 8000 CORE scrive 80 (cavo) e 40 (wireless).
const GP_CORE2_RATES: [(u16, u8); 7] = [(125, 0x08), (250, 0x04), (500, 0x02), (1000, 0x01), (2000, 0x20), (4000, 0x40), (8000, 0x80)];
const GP_CORE2_WIRELESS_MAX: u16 = 4000;
/// Velocità effetti classic: 40% come predefinito in mxw, invertita: (105 - 40) / 5.
const GP_RATE: u8 = (105 - 40) / 5;
const GP_STATUS_OK: u8 = 0xA1;
const GP_STATUS_FAILED: u8 = 0xA2;
const GP_STATUS_ASLEEP: u8 = 0xA4;

/// Pacchetto: [ID 0, 00, 00, destinatario, lunghezza, banco, registro, dati...].
fn gp_frame(reg: (u8, u8), len: u8, data: &[u8]) -> [u8; GP_LEN] {
    let mut b = [0u8; GP_LEN];
    (b[3], b[4], b[5], b[6]) = (GP_TARGET, len, reg.0, reg.1);
    b[7..7 + data.len()].copy_from_slice(data);
    b
}

/// Dati del registro DPI: [profilo, numero stadi, (X, Y) big endian per stadio].
fn gp_dpi_data(profile: u8, stages: &[u16]) -> Vec<u8> {
    let mut d = vec![profile, stages.len() as u8];
    for s in stages {
        let [hi, lo] = s.to_be_bytes();
        d.extend_from_slice(&[hi, lo, hi, lo]);
    }
    d
}

fn gp_core2_code(hz: u16) -> Option<u8> {
    GP_CORE2_RATES.iter().find(|r| r.0 == hz).map(|r| r.1)
}

/// Dati del registro effetto (mappa classic): [profilo, FF, effetto, 00, velocità, RGB].
fn gp_effect_data(profile: u8, effect: &Effect) -> Result<Vec<u8>> {
    let (id, rate, color) = match effect {
        Effect::Off => (0x00, 0, None),
        Effect::Spectrum => (0x01, GP_RATE, None),
        Effect::Static { color } => (0x04, 0, Some(*color)),
        Effect::Breathing { color } => (0x05, GP_RATE, Some(*color)),
        e => bail!("effetto '{}' non supportato", e.name()),
    };
    let mut d = vec![profile, 0xFF, id, 0x00, rate];
    if let Some(c) = color {
        d.extend_from_slice(&[c.0, c.1, c.2]);
    }
    Ok(d)
}

pub struct Gp {
    dev: HidDevice,
    spec: DeviceSpec,
    map: Map,
    cache_pid: u16,
    profile_ready: bool,
}

impl Gp {
    fn open(api: &HidApi, spec: DeviceSpec, cfg: GloriousConfig) -> Result<Gp> {
        let (vid, pid) = (spec.vid, spec.pid);
        let map = cfg.map.context("manca map nella sezione [glorious]")?;
        // Interfaccia 2, collezione del produttore con un feature report da 64 byte. Su Windows c'è
        // un'altra collezione 0xFFFF con report di lunghezza 0: va scartata.
        let dev = api
            .device_list()
            .filter(|d| d.vendor_id() == vid && d.product_id() == pid && d.interface_number() == 2 && d.usage_page() >= 0xFF00)
            .filter_map(|d| d.open_device(api).ok())
            .find(|dev| crate::hid::feature_len(dev, 0) == Some(GP_LEN - 1))
            .with_context(|| format!("{}: interfaccia di configurazione non trovata. Chiudi Glorious CORE e riprova.", spec.name))?;
        Ok(Gp { dev, map, cache_pid: cfg.cable_pid.unwrap_or(pid), spec, profile_ready: false })
    }

    fn send(&self, reg: (u8, u8), data: &[u8], after_ms: u64) -> Result<()> {
        let allowed: &[(u8, u8)] = match self.map {
            Map::Classic => &GP_CLASSIC_WRITES,
            Map::Core2 => &GP_CORE2_WRITES,
        };
        if !allowed.contains(&reg) {
            bail!("registro Glorious {:02x}:{:02x} bloccato", reg.0, reg.1);
        }
        let b = gp_frame(reg, data.len() as u8, data);
        debug(">", &b);
        self.dev.send_feature_report(&b).context("scrittura sul mouse non riuscita")?;
        ms(after_ms);
        Ok(())
    }

    /// Legge la risposta all'ultimo comando (come fa CORE alla fine di una serie). Le impostazioni non si
    /// possono rileggere, ma lo stato dice se il mouse ha rifiutato il comando o non è raggiungibile.
    fn ack(&self, reg: (u8, u8)) -> Result<()> {
        ms(60);
        let mut b = [0u8; GP_LEN];
        let Ok(n) = self.dev.get_feature_report(&mut b) else {
            debug("< (nessuna risposta)", &[]);
            return Ok(());
        };
        debug("<", &b[..n]);
        if n < 7 || (b[5], b[6]) != reg {
            return Ok(());
        }
        match b[1] {
            GP_STATUS_FAILED => bail!("il mouse ha rifiutato il comando"),
            GP_STATUS_ASLEEP => bail!("il ricevitore non trova il mouse: muovilo per svegliarlo e riprova"),
            _ => Ok(()),
        }
    }

    /// Attiva il profilo 1 prima della prima scrittura (il profilo attivo non si può leggere).
    fn profile(&mut self) -> Result<()> {
        if !self.profile_ready {
            self.send(GP_REG_PROFILE, &[GP_PROFILE], 100)?;
            self.profile_ready = true;
        }
        Ok(())
    }

    fn update(&self, f: impl FnOnce(&mut MouseState)) {
        let mut s = cached_state(self.spec.vid, self.cache_pid, &self.spec.caps);
        f(&mut s);
        s.dpi = s.dpi_stages.as_ref().and_then(|d| d.stages.get(d.active).copied());
        cache::save(self.spec.vid, self.cache_pid, &s);
    }
}

impl MouseDriver for Gp {
    fn spec(&self) -> &DeviceSpec {
        &self.spec
    }

    fn identify(&mut self) -> Result<DeviceInfo> {
        // Firmware: solo mappa core2 (richiesta come nella cattura di CORE, lunghezza 3 senza dati).
        let firmware = match self.map {
            Map::Core2 => {
                let req = gp_frame(GP_REG_FIRMWARE, 3, &[]);
                debug(">", &req);
                self.dev.send_feature_report(&req).ok().and_then(|_| {
                    ms(60);
                    let mut b = [0u8; GP_LEN];
                    let n = self.dev.get_feature_report(&mut b).ok()?;
                    debug("<", &b[..n]);
                    (n >= 11 && b[1] == GP_STATUS_OK && (b[5], b[6]) == GP_REG_FIRMWARE)
                        .then(|| format!("{}.{}.{}.{}", b[7], b[8], b[9], b[10]))
                })
            }
            Map::Classic => None,
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
        Ok(cached_state(self.spec.vid, self.cache_pid, &self.spec.caps))
    }

    fn set_dpi_stages(&mut self, stages: &[u16], active: usize) -> Result<()> {
        self.spec.caps.check_dpi_stages(stages, active)?;
        ensure!(stages.len() <= 6, "al massimo 6 stadi");
        if self.map == Map::Classic {
            ensure!(stages.len() == 4, "questo mouse ha esattamente 4 stadi DPI");
        }
        self.profile()?;
        // Ordine di CORE: stadi, poi stadio attivo (1-based) e 120 ms di pausa.
        self.send(GP_REG_DPI, &gp_dpi_data(GP_PROFILE, stages), 30)?;
        self.send(GP_REG_ACTIVE, &[GP_PROFILE, active as u8 + 1], 120)?;
        self.ack(GP_REG_ACTIVE)?;
        let stages = stages.to_vec();
        self.update(|s| s.dpi_stages = Some(DpiStages { stages, active }));
        Ok(())
    }

    fn set_polling(&mut self, hz: u16) -> Result<()> {
        self.spec.caps.check_polling(hz)?;
        let reg = match self.map {
            Map::Classic => {
                // Registro globale, senza profilo.
                let ms = GP_CLASSIC_RATES.iter().find(|r| r.1 == hz).context("polling non gestito")?.0;
                self.send(GP_REG_POLL_CLASSIC, &[ms], 30)?;
                GP_REG_POLL_CLASSIC
            }
            Map::Core2 => {
                self.profile()?;
                let cable = gp_core2_code(hz).context("polling non gestito")?;
                let wireless = gp_core2_code(hz.min(GP_CORE2_WIRELESS_MAX)).context("polling non gestito")?;
                self.send(GP_REG_POLL_CORE2, &[GP_PROFILE, cable, wireless], 30)?;
                GP_REG_POLL_CORE2
            }
        };
        self.ack(reg)?;
        self.update(|s| s.polling_hz = Some(hz));
        Ok(())
    }

    fn set_lighting(&mut self, zone: &str, effect: &Effect) -> Result<()> {
        self.spec.caps.check_effect(zone, effect)?;
        // Mappa core2: CORE userebbe il formato classic, ma nessuna cattura lo conferma.
        ensure!(self.map == Map::Classic, "luci non ancora supportate su questo modello");
        self.profile()?;
        self.send(GP_REG_LED_EFFECT, &gp_effect_data(GP_PROFILE, effect)?, 30)?;
        self.ack(GP_REG_LED_EFFECT)?;
        let (zone, effect) = (zone.to_string(), effect.clone());
        self.update(move |s| zone_mut(s, &zone).effect = Some(effect));
        Ok(())
    }

    fn set_brightness(&mut self, zone: &str, value: u8) -> Result<()> {
        check_zone(&self.spec.caps, zone)?;
        ensure!(self.map == Map::Classic, "luci non ancora supportate su questo modello");
        // Registro globale, senza profilo: [collegamento, valore], prima cavo (01) poi wireless (00).
        self.send(GP_REG_LED_BRIGHTNESS, &[0x01, value], 30)?;
        self.send(GP_REG_LED_BRIGHTNESS, &[0x00, value], 30)?;
        self.ack(GP_REG_LED_BRIGHTNESS)?;
        let zone = zone.to_string();
        self.update(move |s| zone_mut(s, &zone).brightness = Some(value));
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hexpad<const N: usize>(text: &str) -> [u8; N] {
        let mut out = [0u8; N];
        for (i, h) in text.split_whitespace().enumerate() {
            out[i] = u8::from_str_radix(h, 16).unwrap();
        }
        out
    }

    // Vettori di danexpo/o2ctl (tests/test_protocol.py), profilo di fabbrica 400/800/1600/3200, attivo 3.
    #[test]
    fn g2_perf_dongle_matches_o2ctl() {
        let p = g2_perf_packets(1, &[400, 800, 1600, 3200], 2, 1000, false).unwrap();
        assert_eq!(p.len(), 4);
        assert_eq!(p[0], hexpad::<64>("03 04 fb 00 01 02 04 01 02 04 00 08 00 ff ff 00"));
        assert_eq!(p[1], hexpad::<64>("03 04 fb 01 01 10 00 00 00 ff 20 00 ff 00 00"));
        assert_eq!(p[2], hexpad::<64>("03 04 fb 02 01 40 00 00 ff 00"));
        assert_eq!(p[3], hexpad::<64>("03 04 fb 03 01"));
    }

    #[test]
    fn g2_perf_cable_matches_o2ctl() {
        let p = g2_perf_packets(2, &[400, 800, 1600, 3200], 2, 1000, true).unwrap();
        let text = "03 04 fa 02 02 04 01 02 04 00 00 00 00 00 00 00 \
                    08 00 ff ff 00 00 00 00 10 00 00 00 ff 00 00 00 20 00 ff 00 00 00 00 00 40 00 00 ff 00";
        assert_eq!(p, vec![hexpad::<64>(text)]);
    }

    // Pacchetto provato da o2ctl su un Model O 2 Wireless: verde fisso.
    #[test]
    fn g2_lighting_matches_hardware_packet() {
        let green = Effect::Static { color: Rgb(0, 255, 0) };
        let p = g2_light_packets(1, &green, g2_level(255), false).unwrap();
        assert_eq!(p[0], hexpad::<64>("03 02 fb 00 01 04 0c 14 01 0c 14 00 ff 00"));
        assert_eq!(p[1], hexpad::<64>("03 02 fb 01 01 04"));
        assert_eq!(p[2], hexpad::<64>("03 02 fb 02 01 04"));
        let p = g2_light_packets(3, &green, 20, true).unwrap();
        assert_eq!(p, vec![hexpad::<64>("03 02 fa 03 04 0c 14 01 0c 14 00 00 00 00 00 00 00 ff 00")]);
    }

    #[test]
    fn g2_dpi_and_levels() {
        assert_eq!(g2_encode_dpi(400), [0x08, 0x00]);
        assert_eq!(g2_encode_dpi(26000), [0x08, 0x02]);
        assert_eq!(g2_level(0), 1);
        assert_eq!(g2_level(255), 20);
        assert_eq!(g2_level(128), 10);
        assert!(g2_perf_packets(1, &[400, 800, 1600], 0, 1000, false).is_err());
        assert!(g2_perf_packets(1, &[400, 800, 1600, 3200], 0, 2000, false).is_err());
    }

    #[test]
    fn g3_frames_and_stages() {
        assert_eq!(g3_stage_payload(1, Some(800), [1, 2, 3]), [1, 0x03, 0x20, 1, 2, 3, 1, 2, 3]);
        assert_eq!(g3_stage_payload(5, None, [1, 2, 3]), [5, 0, 0, 0, 0, 0, 0, 0, 0]);
        let f = g3_frame(G3_POLL_CABLE, &1000u16.to_be_bytes()).unwrap();
        assert_eq!(&f[..6], &[0x03, 0xBB, 0x0D, 0x02, 0x03, 0xE8]);
        assert!(g3_frame(0x18, &[]).is_err()); // tasti: bloccato
        assert!(g3_frame(0x04, &[]).is_err()); // risparmio energetico: bloccato
        let s = g3_stages(&[400, 0, 1600, 3200, 0, 0], 2).unwrap();
        assert_eq!(s, DpiStages { stages: vec![400, 1600, 3200], active: 1 });
        assert_eq!(g3_stages(&[0; 6], 0), None);
        let mut reply = [0u8; 64];
        reply[..7].copy_from_slice(&[0x03, 0xBB, 0x90, 0x01, 0x02, 0xFF, 0xFF]);
        assert_eq!(g3_reply(&reply, 0x90).unwrap()[..2], [0x02, 0x00]);
        assert_eq!(g3_reply(&[0u8; 64], 0x90), None);
    }

    #[test]
    fn g3_lighting_keeps_brightness_and_speed() {
        // Letto: arcobaleno, luminosità 80/40, velocità 50.
        let cur = [1, 80, 40, 0, 50, 8, 0, 0, 0, 0, 0, 0];
        let p = g3_light_payload(&cur, &Effect::Static { color: Rgb(1, 2, 3) }).unwrap();
        assert_eq!(p, [5, 80, 40, 0, 50, 8, 1, 1, 2, 3]);
        assert_eq!(g3_decode_effect(&p), Effect::Static { color: Rgb(1, 2, 3) });
        let p = g3_light_payload(&[0; 10], &Effect::Spectrum).unwrap();
        assert_eq!(p, [1, 0, 0, 0, 10, 8, 0, 0, 0, 0]);
        assert_eq!(g3_decode_effect(&p), Effect::Spectrum);
        assert_eq!(g3_decode_effect(&[10, 0, 0, 0, 0, 8, 0, 0, 0, 0]), Effect::Unknown { code: 10 });
    }

    // Cattura di CORE su un O 2 PRO 4K/8K (OpenMouse) e misura di GloriousAutoPollingRate.
    #[test]
    fn gp_core2_matches_capture() {
        let dpi = gp_dpi_data(2, &[400, 800, 1600, 3200]);
        let f = gp_frame(GP_REG_DPI, dpi.len() as u8, &dpi);
        assert_eq!(f, hexpad::<65>("00 00 00 02 12 01 01 02 04 01 90 01 90 03 20 03 20 06 40 06 40 0c 80 0c 80"));
        let f = gp_frame(GP_REG_ACTIVE, 2, &[2, 1]);
        assert_eq!(f, hexpad::<65>("00 00 00 02 02 01 02 02 01"));
        let f = gp_frame(GP_REG_POLL_CORE2, 3, &[1, gp_core2_code(125).unwrap(), gp_core2_code(125).unwrap()]);
        assert_eq!(f, hexpad::<65>("00 00 00 02 03 01 0a 01 08 08"));
        assert_eq!(gp_core2_code(4000), Some(0x40));
        assert_eq!(gp_core2_code(300), None);
        assert_eq!(gp_frame(GP_REG_FIRMWARE, 3, &[]), hexpad::<65>("00 00 00 02 03 00 81"));
    }

    // Pacchetti di korkje/mxw (Model O/D Wireless).
    #[test]
    fn gp_classic_matches_mxw() {
        let d = gp_effect_data(1, &Effect::Static { color: Rgb(0xAA, 0xBB, 0xCC) }).unwrap();
        let f = gp_frame(GP_REG_LED_EFFECT, d.len() as u8, &d);
        assert_eq!(f, hexpad::<65>("00 00 00 02 08 02 00 01 ff 04 00 00 aa bb cc"));
        let d = gp_effect_data(1, &Effect::Spectrum).unwrap();
        assert_eq!(gp_frame(GP_REG_LED_EFFECT, d.len() as u8, &d), hexpad::<65>("00 00 00 02 05 02 00 01 ff 01 00 0d"));
        let d = gp_effect_data(1, &Effect::Off).unwrap();
        assert_eq!(gp_frame(GP_REG_LED_EFFECT, d.len() as u8, &d), hexpad::<65>("00 00 00 02 05 02 00 01 ff 00 00 00"));
        assert_eq!(gp_frame(GP_REG_POLL_CLASSIC, 1, &[8]), hexpad::<65>("00 00 00 02 01 01 00 08"));
        assert_eq!(gp_frame(GP_REG_PROFILE, 1, &[1]), hexpad::<65>("00 00 00 02 01 00 05 01"));
        assert_eq!(gp_frame(GP_REG_LED_BRIGHTNESS, 2, &[1, 200]), hexpad::<65>("00 00 00 02 02 02 02 01 c8"));
        assert_eq!(gp_dpi_data(1, &[19000, 100, 800, 1600]).len(), 0x12);
    }
}
