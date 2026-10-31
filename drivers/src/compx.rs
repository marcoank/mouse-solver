//! Driver "CompX/Nordic": un solo protocollo usato da più marchi sullo stesso firmware ODM.
//! - Pulsar famiglia N (VID 0x3554: X2V2, Xlite V3, X2A Wireless, ...);
//! - Lamzu Compx (VID 0x3554: Atlantis, Thorn);
//! - Endgame Gear serie WE (VID 0x3367: XM2we, OP1we).
//!
//! Protocollo reimplementato leggendo pulsar-mouse-linux, python-pulsar-mouse-tool, lamzu-cfg,
//! thorn-config, xm2we-linux e OP1we-Control (licenze MIT / Apache-2.0): nessun codice copiato.
//!
//! Pacchetto da 17 byte, report 0x08: [08, comando, stato, ind_hi, ind_lo, lunghezza, dati x10, ck]
//! con ck = 0x55 - somma dei byte 0..15. Pulsar e Lamzu lo mandano come output report e rispondono
//! sull'input report 0x08; Endgame lo vuole come feature report e risponde sull'input report 0x09.
//!
//! Il mouse espone una memoria di configurazione (quella del profilo attivo) a indirizzi fissi.
//! Ogni valore è salvato con il suo byte di controllo: scalari [v, 0x55-v], record [.., 0x55-somma].
//! Si usano SOLO i comandi 0x08 (leggi memoria) e 0x07 (scrivi memoria) e solo sugli indirizzi
//! dei campi gestiti: niente reset (0x09), cambio profilo (0x0F) o 0x0E (su Endgame WE fa
//! ripartire il ricevitore in bootloader). Non esiste un comando "salva": la scrittura va
//! direttamente nella memoria del mouse. Dopo ogni scrittura si rilegge e si confronta.

use anyhow::{bail, Context, Result};
use hidapi::{HidApi, HidDevice};
use mousecfg_core::*;
use serde::Deserialize;
use std::time::{Duration, Instant};

const REPORT: u8 = 0x08;
const LEN: usize = 17;
/// Massimo di byte di dati in un pacchetto.
const MAX_DATA: usize = 10;

const CMD_WRITE: u8 = 0x07;
const CMD_READ: u8 = 0x08;

const ADDR_POLLING: u16 = 0x00;
const ADDR_COUNT: u16 = 0x02;
const ADDR_ACTIVE: u16 = 0x04;
/// 8 record da 4 byte [x_lo, y_lo, attr, ck].
const ADDR_DPI: u16 = 0x0C;
/// 8 record da 4 byte [R, G, B, ck]: colore della spia DPI per ogni stadio.
const ADDR_COLOR: u16 = 0x2C;
const ADDR_LED_MODE: u16 = 0x4C;
const ADDR_LED_BRIGHTNESS: u16 = 0x4E;
const ADDR_LED_ON: u16 = 0x52;
/// Si legge solo la parte della memoria che serve (0x00..0x53).
const IMAGE_LEN: usize = 0x54;
const SLOTS: usize = 8;

const LED_STEADY: u8 = 0x01;
const LED_BREATHE: u8 = 0x02;

