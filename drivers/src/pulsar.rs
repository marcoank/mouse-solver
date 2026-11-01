//! Driver Pulsar "famiglia S" e Lamzu "Aurora". Protocolli reimplementati leggendo
//! pulsar-mouse-linux e Pulsar-X3-Web-Config (licenza MIT) per Pulsar, lamzuctl (MIT) e le note
//! di All-in-One-Mouse-Driver (GPL-3.0, solo i fatti) per Lamzu: nessun codice copiato.
//!
//! Pulsar S (VID 0x3710: X2/X2H/X2A/Xlite cablati, Xlite v4, X3, ricevitori 1K/8K, Feinmann 8K):
//! feature report da 64 byte senza ID sull'interfaccia 3 (usage page 0xFFFF). Ogni comando è
//! [0, categoria, registro, sotto-registro, 0, 0, profilo, dati..., somma u16 LE dei byte 0-61];
//! il bit 7 del registro indica una lettura. La risposta arriva con GET_REPORT quando il byte 0
//! vale 0x01 (0x02 = ancora in lavorazione, 0x03/0x04 = rifiutato, 0x05 = mouse addormentato).
//! Nessuna fonte documenta un comando di salvataggio: i valori risultano conservati nel mouse
//! (scritti via radio e riletti dal cavo), ma nessuno ha provato a togliergli la corrente.
//! Ogni scrittura viene riletta e confrontata.
//!
//! Lamzu Aurora (VID 0x373E / 0x37B0): feature report da 64 byte senza ID sull'interfaccia 2,
//! [0, 0, 0x02, lunghezza, categoria, opcode, profilo, dati...], senza checksum; la risposta ha
//! un marcatore 0xA0-0xAF nel byte 1 e ripete categoria e opcode. Si scrivono solo polling e
//! stadio attivo (presi da catture e da codice provato); i valori DPI si leggono ma non si
//! scrivono e le luci sono escluse, perché i loro comandi vengono solo da documentazione
//! generata e mai provata su un mouse.