/// Polling: Hz -> codice (uguale per tutti e tre i marchi).
const RATES: [(u16, u8); 7] =
    [(1000, 0x01), (500, 0x02), (250, 0x04), (125, 0x08), (2000, 0x10), (4000, 0x20), (8000, 0x40)];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Transport {
    /// hid_write (Pulsar, Lamzu).
    Output,
    /// send_feature_report (Endgame Gear WE).
    Feature,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CompxConfig {
    pub transport: Transport,
    /// ID dell'input report con le risposte: 8 (Pulsar, Lamzu) o 9 (Endgame Gear WE).
    pub reply_id: u8,
    /// Usage page della collezione di configurazione; 0 = qualsiasi pagina vendor (>= 0xFF00).
    #[serde(default)]
    pub usage_page: u16,
    /// Usage della collezione; 0 = qualsiasi.
    #[serde(default)]
    pub usage: u16,
    /// Interfaccia USB da provare per prima.
    #[serde(default)]
    pub interface: Option<i32>,
    /// true = si può cambiare il numero di stadi (indirizzo 0x02).
    pub write_stage_count: bool,
    /// true = spia DPI con i valori Pulsar (effetto 0x4C, luminosità 0x4E, accesa 0x52, colori 0x2C).
    #[serde(default)]
    pub dpi_led: bool,
}

/// Controlla che la sezione [compx] del file TOML sia valida (usato dai test del database).
pub fn check_config(spec: &DeviceSpec) -> Result<()> {
    let cfg: CompxConfig = spec.section("compx")?;
    let c = &spec.caps;
    if !matches!(cfg.reply_id, 8 | 9) {
        bail!("reply_id deve essere 8 o 9");
    }
    if cfg.usage_page != 0 && cfg.usage_page < 0xFF00 {
        bail!("usage_page deve essere una pagina vendor (>= 0xFF00)");
    }
    // Codifica DPI documentata solo a passi di 50, con al massimo 10 bit (26000 = limite delle fonti).
    if c.dpi_step != 50 || c.dpi_min < 50 || c.dpi_min % 50 != 0 || c.dpi_max > 26000 || !c.dpi_values.is_empty() {
        bail!("DPI: servono passi da 50 tra 50 e 26000");
    }
    if c.max_stages as usize > SLOTS || c.min_stages < 1 {
        bail!("al massimo {SLOTS} stadi");
    }
    if !cfg.write_stage_count && c.min_stages != c.max_stages {
        bail!("con write_stage_count = false il numero di stadi è fisso: min_stages = max_stages");
    }
    if let Some(hz) = c.polling.iter().find(|&&hz| rate_code(hz).is_none()) {
        bail!("polling {hz} Hz non esiste in questo protocollo");
    }
    if cfg.dpi_led {
        if c.zones != ["dpi"] {
            bail!("con dpi_led la sola zona è \"dpi\"");
        }
        if let Some(e) = c.effects.iter().find(|e| !matches!(e.as_str(), "static" | "breathing" | "off")) {
            bail!("effetto '{e}' non gestito dalla spia DPI");
        }
    } else if !c.zones.is_empty() || !c.effects.is_empty() {
        bail!("zone ed effetti richiedono dpi_led = true");
    }
    Ok(())
}

/// Apre il mouse e restituisce il driver.
pub fn open(api: &HidApi, spec: &DeviceSpec, vid: u16, pid: u16) -> Result<Box<dyn MouseDriver>> {
    let cfg: CompxConfig = spec.section("compx")?;
    let debug = std::env::var_os("MOUSECFG_DEBUG").is_some();
    let mut infos: Vec<_> = api
        .device_list()
        .filter(|d| d.vendor_id() == vid && d.product_id() == pid)
        .filter(|d| d.usage_page() >= 0xFF00)
        .filter(|d| cfg.usage_page == 0 || d.usage_page() == cfg.usage_page)
        .filter(|d| cfg.usage == 0 || d.usage() == cfg.usage)
        .collect();
    infos.sort_by_key(|d| Some(d.interface_number()) != cfg.interface);
    for info in infos {
        let Ok(dev) = info.open_device(api) else { continue };
        if !declares_report(&dev, cfg.transport) {
            continue;
        }
        let mut spec = spec.clone();
        (spec.vid, spec.pid) = (vid, pid);
        let drv = Compx { dev, spec, cfg: cfg.clone(), debug };
        // Prova di sola lettura: l'intestazione deve avere i byte di controllo giusti.
        if drv.read_range(0, 6).is_ok_and(|h| header_ok(&h)) {
            return Ok(Box::new(drv));
        }
    }
    bail!(
        "{} trovato ma non risponde. Se è wireless muovilo per svegliarlo, chiudi il software del produttore e riprova.",
        spec.name
    )
}

/// La collezione deve dichiarare il report 0x08 (feature per Endgame, altrimenti cerca l'item "Report ID 8").
/// Se il descrittore non è leggibile decide la prova di lettura.
fn declares_report(dev: &HidDevice, t: Transport) -> bool {
    let mut buf = [0u8; 4096];
    let Ok(n) = dev.get_report_descriptor(&mut buf) else { return true };
    if n == 0 {
        return true;
    }
    match t {
        Transport::Feature => crate::hid::feature_len(dev, REPORT) == Some(LEN - 1),
        Transport::Output => buf[..n].windows(2).any(|w| w == [0x85, REPORT]),
    }
}

// ---------- funzioni pure ----------

/// Byte di controllo: 0x55 - somma (troncata a 8 bit).
fn checksum(bytes: &[u8]) -> u8 {
    bytes.iter().fold(0x55u8, |a, &b| a.wrapping_sub(b))
}

/// Pacchetto completo. Per una lettura `len` è il numero di byte chiesti e `data` è vuoto.
fn build(cmd: u8, addr: u16, len: usize, data: &[u8]) -> [u8; LEN] {
    let mut b = [0u8; LEN];
    b[0] = REPORT;
    b[1] = cmd;
    b[3..5].copy_from_slice(&addr.to_be_bytes());
    b[5] = len as u8;
    b[6..6 + data.len()].copy_from_slice(data);
    b[16] = checksum(&b[..16]);
    b
}

/// Valore singolo come è salvato nel mouse.
fn scalar(v: u8) -> [u8; 2] {
    [v, checksum(&[v])]
}

/// Valore singolo letto, solo se il suo byte di controllo è giusto.
fn read_scalar(img: &[u8], addr: u16) -> Option<u8> {
    let a = addr as usize;
    let (v, c) = (*img.get(a)?, *img.get(a + 1)?);
    (checksum(&[v]) == c).then_some(v)
}

fn record_ok(rec: &[u8]) -> bool {
    rec.len() == 4 && checksum(&rec[..3]) == rec[3]
}

/// Polling, numero di stadi e stadio attivo devono avere byte di controllo validi.
fn header_ok(img: &[u8]) -> bool {
    [ADDR_POLLING, ADDR_COUNT, ADDR_ACTIVE].iter().all(|&a| read_scalar(img, a).is_some())
}

fn rate_code(hz: u16) -> Option<u8> {
    RATES.iter().find(|r| r.0 == hz).map(|r| r.1)
}

fn rate_hz(code: u8) -> Option<u16> {
    RATES.iter().find(|r| r.1 == code).map(|r| r.0)
}

/// Record DPI con X = Y: indice = DPI/50 - 1 su 10 bit; i 2 bit alti vanno nei bit 2-3 (X) e 6-7 (Y)
/// del terzo byte, esponente 0. (Codifica del sensore 3395 da python-pulsar-mouse-tool; per Lamzu e
/// Endgame i bit alti restano 0 perché i limiti nel TOML stanno sotto 12800.)
fn encode_dpi(dpi: u16) -> [u8; 4] {
    let idx = (dpi / 50).saturating_sub(1).min(1023);
    let (lo, hi) = ((idx & 0xFF) as u8, (idx >> 8) as u8);
    let attr = (hi << 2) | (hi << 6);
    [lo, lo, attr, checksum(&[lo, lo, attr])]
}

/// DPI asse X di un record: (indice + 1) * 50, raddoppiato per ogni bit di esponente (bit 0-1).
fn decode_dpi(rec: &[u8]) -> u16 {
    let attr = rec[2];
    let idx = (((attr >> 2) & 0x03) as u32) << 8 | rec[0] as u32;
    let mut v = (idx + 1) * 50;
    if attr & 0x02 != 0 {
        v *= 2;
    }
    if attr & 0x01 != 0 {
        v *= 2;
    }
    v.min(u16::MAX as u32) as u16
}

/// Elenco dei comandi ammessi: letture nella zona nota, scritture solo dei campi gestiti,
/// con lunghezza giusta, valori sensati e byte di controllo corretti.
fn allowed(cfg: &CompxConfig, caps: &Capabilities, cmd: u8, addr: u16, len: usize, data: &[u8]) -> bool {
    let a = addr as usize;
    match cmd {
        CMD_READ => data.is_empty() && (1..=MAX_DATA).contains(&len) && a + len <= IMAGE_LEN,
        CMD_WRITE => {
            if data.len() != len {
                return false;
            }
            let led = cfg.dpi_led;
            match (addr, data) {
                (ADDR_POLLING, &[v, c]) => rate_hz(v).is_some() && c == checksum(&[v]),
                (ADDR_COUNT, &[v, c]) => {
                    cfg.write_stage_count && (1..=caps.max_stages).contains(&v) && c == checksum(&[v])
                }
                (ADDR_ACTIVE, &[v, c]) => caps.active_stage_settable && v < caps.max_stages && c == checksum(&[v]),
                (ADDR_LED_MODE, &[v, c]) => led && matches!(v, LED_STEADY | LED_BREATHE) && c == checksum(&[v]),
                (ADDR_LED_ON, &[v, c]) => led && v <= 1 && c == checksum(&[v]),
                (ADDR_LED_BRIGHTNESS, &[v, c]) => led && c == checksum(&[v]),
                (_, rec) if rec.len() == 4 && record_ok(rec) => {
                    let slot = |base: u16| addr >= base && addr < base + 4 * SLOTS as u16 && (addr - base) % 4 == 0;
                    slot(ADDR_DPI) || (led && slot(ADDR_COLOR))
                }
                _ => false,
            }
        }
        _ => false,
    }
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect::<Vec<_>>().join(" ")
}

// ---------- driver ----------

pub struct Compx {
    dev: HidDevice,
    spec: DeviceSpec,
    cfg: CompxConfig,
    debug: bool,
}

impl Compx {
    fn log(&self, dir: &str, b: &[u8]) {
        if self.debug {
            eprintln!("{dir} {}", hex(b));
        }
    }

    /// Scarta le risposte vecchie e gli eventi spontanei (es. 0x0A cambio stadio).
    fn drain(&self) {
        let mut buf = [0u8; 64];
        for _ in 0..64 {
            match self.dev.read_timeout(&mut buf, 0) {
                Ok(n) if n > 0 => self.log("(scartato)", &buf[..n.min(LEN)]),
                _ => break,
            }
        }
    }

    /// Invia un comando ammesso e aspetta la sua risposta (stesso comando; per le letture anche
    /// stesso indirizzo e lunghezza). Riprova 3 volte: i wireless addormentati perdono la prima.
    fn transact(&self, cmd: u8, addr: u16, len: usize, data: &[u8]) -> Result<[u8; LEN]> {
        if !allowed(&self.cfg, &self.spec.caps, cmd, addr, len, data) {
            bail!("comando 0x{cmd:02x} all'indirizzo 0x{addr:04x} non ammesso: bloccato per sicurezza");
        }
        let req = build(cmd, addr, len, data);
        for _ in 0..3 {
            self.drain();
            self.log(">", &req);
            match self.cfg.transport {
                Transport::Output => {
                    self.dev.write(&req)?;
                }
                Transport::Feature => self.dev.send_feature_report(&req)?,
            }
            let deadline = Instant::now() + Duration::from_millis(600);
            let mut buf = [0u8; 64];
            while Instant::now() < deadline {
                let n = self.dev.read_timeout(&mut buf, 100)?;
                if n == 0 {
                    continue;
                }
                self.log("<", &buf[..n.min(LEN)]);
                if n < LEN || buf[0] != self.cfg.reply_id || buf[1] != cmd || checksum(&buf[..16]) != buf[16] {
                    continue;
                }
                if cmd == CMD_READ {
                    if buf[3..6] != req[3..6] {
                        continue;
                    }
                    if buf[2] != 0 {
                        bail!("il mouse ha rifiutato la lettura di 0x{addr:04x} (stato {})", buf[2]);
                    }
                }
                // Per le scritture lo stato non è documentato per tutti i marchi: decide la rilettura.
                let mut out = [0u8; LEN];
                out.copy_from_slice(&buf[..LEN]);
                return Ok(out);
            }
        }
        bail!("{} non risponde. Se è wireless muovilo per svegliarlo e riprova.", self.spec.name)
    }

    fn read_range(&self, addr: u16, len: usize) -> Result<Vec<u8>> {
        let mut out = Vec::with_capacity(len);
        while out.len() < len {
            let a = addr + out.len() as u16;
            let n = (len - out.len()).min(MAX_DATA);
            let r = self.transact(CMD_READ, a, n, &[])?;
            out.extend_from_slice(&r[6..6 + n]);
        }
        Ok(out)
    }

    fn read_image(&self) -> Result<Vec<u8>> {
        let img = self.read_range(0, IMAGE_LEN)?;
        if !header_ok(&img) {
            bail!("dati letti dal mouse non validi (byte di controllo errati): non scrivo nulla");
        }
        Ok(img)
    }

    /// Scrive un campo solo se diverso da quello letto, poi lo rilegge per verifica.
    fn write_field(&self, img: &mut [u8], addr: u16, data: &[u8]) -> Result<()> {
        let a = addr as usize;
        if img[a..a + data.len()] == *data {
            return Ok(());
        }
        self.transact(CMD_WRITE, addr, data.len(), data)?;
        let back = self.read_range(addr, data.len())?;
        if back != data {
            bail!(
                "verifica fallita all'indirizzo 0x{addr:04x}: scritto {}, riletto {}",
                hex(data),
                hex(&back)
            );
        }
        img[a..a + data.len()].copy_from_slice(data);
        Ok(())
    }

    fn decode(&self, img: &[u8]) -> MouseState {
        let count = read_scalar(img, ADDR_COUNT).map(|c| c as usize).filter(|c| (1..=SLOTS).contains(c));
        let active = read_scalar(img, ADDR_ACTIVE).map(|a| a as usize);
        let rec = |base: u16, i: usize| &img[base as usize + 4 * i..base as usize + 4 * i + 4];
        let dpi_stages = match (count, active) {
            (Some(n), Some(a)) if a < n => {
                let stages: Vec<u16> = (0..n)
                    .map(|i| {
                        if !record_ok(rec(ADDR_DPI, i)) {
                            self.log("stadio con byte di controllo errato:", rec(ADDR_DPI, i));
                        }
                        decode_dpi(rec(ADDR_DPI, i))
                    })
                    .collect();
                Some(DpiStages { stages, active: a })
            }
            _ => None,
        };
        let mut zones = Vec::new();
        if self.cfg.dpi_led {
            let slot = active.unwrap_or(0).min(SLOTS - 1);
            let c = rec(ADDR_COLOR, slot);
            let color = Rgb(c[0], c[1], c[2]);
            let effect = match (read_scalar(img, ADDR_LED_ON), read_scalar(img, ADDR_LED_MODE)) {
                (Some(0), _) => Some(Effect::Off),
                (Some(_), Some(LED_STEADY)) => Some(Effect::Static { color }),
                (Some(_), Some(LED_BREATHE)) => Some(Effect::Breathing { color }),
                (Some(_), Some(code)) => Some(Effect::Unknown { code }),
                _ => None,
            };
            zones.push(ZoneState {
                zone: "dpi".into(),
                effect,
                brightness: read_scalar(img, ADDR_LED_BRIGHTNESS),
            });
        }
        MouseState {
            dpi: dpi_stages.as_ref().map(|s| s.stages[s.active]),
            dpi_stages,
            polling_hz: read_scalar(img, ADDR_POLLING).and_then(rate_hz),
            zones,
            cached: false,
        }
    }
}

impl MouseDriver for Compx {
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
        Ok(self.decode(&self.read_image()?))
    }

    fn set_dpi_stages(&mut self, stages: &[u16], active: usize) -> Result<()> {
        self.spec.caps.check_dpi_stages(stages, active)?;
        let mut img = self.read_image()?;
        let old_count = read_scalar(&img, ADDR_COUNT).context("numero di stadi illeggibile")? as usize;
        if !self.cfg.write_stage_count && stages.len() != old_count {
            bail!("questo mouse ha {old_count} stadi DPI e il loro numero non si cambia da qui");
        }
        // Prima i valori degli stadi (gli altri slot restano come sono)...
        for (i, &d) in stages.iter().enumerate() {
            self.write_field(&mut img, ADDR_DPI + 4 * i as u16, &encode_dpi(d))?;
        }
        // ...poi numero di stadi e stadio attivo, nell'ordine che non lascia mai l'attivo oltre l'ultimo.
        // Endgame WE: la scrittura dello stadio attivo non è mai stata provata dalle fonti, quindi
        // active_stage_settable = false e lo stadio si cambia col tasto.
        let set_active = self.spec.caps.active_stage_settable;
        let set_count = self.cfg.write_stage_count;
        if active < old_count {
            if set_active {
                self.write_field(&mut img, ADDR_ACTIVE, &scalar(active as u8))?;
            }
            if set_count {
                self.write_field(&mut img, ADDR_COUNT, &scalar(stages.len() as u8))?;
            }
        } else {
            if set_count {
                self.write_field(&mut img, ADDR_COUNT, &scalar(stages.len() as u8))?;
            }
            if set_active {
                self.write_field(&mut img, ADDR_ACTIVE, &scalar(active as u8))?;
            }
        }
        Ok(())
    }

    fn set_polling(&mut self, hz: u16) -> Result<()> {
        self.spec.caps.check_polling(hz)?;
        let code = rate_code(hz).context("polling non gestito")?;
        let mut img = self.read_image()?;
        self.write_field(&mut img, ADDR_POLLING, &scalar(code))
    }

    fn set_lighting(&mut self, zone: &str, effect: &Effect) -> Result<()> {
        self.spec.caps.check_effect(zone, effect)?;
        if !self.cfg.dpi_led {
            bail!("questo mouse non ha luci configurabili");
        }
        let mut img = self.read_image()?;
        let mode = match effect {
            Effect::Off => None,
            Effect::Static { .. } => Some(LED_STEADY),
            Effect::Breathing { .. } => Some(LED_BREATHE),
            e => bail!("effetto '{}' non gestito", e.name()),
        };
        let Some(mode) = mode else {
            // "Spento" = spia disattivata (0x52), l'effetto salvato resta com'è.
            return self.write_field(&mut img, ADDR_LED_ON, &scalar(0));
        };
        // La spia ha un colore per stadio: un colore unico vuol dire lo stesso colore su tutti.
        if let Some(c) = effect.color() {
            for i in 0..self.spec.caps.max_stages as u16 {
                let rgb = [c.0, c.1, c.2];
                let rec = [c.0, c.1, c.2, checksum(&rgb)];
                self.write_field(&mut img, ADDR_COLOR + 4 * i, &rec)?;
            }
        }
        self.write_field(&mut img, ADDR_LED_MODE, &scalar(mode))?;
        self.write_field(&mut img, ADDR_LED_ON, &scalar(1))
    }

    fn set_brightness(&mut self, zone: &str, value: u8) -> Result<()> {
        if !self.cfg.dpi_led || !self.spec.caps.zones.iter().any(|z| z == zone) {
            bail!("zona '{zone}' non esiste");
        }
        let mut img = self.read_image()?;
        self.write_field(&mut img, ADDR_LED_BRIGHTNESS, &scalar(value))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frame_checksums() {
        // Polling 1000 Hz sul profilo attivo (esempio della specifica).
        let f = build(CMD_WRITE, ADDR_POLLING, 2, &scalar(0x01));
        assert_eq!(f, [0x08, 0x07, 0, 0, 0, 0x02, 0x01, 0x54, 0, 0, 0, 0, 0, 0, 0, 0, 0xEF]);
        // Lettura di 2 byte all'indirizzo 0.
        let f = build(CMD_READ, 0, 2, &[]);
        assert_eq!(f[16], 0x43);
        // Invariante: somma dei 17 byte = 0x55.
        assert_eq!(f.iter().fold(0u8, |a, &b| a.wrapping_add(b)), 0x55);
        // Richiesta batteria Lamzu "08 04 00..00 49".
        let mut b = [0u8; 16];
        (b[0], b[1]) = (0x08, 0x04);
        assert_eq!(checksum(&b), 0x49);
    }

    #[test]
    fn dpi_encoding_3395() {
        assert_eq!(encode_dpi(400)[..3], [0x07, 0x07, 0x00]);
        assert_eq!(encode_dpi(12800)[..3], [0xFF, 0xFF, 0x00]);
        assert_eq!(encode_dpi(12850)[..3], [0x00, 0x00, 0x44]);
        assert_eq!(encode_dpi(26000)[..3], [0x07, 0x07, 0x88]);
        // Endgame XM2we: 400 cpi = 07 07 00 47.
        assert_eq!(encode_dpi(400), [0x07, 0x07, 0x00, 0x47]);
        for d in (50..=26000).step_by(50) {
            assert_eq!(decode_dpi(&encode_dpi(d)), d);
            assert!(record_ok(&encode_dpi(d)));
        }
        // Esponente 1: passi da 100 (3395 sopra 26000, Endgame sopra 10000).
        assert_eq!(decode_dpi(&[0x64, 0x64, 0x11, 0]), 10100);
    }

    #[test]
    fn scalars_and_polling() {
        assert_eq!(scalar(0x01), [0x01, 0x54]);
        assert_eq!(read_scalar(&[0x08, 0x4D], 0), Some(0x08));
        assert_eq!(read_scalar(&[0x08, 0x4C], 0), None);
        assert_eq!(rate_hz(0x08), Some(125));
        assert_eq!(rate_code(4000), Some(0x20));
        // Intestazione tipica OP1we: polling 1000, 4 stadi, attivo 2.
        assert!(header_ok(&[0x01, 0x54, 0x04, 0x51, 0x02, 0x53]));
    }

    fn caps(active: bool) -> Capabilities {
        Capabilities {
            dpi_min: 50,
            dpi_max: 10000,
            dpi_step: 50,
            dpi_values: vec![],
            min_stages: 4,
            max_stages: 4,
            active_stage_settable: active,
            polling: vec![125, 250, 500, 1000],
            zones: vec![],
            effects: vec![],
        }
    }

    #[test]
    fn allowlist() {
        let endgame = CompxConfig {
            transport: Transport::Feature,
            reply_id: 9,
            usage_page: 0xFF02,
            usage: 2,
            interface: None,
            write_stage_count: false,
            dpi_led: false,
        };
        let c = caps(false);
        let ok = |cmd, addr, len, data: &[u8]| allowed(&endgame, &c, cmd, addr, len, data);
        assert!(ok(CMD_READ, 0x00, 10, &[]));
        assert!(!ok(CMD_READ, 0x50, 10, &[]));
        assert!(ok(CMD_WRITE, ADDR_POLLING, 2, &scalar(0x01)));
        assert!(!ok(CMD_WRITE, ADDR_POLLING, 2, &scalar(0x03)));
        assert!(!ok(CMD_WRITE, ADDR_POLLING, 2, &[0x01, 0x00]));
        assert!(ok(CMD_WRITE, 0x0C, 4, &encode_dpi(400)));
        assert!(ok(CMD_WRITE, 0x28, 4, &encode_dpi(400)));
        assert!(!ok(CMD_WRITE, 0x0D, 4, &encode_dpi(400)));
        assert!(!ok(CMD_WRITE, 0x2C, 4, &encode_dpi(400)));
        // Endgame: niente numero di stadi, stadio attivo, luci, debounce (0xA9).
        assert!(!ok(CMD_WRITE, ADDR_COUNT, 2, &scalar(4)));
        assert!(!ok(CMD_WRITE, ADDR_ACTIVE, 2, &scalar(1)));
        assert!(!ok(CMD_WRITE, ADDR_LED_ON, 2, &scalar(1)));
        assert!(!ok(CMD_WRITE, 0xA9, 2, &scalar(2)));
        // Reset, cambio profilo, 0x0E (bootloader del ricevitore WE), firmware: mai.
        for cmd in [0x01, 0x02, 0x03, 0x09, 0x0D, 0x0E, 0x0F, 0x10, 0x12] {
            assert!(!ok(cmd, 0, 0, &[]));
        }
        let pulsar = CompxConfig { transport: Transport::Output, reply_id: 8, write_stage_count: true, dpi_led: true, ..endgame };
        let c = caps(true);
        let ok = |cmd, addr, len, data: &[u8]| allowed(&pulsar, &c, cmd, addr, len, data);
        assert!(ok(CMD_WRITE, ADDR_COUNT, 2, &scalar(4)));
        assert!(!ok(CMD_WRITE, ADDR_COUNT, 2, &scalar(5)));
        assert!(ok(CMD_WRITE, ADDR_ACTIVE, 2, &scalar(1)));
        assert!(ok(CMD_WRITE, ADDR_LED_MODE, 2, &scalar(LED_BREATHE)));
        assert!(!ok(CMD_WRITE, ADDR_LED_MODE, 2, &scalar(0)));
        assert!(ok(CMD_WRITE, 0x2C, 4, &[1, 2, 3, checksum(&[1, 2, 3])]));
    }
}