use anyhow::{bail, Context, Result};
use hidapi::{HidApi, HidDevice};
use mousecfg_core::*;
use serde::Deserialize;
use std::thread::sleep;
use std::time::{Duration, Instant};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Family {
    /// Pulsar, protocollo a 64 byte con checksum (Fusion / Fusion2).
    PulsarS,
    /// Lamzu, protocollo "Aurora" a 64 byte senza checksum.
    LamzuAurora,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PulsarConfig {
    pub family: Family,
    /// Solo Pulsar S: true = chiede al mouse il profilo attivo (provato su X2A e Feinmann 8K);
    /// false = usa sempre il profilo 1 (sull'X3 quella lettura non risponde).
    #[serde(default)]
    pub read_profile: bool,
    /// Solo Pulsar S: profili salvati nel mouse (5 o 6).
    #[serde(default = "six")]
    pub profiles: u8,
    /// Solo Pulsar S: pausa dopo ogni scrittura in ms (Feinmann 8K: 300).
    #[serde(default = "gap")]
    pub write_gap_ms: u64,
    /// Solo Pulsar S: il LED indicatore DPI (zona "dpi") ha registri provati sul mouse.
    #[serde(default)]
    pub led: bool,
}

fn six() -> u8 {
    6
}

fn gap() -> u64 {
    30
}

/// Stadi DPI massimi in entrambe le famiglie.
const MAX_STAGES: usize = 6;
/// Nome della zona luci (il LED che indica lo stadio DPI).
const LED_ZONE: &str = "dpi";

fn debug(dir: &str, data: &[u8]) {
    if mousecfg_core::trace::on() {
        // Gli zeri finali non dicono niente: li taglio (lasciando almeno 8 byte).
        let n = data.iter().rposition(|&b| b != 0).map_or(0, |i| i + 1).max(8).min(data.len());
        mousecfg_core::trace!("{dir} {:02x?}", &data[..n]);
    }
}

fn note(msg: &str) {
    if mousecfg_core::trace::on() {
        mousecfg_core::trace!("pulsar: {msg}");
    }
}

/// Apre il mouse e restituisce il driver.
pub fn open(api: &HidApi, spec: &DeviceSpec, vid: u16, pid: u16) -> Result<Box<dyn MouseDriver>> {
    let cfg: PulsarConfig = spec.section("pulsar")?;
    let mut spec = spec.clone();
    (spec.vid, spec.pid) = (vid, pid);
    match cfg.family {
        Family::PulsarS => Ok(Box::new(PulsarS::open(api, spec, cfg)?)),
        Family::LamzuAurora => Ok(Box::new(Aurora::open(api, spec)?)),
    }
}

/// Controlla che la sezione del driver nel file TOML sia valida (usato dai test del database).
pub fn check_config(spec: &DeviceSpec) -> Result<()> {
    let cfg: PulsarConfig = spec.section("pulsar")?;
    let caps = &spec.caps;
    if caps.max_stages as usize > MAX_STAGES || caps.min_stages == 0 {
        bail!(tf!("stadi DPI ammessi: da 1 a {MAX_STAGES}", "allowed DPI stages: 1 to {MAX_STAGES}"));
    }
    match cfg.family {
        Family::PulsarS => {
            if spec.vid != 0x3710 {
                bail!(t!("la famiglia pulsar_s usa solo il VID 0x3710", "the pulsar_s family only uses VID 0x3710"));
            }
            for &hz in &caps.polling {
                s_poll_code(hz).with_context(|| tf!("polling {hz} Hz non codificabile", "polling {hz} Hz cannot be encoded"))?;
            }
            if !(1..=6).contains(&cfg.profiles) {
                bail!(t!("profiles deve essere tra 1 e 6", "profiles must be between 1 and 6"));
            }
            if caps.dpi_max > 32000 || caps.dpi_min == 0 {
                bail!(t!("DPI fuori dai limiti del protocollo", "DPI outside the protocol limits"));
            }
            match cfg.led {
                true if caps.zones != [LED_ZONE] => bail!(tf!("con led = true l'unica zona è \"{LED_ZONE}\"", "with led = true the only zone is \"{LED_ZONE}\"")),
                false if !caps.zones.is_empty() => bail!(t!("zone senza led = true", "zones without led = true")),
                _ => {}
            }
            if let Some(e) = caps.effects.iter().find(|e| !["static", "breathing", "off"].contains(&e.as_str())) {
                bail!(tf!("effetto '{e}' non disponibile sul LED Pulsar", "effect '{e}' not available on the Pulsar LED"));
            }
        }
        Family::LamzuAurora => {
            if spec.vid != 0x373E && spec.vid != 0x37B0 {
                bail!(t!("la famiglia lamzu_aurora usa solo i VID 0x373E e 0x37B0", "the lamzu_aurora family only uses VIDs 0x373E and 0x37B0"));
            }
            for &hz in &caps.polling {
                a_poll_code(hz).with_context(|| tf!("polling {hz} Hz non codificabile", "polling {hz} Hz cannot be encoded"))?;
            }
            if cfg.led || cfg.read_profile || !caps.zones.is_empty() || !caps.effects.is_empty() {
                bail!(t!("Lamzu Aurora: luci e opzioni Pulsar non sono gestite", "Lamzu Aurora: lighting and Pulsar options are not supported"));
            }
        }
    }
    Ok(())
}

fn info(spec: &DeviceSpec, firmware: Option<String>) -> DeviceInfo {
    DeviceInfo {
        name: spec.name.clone(),
        vid: spec.vid,
        pid: spec.pid,
        status: spec.status,
        firmware,
        serial: None,
    }
}

/// Collezioni di controllo candidate: pagina del produttore e feature report da 64 byte senza
/// ID (così non si manda nulla alle collezioni mouse/tastiera), interfaccia `iface` per prima.
fn control_infos(api: &HidApi, vid: u16, pid: u16, iface: i32) -> Vec<hidapi::DeviceInfo> {
    let mut v: Vec<_> = api
        .device_list()
        .filter(|d| d.vendor_id() == vid && d.product_id() == pid && d.usage_page() >= 0xFF00)
        .cloned()
        .collect();
    v.sort_by_key(|d| d.interface_number() != iface);
    v
}

fn open_control(api: &HidApi, info: &hidapi::DeviceInfo) -> Option<HidDevice> {
    let dev = info.open_device(api).ok()?;
    (crate::hid::feature_len(&dev, 0) == Some(64)).then_some(dev)
}

// ───────────────────────────── Pulsar famiglia S ─────────────────────────────

/// Scritture ammesse (categoria, registro, sotto-registro). Tutto il resto viene rifiutato
/// prima di arrivare al mouse: reset di fabbrica (02 05 01), cambio profilo (02 06 01), tasti,
/// macro, firmware.
const S_WRITES: [[u8; 3]; 6] = [
    [0x01, 0x09, 0x02], // polling
    [0x05, 0x04, 0x21], // tabella stadi DPI (attivo, numero, stadi)
    [0x05, 0x01, 0x02], // stadio attivo
    [0x03, 0x03, 0x03], // luminosità LED
    [0x03, 0x04, 0x0F], // effetto LED + velocità respiro
    [0x05, 0x05, 0x05], // colore LED di uno stadio
];

/// Letture ammesse (registro con il bit 7 acceso).
const S_READS: [[u8; 3]; 8] = [
    [0x01, 0x89, 0x02], // polling
    [0x05, 0x84, 0x15], // tabella stadi DPI
    [0x05, 0x81, 0x02], // stadio attivo
    [0x02, 0x86, 0x01], // profilo attivo (risposta nel byte 6)
    [0x03, 0x83, 0x03], // luminosità LED
    [0x03, 0x84, 0x0F], // effetto LED
    [0x05, 0x85, 0x05], // colore LED di uno stadio
    [0x01, 0x87, 0x04], // versione firmware
];

/// Polling: 125 Hz = 0x01 e raddoppia a ogni bit, fino a 8000 Hz = 0x40.
const S_POLL: [(u16, u8); 7] = [(125, 0x01), (250, 0x02), (500, 0x04), (1000, 0x08), (2000, 0x10), (4000, 0x20), (8000, 0x40)];

const S_POLL_EVERY: Duration = Duration::from_millis(15);
const S_TIMEOUT: Duration = Duration::from_millis(1000);
/// Appena aperto, via cavo un comando può restare a 0x00 (mai preso): le letture si rimandano.
const S_ZERO_POLLS: u32 = 8;
const S_READ_RESENDS: u8 = 6;

fn s_poll_code(hz: u16) -> Option<u8> {
    S_POLL.iter().find(|p| p.0 == hz).map(|p| p.1)
}

fn s_poll_hz(code: u8) -> Option<u16> {
    S_POLL.iter().find(|p| p.1 == code).map(|p| p.0)
}

fn s_checksum(f: &[u8; 64]) -> u16 {
    f[..62].iter().fold(0u16, |a, &b| a.wrapping_add(b as u16))
}

/// Comando da 64 byte (senza l'ID del report). `payload` va dal byte 7 (al massimo 55 byte).
fn s_frame(cat: u8, reg: u8, sub: u8, profile: u8, payload: &[u8]) -> [u8; 64] {
    let mut f = [0u8; 64];
    f[1] = cat;
    f[2] = reg;
    f[3] = sub;
    f[6] = profile;
    f[7..7 + payload.len()].copy_from_slice(payload);
    let c = s_checksum(&f);
    f[62..].copy_from_slice(&c.to_le_bytes());
    f
}

fn s_allowed(f: &[u8; 64]) -> bool {
    let key = [f[1], f[2], f[3]];
    f[0] == 0 && s_checksum(f).to_le_bytes() == [f[62], f[63]] && (S_WRITES.contains(&key) || S_READS.contains(&key))
}

/// La risposta è proprio a questa richiesta? Categoria e registro sempre, il profilo se non è 0,
/// il byte 7 (indice dello stadio) per i colori LED.
fn s_echoes(rsp: &[u8; 64], req: &[u8; 64]) -> bool {
    if rsp[1] != req[1] || rsp[2] != req[2] || (req[6] != 0 && rsp[6] != req[6]) {
        return false;
    }
    req[1] != 0x05 || req[2] & 0x7F != 0x05 || rsp[7] == req[7]
}

/// Dati della tabella DPI: [attivo da 1, numero, (indice, X lo, X hi, Y lo, Y hi) per stadio].
fn s_dpi_payload(stages: &[u16], active: usize) -> Vec<u8> {
    let mut p = vec![active as u8 + 1, stages.len() as u8];
    for (i, &d) in stages.iter().enumerate() {
        let [lo, hi] = d.to_le_bytes();
        p.extend_from_slice(&[i as u8 + 1, lo, hi, lo, hi]);
    }
    p
}

/// Tabella DPI letta: (stadio attivo da 1, [(X, Y)]). Gli slot oltre il numero di stadi
/// contengono valori vecchi e vengono ignorati.
fn s_dpi_decode(rsp: &[u8; 64]) -> Option<(u8, Vec<(u16, u16)>)> {
    let count = rsp[8] as usize;
    if count == 0 || count > MAX_STAGES {
        return None;
    }
    let stages = (0..count)
        .map(|i| {
            let b = 9 + i * 5;
            (u16::from_le_bytes([rsp[b + 1], rsp[b + 2]]), u16::from_le_bytes([rsp[b + 3], rsp[b + 4]]))
        })
        .collect();
    Some((rsp[7], stages))
}

/// Versione firmware: byte 6-9 in little endian, scritti dal più alto e senza zeri iniziali
/// (X3: 16 10 00 00 = "10.16"; X2A: 00 00 08 00 = "08.00.00").
fn firmware_text(r: &[u8; 64]) -> String {
    let v: Vec<u8> = r[6..10].iter().rev().copied().collect();
    let start = v.iter().position(|&b| b != 0).unwrap_or(4).min(2);
    v[start..].iter().map(|b| format!("{b:02x}")).collect::<Vec<_>>().join(".")
}

struct PulsarS {
    dev: HidDevice,
    spec: DeviceSpec,
    cfg: PulsarConfig,
    /// Profilo a cui vanno DPI e luci (il polling usa sempre 0 = profilo attivo).
    profile: u8,
    /// Il mouse ha già risposto: da qui in poi niente reinvii.
    answered: bool,
    resends: u8,
}

impl PulsarS {
    fn open(api: &HidApi, spec: DeviceSpec, cfg: PulsarConfig) -> Result<PulsarS> {
        let mut asleep = false;
        for info in control_infos(api, spec.vid, spec.pid, 3) {
            let Some(dev) = open_control(api, &info) else { continue };
            let mut d = PulsarS { dev, spec: spec.clone(), cfg: cfg.clone(), profile: 1, answered: false, resends: S_READ_RESENDS };
            // Prima lettura (polling): verificata su tutti i modelli, dice se è l'interfaccia giusta.
            match d.read(0x01, 0x09, 0x02, 0, &[]) {
                Ok(_) => {}
                Err(e) => {
                    let msg = e.to_string();
                    asleep |= msg.contains("addormentato") || msg.contains("asleep");
                    continue;
                }
            }
            if d.cfg.read_profile {
                match d.read(0x02, 0x06, 0x01, 0, &[]) {
                    Ok(r) if (1..=d.cfg.profiles).contains(&r[6]) => d.profile = r[6],
                    _ => note("profilo attivo non leggibile: uso il profilo 1"),
                }
            }
            return Ok(d);
        }
        if asleep {
            bail!(tf!("{}: il mouse è addormentato. Muovilo per svegliarlo e riprova.", "{}: the mouse is asleep. Move it to wake it and try again.", spec.name));
        }
        bail!(tf!("{} trovato ma non risponde. Chiudi Pulsar Fusion e riprova.", "{} found but not responding. Close Pulsar Fusion and try again.", spec.name))
    }

    /// Invia un comando e aspetta la risposta che gli corrisponde.
    fn transact(&mut self, req: &[u8; 64]) -> Result<[u8; 64]> {
        if !s_allowed(req) {
            bail!(tf!("comando {:02x} {:02x} {:02x} non consentito", "command {:02x} {:02x} {:02x} not allowed", req[1], req[2], req[3]));
        }
        let read = req[2] & 0x80 != 0;
        let mut out = [0u8; 65];
        out[1..].copy_from_slice(req);
        debug(">", req);
        self.dev.send_feature_report(&out)?;
        let start = Instant::now();
        let mut zeros = 0u32;
        let mut last: u8;
        loop {
            sleep(S_POLL_EVERY);
            let mut buf = [0u8; 65];
            let n = self.dev.get_feature_report(&mut buf)?;
            if n < 64 {
                bail!(tf!("risposta troppo corta ({n} byte)", "response too short ({n} bytes)"));
            }
            let mut rsp = [0u8; 64];
            rsp.copy_from_slice(&buf[1..]);
            last = rsp[0];
            match rsp[0] {
                0x01 if s_echoes(&rsp, req) => {
                    debug("<", &rsp);
                    if s_checksum(&rsp).to_le_bytes() != [rsp[62], rsp[63]] {
                        debug("checksum della risposta diverso", &rsp[62..]);
                    }
                    self.answered = true;
                    if !read {
                        sleep(Duration::from_millis(self.cfg.write_gap_ms));
                    }
                    return Ok(rsp);
                }
                0x03 | 0x04 if s_echoes(&rsp, req) => {
                    debug("<", &rsp);
                    bail!(tf!("il mouse ha rifiutato il comando {:02x} {:02x}", "the mouse rejected command {:02x} {:02x}", req[1], req[2]));
                }
                0x05 => {
                    debug("<", &rsp);
                    bail!(t!("il mouse è addormentato o fuori portata: muovilo e riprova", "the mouse is asleep or out of range: move it and try again"));
                }
                0x00 => {
                    zeros += 1;
                    // Solo le letture e solo finché il mouse non ha mai risposto: rimandare
                    // una scrittura già presa è un rischio che nessuno ha misurato.
                    if read && zeros >= S_ZERO_POLLS && !self.answered && self.resends > 0 {
                        self.resends -= 1;
                        zeros = 0;
                        debug(">", req);
                        self.dev.send_feature_report(&out)?;
                    }
                }
                _ => zeros = 0,
            }
            if start.elapsed() > S_TIMEOUT {
                break;
            }
        }
        if !read && last == 0x02 {
            // Via cavo la conferma 0x02 basta (così fa il programma originale): la rilettura
            // successiva dirà se la scrittura è arrivata.
            sleep(Duration::from_millis(self.cfg.write_gap_ms));
            return Ok([0u8; 64]);
        }
        bail!(tf!("il mouse non risponde (stato 0x{last:02x})", "the mouse is not responding (status 0x{last:02x})"))
    }

    fn read(&mut self, cat: u8, reg: u8, sub: u8, profile: u8, payload: &[u8]) -> Result<[u8; 64]> {
        self.transact(&s_frame(cat, reg | 0x80, sub, profile, payload))
    }

    fn write(&mut self, cat: u8, reg: u8, sub: u8, profile: u8, payload: &[u8]) -> Result<()> {
        self.transact(&s_frame(cat, reg, sub, profile, payload)).map(|_| ())
    }

    fn read_dpi(&mut self) -> Result<(u8, Vec<(u16, u16)>)> {
        let r = self.read(0x05, 0x04, 0x15, self.profile, &[])?;
        s_dpi_decode(&r).context(t!("tabella DPI non valida", "invalid DPI table"))
    }

    fn read_color(&mut self, stage: u8) -> Result<Rgb> {
        let r = self.read(0x05, 0x05, 0x05, self.profile, &[stage])?;
        Ok(Rgb(r[8], r[9], r[10]))
    }

    fn read_led_block(&mut self) -> Result<[u8; 64]> {
        self.read(0x03, 0x04, 0x0F, self.profile, &[0x01])
    }

    fn check_led_zone(&self, zone: &str) -> Result<()> {
        if !self.cfg.led || zone != LED_ZONE {
            bail!(t!("questo mouse non ha luci configurabili", "this mouse has no configurable lighting"));
        }
        Ok(())
    }
}

impl MouseDriver for PulsarS {
    fn spec(&self) -> &DeviceSpec {
        &self.spec
    }

    fn identify(&mut self) -> Result<DeviceInfo> {
        // Byte 6-9: versione in little endian (X3: 16 10 00 00 = 10.16).
        let firmware = self.read(0x01, 0x07, 0x04, 0, &[]).ok().map(|r| firmware_text(&r));
        Ok(info(&self.spec, firmware))
    }

    fn read_state(&mut self) -> Result<MouseState> {
        let poll = self.read(0x01, 0x09, 0x02, 0, &[])?;
        let (active, table) = self.read_dpi()?;
        let stages: Vec<u16> = table.iter().map(|s| s.0).collect();
        let active = (active as usize).saturating_sub(1).min(stages.len() - 1);
        let mut zones = Vec::new();
        if self.cfg.led {
            let block = self.read_led_block()?;
            let color = self.read_color(active as u8 + 1)?;
            let effect = match block[8] {
                0 => Effect::Off,
                1 => Effect::Static { color },
                2 => Effect::Breathing { color },
                code => Effect::Unknown { code },
            };
            let brightness = self.read(0x03, 0x03, 0x03, self.profile, &[0x01])?[8];
            zones.push(ZoneState { zone: LED_ZONE.into(), effect: Some(effect), brightness: Some(brightness) });
        }
        Ok(MouseState {
            dpi: stages.get(active).copied(),
            dpi_stages: Some(DpiStages { stages, active }),
            polling_hz: s_poll_hz(poll[7]),
            zones,
            cached: false,
        })
    }

    fn set_dpi_stages(&mut self, stages: &[u16], active: usize) -> Result<()> {
        self.spec.caps.check_dpi_stages(stages, active)?;
        self.write(0x05, 0x04, 0x21, self.profile, &s_dpi_payload(stages, active))?;
        // Sull'X3 la tabella sceglie anche lo stadio attivo; se un modello non lo fa,
        // si usa il comando apposito (anche questo documentato e provato).
        let (got, _) = self.read_dpi()?;
        if got as usize != active + 1 {
            self.write(0x05, 0x01, 0x02, self.profile, &[active as u8 + 1])?;
        }
        let (got, table) = self.read_dpi()?;
        let want: Vec<(u16, u16)> = stages.iter().map(|&d| (d, d)).collect();
        if got as usize != active + 1 || table != want {
            bail!(t!("verifica fallita: il mouse non ha salvato gli stadi DPI come inviati", "verification failed: the mouse did not save the DPI stages as sent"));
        }
        Ok(())
    }

    fn set_polling(&mut self, hz: u16) -> Result<()> {
        self.spec.caps.check_polling(hz)?;
        let code = s_poll_code(hz).context(t!("polling non gestito", "polling rate not supported"))?;
        self.write(0x01, 0x09, 0x02, 0, &[code])?;
        if self.read(0x01, 0x09, 0x02, 0, &[])?[7] != code {
            bail!(t!("verifica fallita: il polling non è cambiato", "verification failed: the polling rate did not change"));
        }
        Ok(())
    }

    fn set_lighting(&mut self, zone: &str, effect: &Effect) -> Result<()> {
        self.check_led_zone(zone)?;
        self.spec.caps.check_effect(zone, effect)?;
        let code = match effect {
            Effect::Off => 0,
            Effect::Static { .. } => 1,
            Effect::Breathing { .. } => 2,
            e => bail!(tf!("effetto '{}' non gestito", "effect '{}' not supported", e.name())),
        };
        // Il colore è per stadio DPI: per un colore unico lo scrivo su tutti gli stadi.
        if let Some(c) = effect.color() {
            for stage in 1..=self.spec.caps.max_stages {
                self.write(0x05, 0x05, 0x05, self.profile, &[stage, c.0, c.1, c.2])?;
                if self.read_color(stage)? != c {
                    bail!(tf!("verifica fallita: colore dello stadio {stage} non salvato", "verification failed: stage {stage} color not saved"));
                }
            }
        }
        // Effetto e velocità del respiro stanno nello stesso blocco: leggo, cambio solo
        // l'effetto e riscrivo il resto com'era.
        let cur = self.read_led_block()?;
        self.write(0x03, 0x04, 0x0F, self.profile, &[0x01, code, cur[9], cur[10], cur[11]])?;
        let got = self.read_led_block()?;
        if got[8] != code || got[11] != cur[11] {
            bail!(t!("verifica fallita: effetto non salvato", "verification failed: effect not saved"));
        }
        Ok(())
    }

    fn set_brightness(&mut self, zone: &str, value: u8) -> Result<()> {
        self.check_led_zone(zone)?;
        self.write(0x03, 0x03, 0x03, self.profile, &[0x01, value])?;
        if self.read(0x03, 0x03, 0x03, self.profile, &[0x01])?[8] != value {
            bail!(t!("verifica fallita: luminosità non salvata", "verification failed: brightness not saved"));
        }
        Ok(())
    }
}

// ─────────────────────────────── Lamzu Aurora ───────────────────────────────

/// ID dispositivo: i comandi di configurazione vanno sempre al mouse, anche via ricevitore.
const A_MOUSE: u8 = 0x02;

/// Letture ammesse (lunghezza, categoria, opcode): profilo, polling, stadi DPI, stadio attivo.
const A_READS: [[u8; 3]; 4] = [[0x01, 0x00, 0x85], [0x02, 0x01, 0x80], [0x0A, 0x01, 0x81], [0x02, 0x01, 0x82]];
/// Scritture ammesse: polling e stadio attivo. Niente valori DPI (0x01) né luci (categoria
/// 0x02): documentati solo da testi generati e mai provati. Niente firmware né cambio profilo.
const A_WRITES: [[u8; 3]; 2] = [[0x02, 0x01, 0x00], [0x02, 0x01, 0x02]];

/// Polling: 1000 Hz = 0x01 e i bit salgono al calare della frequenza; poi 2000/4000/8000.
const A_POLL: [(u16, u8); 7] = [(125, 0x08), (250, 0x04), (500, 0x02), (1000, 0x01), (2000, 0x20), (4000, 0x40), (8000, 0x80)];

/// Pausa tra comandi usata dal programma provato sull'Inca.
const A_WRITE_WAIT: Duration = Duration::from_millis(100);

fn a_poll_code(hz: u16) -> Option<u8> {
    A_POLL.iter().find(|p| p.0 == hz).map(|p| p.1)
}

fn a_poll_hz(code: u8) -> Option<u16> {
    match code {
        0x10 => Some(1000), // alcuni firmware rispondono così a 1000 Hz
        c => A_POLL.iter().find(|p| p.1 == c).map(|p| p.0),
    }
}

/// Report da 65 byte (ID 0 + 64): [0, 0, 0, 0x02, len, categoria, opcode, argomenti...].
fn a_cmd(len: u8, cat: u8, op: u8, args: &[u8]) -> [u8; 65] {
    let mut b = [0u8; 65];
    b[3] = A_MOUSE;
    b[4] = len;
    b[5] = cat;
    b[6] = op;
    b[7..7 + args.len()].copy_from_slice(args);
    b
}

fn a_key(b: &[u8; 65]) -> [u8; 3] {
    [b[4], b[5], b[6]]
}

/// Stadi letti: [numero, poi X hi, X lo, Y hi, Y lo per stadio] (big endian).
fn a_dpi_decode(rsp: &[u8; 65]) -> Option<Vec<u16>> {
    let count = rsp[8] as usize;
    if count == 0 || count > MAX_STAGES {
        return None;
    }
    Some((0..count).map(|i| u16::from_be_bytes([rsp[9 + i * 4], rsp[10 + i * 4]])).collect())
}

struct Aurora {
    dev: HidDevice,
    spec: DeviceSpec,
}

impl Aurora {
    fn open(api: &HidApi, spec: DeviceSpec) -> Result<Aurora> {
        let mut last_err = None;
        for info in control_infos(api, spec.vid, spec.pid, 2) {
            let Some(dev) = open_control(api, &info) else { continue };
            let d = Aurora { dev, spec: spec.clone() };
            match d.profile() {
                Ok(_) => return Ok(d),
                Err(e) => last_err = Some(e),
            }
        }
        match last_err {
            Some(e) => Err(e.context(tf!("{} non risponde. Chiudi il programma Lamzu e riprova.", "{} is not responding. Close the Lamzu software and try again.", spec.name))),
            None => bail!(tf!("{}: interfaccia di controllo non trovata", "{}: control interface not found", spec.name)),
        }
    }

    fn transact(&self, req: &[u8; 65], wait: Duration) -> Result<[u8; 65]> {
        let key = a_key(req);
        if !A_READS.contains(&key) && !A_WRITES.contains(&key) {
            bail!(tf!("comando {:02x} {:02x} non consentito", "command {:02x} {:02x} not allowed", req[5], req[6]));
        }
        debug(">", &req[1..]);
        self.dev.send_feature_report(req)?;
        sleep(wait);
        let mut buf = [0u8; 65];
        let n = self.dev.get_feature_report(&mut buf)?;
        debug("<", &buf[1..]);
        if n < 9 {
            bail!(tf!("risposta troppo corta ({n} byte)", "response too short ({n} bytes)"));
        }
        if buf[1] == 0xA4 {
            bail!(t!("il ricevitore non vede il mouse: accendilo o muovilo e riprova", "the receiver can't see the mouse: turn it on or move it and try again"));
        }
        Ok(buf)
    }

    /// Lettura con controllo di marcatore, categoria, opcode e (se c'è) profilo.
    /// Una risposta vecchia o non pronta si scarta e si rimanda la stessa richiesta.
    fn read(&self, req: [u8; 65]) -> Result<[u8; 65]> {
        if !A_READS.contains(&a_key(&req)) {
            bail!(tf!("lettura {:02x} {:02x} non consentita", "read {:02x} {:02x} not allowed", req[5], req[6]));
        }
        for attempt in 0..5u64 {
            let r = self.transact(&req, Duration::from_millis(10 + 15 * attempt))?;
            let echo = (0xA0..=0xAF).contains(&r[1]) && r[5] == req[5] && r[6] == req[6];
            if echo && (req[7] == 0 || r[7] == req[7]) {
                return Ok(r);
            }
        }
        bail!(t!("il mouse non risponde (risposte non valide)", "the mouse is not responding (invalid responses)"))
    }

    /// Scrittura singola (mai ripetuta): la verifica la fa la rilettura successiva.
    fn write(&self, req: [u8; 65]) -> Result<()> {
        if !A_WRITES.contains(&a_key(&req)) {
            bail!(tf!("scrittura {:02x} {:02x} non consentita", "write {:02x} {:02x} not allowed", req[5], req[6]));
        }
        self.transact(&req, A_WRITE_WAIT).map(|_| ())
    }

    /// Profilo attivo (da 1). 0 = il ricevitore risponde ma il mouse dorme.
    fn profile(&self) -> Result<u8> {
        for _ in 0..3 {
            let p = self.read(a_cmd(0x01, 0x00, 0x85, &[]))?[7];
            if p != 0 {
                return Ok(p);
            }
            sleep(Duration::from_millis(100));
        }
        bail!(t!("il mouse è addormentato o spento: muovilo e riprova", "the mouse is asleep or off: move it and try again"))
    }

    fn read_polling(&self, p: u8) -> Result<u8> {
        Ok(self.read(a_cmd(0x02, 0x01, 0x80, &[p]))?[8])
    }

    fn read_active(&self, p: u8) -> Result<u8> {
        Ok(self.read(a_cmd(0x02, 0x01, 0x82, &[p]))?[8])
    }

    fn read_stages(&self, p: u8) -> Result<Vec<u16>> {
        let r = self.read(a_cmd(0x0A, 0x01, 0x81, &[p, MAX_STAGES as u8]))?;
        a_dpi_decode(&r).context(t!("tabella DPI non valida", "invalid DPI table"))
    }
}

impl MouseDriver for Aurora {
    fn spec(&self) -> &DeviceSpec {
        &self.spec
    }

    fn identify(&mut self) -> Result<DeviceInfo> {
        Ok(info(&self.spec, None))
    }

    fn read_state(&mut self) -> Result<MouseState> {
        let p = self.profile()?;
        let poll = self.read_polling(p)?;
        let stages = self.read_stages(p)?;
        let active = (self.read_active(p)? as usize).saturating_sub(1).min(stages.len() - 1);
        Ok(MouseState {
            dpi: stages.get(active).copied(),
            dpi_stages: Some(DpiStages { stages, active }),
            polling_hz: a_poll_hz(poll),
            zones: Vec::new(),
            cached: false,
        })
    }

    fn set_dpi_stages(&mut self, stages: &[u16], active: usize) -> Result<()> {
        let p = self.profile()?;
        // I valori DPI si scrivono con un comando mai provato: qui si cambia solo lo stadio attivo.
        if self.read_stages(p)? != stages {
            bail!(t!("su questo mouse i valori DPI non si possono ancora cambiare da qui (comando non verificato): puoi scegliere solo lo stadio attivo", "DPI values on this mouse can't be changed from here yet (command not verified): you can only choose the active stage"));
        }
        if active >= stages.len() {
            bail!(tf!("stadio attivo {} non esiste (ci sono {} stadi)", "active stage {} does not exist (there are {} stages)", active + 1, stages.len()));
        }
        self.write(a_cmd(0x02, 0x01, 0x02, &[p, active as u8 + 1]))?;
        if self.read_active(p)? as usize != active + 1 {
            bail!(t!("verifica fallita: lo stadio attivo non è cambiato", "verification failed: the active stage did not change"));
        }
        Ok(())
    }

    fn set_polling(&mut self, hz: u16) -> Result<()> {
        self.spec.caps.check_polling(hz)?;
        let code = a_poll_code(hz).context(t!("polling non gestito", "polling rate not supported"))?;
        let p = self.profile()?;
        self.write(a_cmd(0x02, 0x01, 0x00, &[p, code]))?;
        if a_poll_hz(self.read_polling(p)?) != Some(hz) {
            bail!(t!("verifica fallita: il polling non è cambiato", "verification failed: the polling rate did not change"));
        }
        Ok(())
    }

    fn set_lighting(&mut self, _: &str, _: &Effect) -> Result<()> {
        bail!(t!("luci non gestite su questo mouse: i comandi Lamzu per le luci non sono verificati", "lighting not supported on this mouse: the Lamzu lighting commands are not verified"))
    }

    fn set_brightness(&mut self, _: &str, _: u8) -> Result<()> {
        bail!(t!("luci non gestite su questo mouse: i comandi Lamzu per le luci non sono verificati", "lighting not supported on this mouse: the Lamzu lighting commands are not verified"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hex(s: &str) -> Vec<u8> {
        s.split_whitespace().map(|b| u8::from_str_radix(b, 16).unwrap()).collect()
    }

    fn frame64(s: &str) -> [u8; 64] {
        let mut f = [0u8; 64];
        let v = hex(s);
        f[..v.len()].copy_from_slice(&v);
        f
    }

    #[test]
    fn s_checksum_is_le_sum() {
        // Lettura del polling: 01 + 89 + 02 = 0x8C.
        let f = s_frame(0x01, 0x89, 0x02, 0, &[]);
        assert_eq!((f[62], f[63]), (0x8C, 0x00));
        assert!(s_allowed(&f));
        // Somma oltre 255: riporto nel byte alto.
        let f = s_frame(0x05, 0x05, 0x05, 1, &[1, 0xFF, 0xFF, 0xFF]);
        assert_eq!(u16::from_le_bytes([f[62], f[63]]), 0x05 + 0x05 + 0x05 + 1 + 1 + 3 * 0xFF);
    }

    #[test]
    fn s_allowlist_refuses_others() {
        assert!(!s_allowed(&s_frame(0x02, 0x05, 0x01, 1, &[]))); // reset di fabbrica
        assert!(!s_allowed(&s_frame(0x02, 0x06, 0x01, 2, &[]))); // cambio profilo
        assert!(!s_allowed(&s_frame(0x04, 0x01, 0x06, 1, &[1, 1, 1, 0]))); // tasti
        let mut f = s_frame(0x01, 0x09, 0x02, 0, &[0x08]);
        assert!(s_allowed(&f));
        f[62] ^= 1; // checksum sbagliato
        assert!(!s_allowed(&f));
    }

    #[test]
    fn s_dpi_table_from_x3_capture() {
        // Risposta catturata da un X3 (Pulsar-X3-Web-Config): attivo 2, 2 stadi, 400 e 800,
        // poi avanzi di stadi vecchi da ignorare.
        let r = frame64("01 05 84 0d 00 00 01 02 02 01 90 01 90 01 02 20 03 20 03 00 00 00 00 06 04 80 0c 80 0c");
        assert_eq!(s_dpi_decode(&r), Some((2, vec![(400, 400), (800, 800)])));
        assert_eq!(s_dpi_payload(&[400, 800], 1), hex("02 02 01 90 01 90 01 02 20 03 20 03"));
        let w = s_frame(0x05, 0x04, 0x21, 1, &s_dpi_payload(&[400, 800], 1));
        assert!(s_allowed(&w));
        assert_eq!(&w[7..19], &hex("02 02 01 90 01 90 01 02 20 03 20 03")[..]);
        assert_eq!(s_dpi_payload(&[32000], 0), hex("01 01 01 00 7d 00 7d"));
    }

    #[test]
    fn s_polling_codes() {
        let r = frame64("01 01 89 02 00 00 00 08");
        assert_eq!(s_poll_hz(r[7]), Some(1000));
        assert_eq!(s_poll_code(125), Some(0x01));
        assert_eq!(s_poll_code(8000), Some(0x40));
        assert_eq!(s_poll_code(300), None);
    }

    #[test]
    fn s_firmware_from_capture() {
        assert_eq!(firmware_text(&frame64("01 01 87 04 00 00 16 10")), "10.16");
        assert_eq!(firmware_text(&frame64("01 01 87 04 00 00 00 00 08 00")), "08.00.00");
    }

    #[test]
    fn s_echo_rules() {
        let req = s_frame(0x05, 0x85, 0x05, 1, &[2]);
        assert!(s_echoes(&frame64("01 05 85 05 00 00 01 02 ff 00 00"), &req));
        assert!(!s_echoes(&frame64("01 05 85 05 00 00 01 01 ff 00 00"), &req)); // stadio sbagliato
        assert!(!s_echoes(&frame64("01 05 85 05 00 00 02 02 ff 00 00"), &req)); // profilo sbagliato
        // Il sotto-registro della tabella DPI non sempre torna uguale: non lo controllo.
        let req = s_frame(0x05, 0x84, 0x15, 1, &[]);
        assert!(s_echoes(&frame64("01 05 84 0d 00 00 01 02 02"), &req));
    }

    #[test]
    fn aurora_frames() {
        // Cambio polling a 4000 Hz sul profilo 1, come nelle catture (Maya X 8K / Inca).
        let c = a_cmd(0x02, 0x01, 0x00, &[1, a_poll_code(4000).unwrap()]);
        assert_eq!(&c[..9], &hex("00 00 00 02 02 01 00 01 40")[..]);
        assert!(A_WRITES.contains(&a_key(&c)));
        // Valori DPI e luci non sono tra i comandi ammessi.
        assert!(!A_WRITES.contains(&a_key(&a_cmd(0x1A, 0x01, 0x01, &[1, 1]))));
        assert!(!A_WRITES.contains(&a_key(&a_cmd(0x13, 0x02, 0x01, &[1]))));
        assert_eq!(a_poll_hz(0x10), Some(1000));
        assert_eq!(a_poll_hz(0x08), Some(125));
        // Stadi big endian: 800 e 1600.
        let mut r = [0u8; 65];
        r[..17].copy_from_slice(&hex("00 a1 00 02 0a 01 81 01 02 03 20 03 20 06 40 06 40"));
        assert_eq!(a_dpi_decode(&r), Some(vec![800, 1600]));
    }
}
