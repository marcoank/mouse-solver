//! Driver Endgame Gear: XM2w 4k / OP1w 4k (v1 e v2, via cavo o ricevitore) e OP1 8k v2.
//! Protocollo reimplementato leggendo vole (MIT), xm2ctl (MIT) e OP1-WebAPP (MIT) per i 4k,
//! egg-mouse (Apache-2.0) per l'8k: nessun codice copiato.
//!
//! Tutto passa da due feature report sulla collezione 0xFF01/0x0002:
//! - 0xA1 (64 byte con l'ID): comandi e risposte, stato in [1], dati da [16];
//! - 0xA0 (1041 byte con l'ID): il blocco di configurazione da 1024 byte, da [16].
//!
//! Lettura: A1 12, poi GET 0xA0. Non c'è checksum e non c'è un comando di salvataggio: le
//! scritture restano nel mouse anche staccandolo (A1 13 è il RESET DI FABBRICA: mai inviato).
//!
//! Famiglia "4k" (wireless): si scrive per blocchi, 0x14 (stadi DPI) e 0x15 (polling), sempre
//! costruiti dal blocco appena letto. Il ricevitore 0x1970 è lo stesso per tutti i modelli: il
//! mouse collegato si scopre con A1 0E, e senza risposta non si scrive niente (v1 e v2 hanno
//! campi diversi negli stessi blocchi).
//! Famiglia "8k" (cablata): si riscrive l'intero blocco con A0 11.
//!
//! Esclusi di proposito:
//! - XM1 (VID 0x22D4): un solo test remoto, persistenza mai verificata, una scrittura sbagliata
//!   ha già azzerato i DPI di un utente;
//! - XM1r / OP1 / OP1 RGB: protocollo non documentato in nessuna fonte;
//! - XM2 8k, OP1 8k, OP1 8k Purple Frost, XM2 8k v2: stessa struttura dell'OP1 8k v2 solo dedotta,
//!   nessuna prova sul mouse;
//! - serie WE (XM2we, OP1we): protocollo CompX, driver "compx";
//! - luci: il LED sotto il mouse ha colori fissi per stadio, nessuna fonte documenta una scrittura.

use anyhow::{bail, Context, Result};
use hidapi::{HidApi, HidDevice};
use mousecfg_core::*;
use serde::Deserialize;
use std::thread::sleep;
use std::time::{Duration, Instant};

const VID: u16 = 0x3367;
const PID_DONGLE: u16 = 0x1970;
/// PID di bootloader/aggiornamento firmware: non si aprono mai.
const BOOTLOADER_PIDS: [u16; 6] = [0x1967, 0x1969, 0x1971, 0x1977, 0x1981, 0x1983];

const USAGE_PAGE: u16 = 0xFF01;
const USAGE: u16 = 0x0002;

const REPORT_CMD: u8 = 0xA1;
const REPORT_BLOB: u8 = 0xA0;
const CMD_LEN: usize = 64;
const BLOB_REPORT_LEN: usize = 1041;
const BLOB_LEN: usize = 1024;
const PAYLOAD: usize = 16;

// Comandi (byte [1]).
const CMD_MOUSE_INFO: u8 = 0x0E;
const CMD_PROBE: u8 = 0x0F;
const CMD_STORE_8K: u8 = 0x11;
const CMD_READ: u8 = 0x12;
const CMD_SENSOR: u8 = 0x14;
const CMD_POWER: u8 = 0x15;

// Destinatario (byte [2]).
const TARGET_NONE: u8 = 0x00;
const TARGET_DONGLE: u8 = 0x01;
const TARGET_MOUSE: u8 = 0x0F;

// Stato nella risposta (byte [1]).
const ST_OK: u8 = 0x01;
const ST_BUSY: u8 = 0x03;
const ST_WRONG_LINK: u8 = 0x07;
const ST_PENDING: u8 = 0x08;
const ST_UNREACHABLE: u8 = 0x11;

// Offset nel blocco da 1024 byte.
const B_ANGLE: usize = 0x01;
const B_DEEP_SLEEP: usize = 0x03;
const B_POWER_SAVING: usize = 0x04;
const B_POLLING: usize = 0x05;
const B_FLAGS: usize = 0x06;
const B_ACTIVE: usize = 0x0D;
const B_COUNT: usize = 0x0E;
const B_MOTION_SYNC: usize = 0x0C;
const B_CPI: usize = 0x23;
const CPI_STRIDE: usize = 5;
const STAGES: usize = 4;
const B_CLICK_FILTERS: [usize; 5] = [0x3D, 0x44, 0x4B, 0x52, 0x59];
const B_GLASS: usize = 0x6F;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
pub enum Family {
    /// XM2w 4k / OP1w 4k (v1 e v2): scrittura a blocchi 0x14/0x15.
    #[serde(rename = "4k")]
    FourK,
    /// OP1 8k v2: scrittura dell'intero blocco A0 11.
    #[serde(rename = "8k")]
    EightK,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EndgameConfig {
    /// "4k" o "8k".
    pub family: Family,
}

struct Model {
    pid: u16,
    name: &'static str,
    v2: bool,
    family: Family,
}

/// Modelli supportati, per PID del mouse (via cavo, o riportato da A1 0E sul ricevitore).
const MODELS: [Model; 5] = [
    Model { pid: 0x1968, name: "Endgame Gear XM2w 4k", v2: false, family: Family::FourK },
    Model { pid: 0x1972, name: "Endgame Gear OP1w 4k", v2: false, family: Family::FourK },
    Model { pid: 0x1982, name: "Endgame Gear XM2w 4k v2", v2: true, family: Family::FourK },
    Model { pid: 0x1984, name: "Endgame Gear OP1w 4k v2", v2: true, family: Family::FourK },
    Model { pid: 0x1978, name: "Endgame Gear OP1 8k v2", v2: true, family: Family::EightK },
];

fn model(pid: u16) -> Option<&'static Model> {
    MODELS.iter().find(|m| m.pid == pid)
}

fn known_pid(family: Family, pid: u16) -> bool {
    (family == Family::FourK && pid == PID_DONGLE) || model(pid).is_some_and(|m| m.family == family)
}

fn debug(dir: &str, b: &[u8]) {
    if mousecfg_core::trace::on() {
        mousecfg_core::trace!("{dir} {}", b.iter().take(96).map(|x| format!("{x:02x}")).collect::<Vec<_>>().join(" "));
    }
}

/// Elenco dei pacchetti che possono arrivare al mouse. Tutto il resto (reset di fabbrica A1 13,
/// abbinamento 0x70-0x72, tasti 0x16, bootloader A1 3A o 0xA0 sui 4k...) è bloccato qui.
fn allowed(family: Family, wireless: bool, buf: &[u8]) -> bool {
    match (buf.first(), buf.len()) {
        (Some(&REPORT_CMD), CMD_LEN) => match family {
            Family::FourK => match buf[1] {
                // Mai la sonda via cavo: il mouse risponde 0x07 e scarta la scrittura successiva.
                CMD_PROBE => wireless && matches!(buf[2], TARGET_DONGLE | TARGET_MOUSE),
                CMD_MOUSE_INFO | CMD_READ => true,
                CMD_SENSOR | CMD_POWER => buf[2] == TARGET_MOUSE,
                _ => false,
            },
            Family::EightK => buf[1] == CMD_READ,
        },
        // Sui 4k un SET 0xA0 può far entrare il mouse nel bootloader: solo l'8k lo usa, e solo con 0x11.
        (Some(&REPORT_BLOB), BLOB_REPORT_LEN) => family == Family::EightK && buf[1] == CMD_STORE_8K,
        _ => false,
    }
}

/// Il blocco da 1024 byte dalla risposta a GET 0xA0. Alcuni sistemi lo restituiscono con 0xA1
/// nel posto dell'ID e lo stato spostato di uno: si accettano entrambe le forme.
/// In caso di errore restituisce lo stato letto.
fn parse_blob_reply(buf: &[u8]) -> std::result::Result<Vec<u8>, u8> {
    if buf.len() >= PAYLOAD + BLOB_LEN && buf[1] == ST_OK {
        return Ok(buf[PAYLOAD..PAYLOAD + BLOB_LEN].to_vec());
    }
    if buf.len() > PAYLOAD + BLOB_LEN && buf[1] == REPORT_CMD && buf[2] == ST_OK {
        return Ok(buf[PAYLOAD + 1..PAYLOAD + 1 + BLOB_LEN].to_vec());
    }
    match buf.get(1) {
        Some(&REPORT_CMD) => Err(buf.get(2).copied().unwrap_or(0)),
        Some(&s) => Err(s),
        None => Err(0),
    }
}

fn polling_hz(family: Family, code: u8) -> Option<u16> {
    match family {
        Family::FourK => match code {
            0x02 => Some(4000),
            0x04 => Some(2000),
            // 0x80 = "1000 Hz risparmio energetico" (solo v2).
            0x08 | 0x80 => Some(1000),
            // "125 Hz Office Mode" (solo v2).
            0x40 => Some(125),
            _ => None,
        },
        // 8000 / Hz.
        Family::EightK => match code {
            0x01 | 0x02 | 0x04 | 0x08 | 0x10 | 0x20 | 0x40 => Some(8000 / code as u16),
            _ => None,
        },
    }
}

fn polling_code(family: Family, v2: bool, hz: u16) -> Option<u8> {
    match family {
        Family::FourK => match hz {
            4000 => Some(0x02),
            2000 => Some(0x04),
            1000 => Some(0x08),
            125 if v2 => Some(0x40),
            _ => None,
        },
        Family::EightK => match hz {
            125 | 250 | 500 | 1000 | 2000 | 4000 | 8000 => Some((8000 / hz) as u8),
            _ => None,
        },
    }
}

/// Record di uno stadio: [X≠Y][X u16 LE][Y u16 LE], DPI senza scala. Si scrive sempre Y = X.
fn encode_stage(dpi: u16) -> [u8; 5] {
    let [lo, hi] = dpi.to_le_bytes();
    [0, lo, hi, lo, hi]
}

fn stage_dpi(rec: &[u8]) -> u16 {
    u16::from_le_bytes([rec[1], rec[2]])
}

/// Controlla che il blocco letto sia credibile: un ricevitore bloccato restituisce la vecchia
/// risposta da 64 byte al posto della configurazione, e scriverla rovinerebbe il mouse.
fn check_blob(blob: &[u8], family: Family) -> Result<()> {
    if blob.len() != BLOB_LEN {
        bail!(mousecfg_core::tf!("blocco di {} byte invece di {BLOB_LEN}", "block of {} bytes instead of {BLOB_LEN}", blob.len()));
    }
    let count = blob[B_COUNT] as usize;
    if !(1..=STAGES).contains(&count) || blob[B_ACTIVE] as usize >= STAGES {
        bail!(mousecfg_core::tf!("numero di stadi ({count}) o stadio attivo ({}) impossibile", "impossible stage count ({count}) or active stage ({})", blob[B_ACTIVE]));
    }
    for i in 0..count {
        let rec = &blob[B_CPI + i * CPI_STRIDE..B_CPI + (i + 1) * CPI_STRIDE];
        let (x, y) = (stage_dpi(rec), u16::from_le_bytes([rec[3], rec[4]]));
        if rec[0] > 1 || !(10..=30000).contains(&x) || (rec[0] == 0 && x != y) {
            bail!(mousecfg_core::tf!("stadio {} non valido ({:02x?})", "invalid stage {} ({:02x?})", i + 1, rec));
        }
    }
    if polling_hz(family, blob[B_POLLING]).is_none() {
        bail!(mousecfg_core::tf!("valore di polling sconosciuto 0x{:02x}", "unknown polling value 0x{:02x}", blob[B_POLLING]));
    }
    Ok(())
}

/// Sicurezza: tra il blocco letto e quello da scrivere cambiano solo i byte previsti.
fn ensure_only(old: &[u8], new: &[u8], allowed: &[(usize, usize)]) -> Result<()> {
    for i in 0..old.len().min(new.len()) {
        if old[i] != new[i] && !allowed.iter().any(|&(a, b)| (a..b).contains(&i)) {
            bail!(mousecfg_core::tf!("errore interno: il byte 0x{i:02x} non doveva cambiare, scrittura annullata", "internal error: byte 0x{i:02x} should not have changed, write cancelled"));
        }
    }
    Ok(())
}

/// Imposta numero di stadi, stadio attivo e DPI. Gli stadi che non cambiano DPI restano com'erano
/// (anche con X e Y separati); quelli oltre il numero di stadi restano intatti.
fn apply_stages(blob: &mut [u8], stages: &[u16], active: u8) {
    blob[B_COUNT] = stages.len() as u8;
    blob[B_ACTIVE] = active;
    for (i, &d) in stages.iter().enumerate() {
        let r = B_CPI + i * CPI_STRIDE;
        if stage_dpi(&blob[r..r + CPI_STRIDE]) != d {
            blob[r..r + CPI_STRIDE].copy_from_slice(&encode_stage(d));
        }
    }
}

/// Dati del comando 0x14 (28 byte), presi dal blocco.
fn sensor_payload(blob: &[u8], v2: bool) -> [u8; 28] {
    let mut p = [0u8; 28];
    p[..5].copy_from_slice(&blob[0x07..0x0C]); // ?, LED al sollevamento, LOD, angle snapping, ripple
    // +5 = correzione dell'angolo: esiste solo sui v2, i v1 vogliono 0 (come il programma ufficiale).
    p[5] = if v2 { blob[B_ANGLE] } else { 0 };
    p[6] = blob[B_COUNT];
    p[7] = blob[B_ACTIVE];
    p[8..].copy_from_slice(&blob[B_CPI..B_CPI + STAGES * CPI_STRIDE]);
    p
}

/// Dati del comando 0x15: 10 byte sui v1, 11 sui v2 (l'ultimo è la modalità vetro; mandarne 10
/// a un v2 la spegnerebbe). La lunghezza dichiarata resta 0x0A in entrambi i casi.
fn power_payload(blob: &[u8], v2: bool) -> Vec<u8> {
    let mut p = vec![blob[B_MOTION_SYNC], blob[B_POLLING], blob[B_FLAGS], blob[B_POWER_SAVING]];
    p.extend(B_CLICK_FILTERS.iter().map(|&o| blob[o]));
    p.push(blob[B_DEEP_SLEEP]);
    if v2 {
        p.push(blob[B_GLASS]);
    }
    p
}

fn decode_state(blob: &[u8], family: Family) -> MouseState {
    let count = (blob[B_COUNT] as usize).clamp(1, STAGES);
    let stages: Vec<u16> = (0..count).map(|i| stage_dpi(&blob[B_CPI + i * CPI_STRIDE..])).collect();
    let active = (blob[B_ACTIVE] as usize).min(count - 1);
    MouseState {
        dpi: stages.get(active).copied(),
        dpi_stages: Some(DpiStages { stages, active }),
        polling_hz: polling_hz(family, blob[B_POLLING]),
        zones: Vec::new(),
        cached: false,
    }
}

/// Sopra 10000 DPI il programma ufficiale dei v2 usa solo multipli di 50.
fn check_grid(stages: &[u16], v2: bool) -> Result<()> {
    if let Some(d) = stages.iter().find(|&&d| v2 && d > 10000 && d % 50 != 0) {
        bail!(mousecfg_core::tf!("sopra 10000 DPI servono multipli di 50 ({d} non va bene)", "above 10000 DPI only multiples of 50 are allowed ({d} is not)"));
    }
    Ok(())
}

/// Capacità dei 4k secondo la generazione (vale anche per il ricevitore, dopo A1 0E).
fn caps_4k(caps: &mut Capabilities, v2: bool) {
    caps.dpi_min = if v2 { 10 } else { 50 };
    caps.dpi_max = if v2 { 30000 } else { 26000 };
    caps.dpi_step = caps.dpi_min;
    caps.dpi_values.clear();
    caps.min_stages = 1;
    caps.max_stages = STAGES as u8;
    // I v1 ignorano lo stadio attivo scritto via software: si cambia solo col tasto.
    caps.active_stage_settable = v2;
    caps.polling = if v2 { vec![125, 1000, 2000, 4000] } else { vec![1000, 2000, 4000] };
    caps.zones.clear();
    caps.effects.clear();
}

pub struct Endgame {
    dev: HidDevice,
    spec: DeviceSpec,
    family: Family,
    wireless: bool,
    v2: bool,
    firmware: Option<String>,
    /// Dopo un comando fallito il mouse non risponde finché non passa una sonda.
    needs_probe: bool,
}

impl Endgame {
    fn set_feature(&self, buf: &[u8]) -> Result<()> {
        if !allowed(self.family, self.wireless, buf) {
            bail!(mousecfg_core::tf!("comando Endgame Gear {:02x} {:02x} bloccato", "Endgame Gear command {:02x} {:02x} blocked", buf.first().unwrap_or(&0), buf.get(1).unwrap_or(&0)));
        }
        debug(">", buf);
        self.dev.send_feature_report(buf)?;
        Ok(())
    }

    fn get_feature(&self, id: u8, len: usize) -> Result<Vec<u8>> {
        let mut buf = vec![0u8; len];
        buf[0] = id;
        let n = self.dev.get_feature_report(&mut buf)?;
        debug("<", &buf[..n.min(len)]);
        if n < 2 {
            bail!(mousecfg_core::t!("risposta vuota dal mouse", "empty response from the mouse"));
        }
        Ok(buf)
    }

    fn unreachable(&self, status: u8) -> anyhow::Error {
        match self.wireless {
            true => anyhow::anyhow!(
                mousecfg_core::tf!("il mouse non risponde (stato 0x{status:02x}): probabilmente dorme. Muovilo, aspetta qualche secondo e riprova. Se continua, scollega e ricollega il ricevitore.", "the mouse is not responding (status 0x{status:02x}): it is probably asleep. Move it, wait a few seconds and try again. If it keeps happening, unplug and replug the receiver."
            )),
            false => anyhow::anyhow!(mousecfg_core::tf!("il mouse non risponde (stato 0x{status:02x}). Chiudi il software Endgame Gear e riprova.", "the mouse is not responding (status 0x{status:02x}). Close the Endgame Gear software and try again.")),
        }
    }

    /// Legge la risposta da 64 byte. 0x08 = non ancora pronta (si riprova fino a 3 s),
    /// 0x03 = occupato (si rilegge fino a 3 volte, +200 ms ogni volta).
    fn await_ack(&self) -> Result<Vec<u8>> {
        let deadline = Instant::now() + Duration::from_secs(3);
        let mut busy = 0u64;
        loop {
            let r = self.get_feature(REPORT_CMD, CMD_LEN)?;
            match r[1] {
                ST_OK => return Ok(r),
                ST_PENDING if Instant::now() < deadline => sleep(Duration::from_millis(100)),
                ST_BUSY if busy < 3 => {
                    busy += 1;
                    sleep(Duration::from_millis(200 * busy));
                }
                ST_WRONG_LINK => bail!(mousecfg_core::t!("comando non valido con questo collegamento (stato 0x07)", "command not valid with this connection (status 0x07)")),
                s @ (ST_BUSY | ST_PENDING | ST_UNREACHABLE) => return Err(self.unreachable(s)),
                s => bail!(mousecfg_core::tf!("il mouse ha risposto con lo stato 0x{s:02x}", "the mouse replied with status 0x{s:02x}")),
            }
        }
    }

    /// Un comando sul report 0xA1 e la sua risposta (solo 4k).
    fn exchange(&self, cmd: u8, target: u8, payload: &[u8], declared: u8, settle_ms: u64) -> Result<Vec<u8>> {
        if payload.len() > CMD_LEN - PAYLOAD {
            bail!(mousecfg_core::t!("dati troppo lunghi per il report 0xA1", "data too long for report 0xA1"));
        }
        let mut buf = [0u8; CMD_LEN];
        buf[0] = REPORT_CMD;
        buf[1] = cmd;
        buf[2] = target;
        buf[3] = declared;
        buf[PAYLOAD..PAYLOAD + payload.len()].copy_from_slice(payload);
        self.set_feature(&buf)?;
        sleep(Duration::from_millis(settle_ms));
        self.await_ack()
    }

    fn query_settle(&self) -> u64 {
        if self.wireless {
            300
        } else {
            150
        }
    }

    /// Sul ricevitore il programma ufficiale manda A1 0F 01 (o A1 0F 0F) prima di ogni comando;
    /// via cavo, dopo un errore, A1 0E fa da sonda.
    fn prepare(&mut self) -> Result<()> {
        if self.family != Family::FourK {
            return Ok(());
        }
        let settle = self.query_settle();
        if self.wireless {
            if self.exchange(CMD_PROBE, TARGET_DONGLE, &[], 0, settle).is_err() {
                self.exchange(CMD_PROBE, TARGET_MOUSE, &[], 0, settle)
                    .context(mousecfg_core::t!("il ricevitore non risponde: scollegalo, ricollegalo e riprova", "the receiver is not responding: unplug it, plug it back in and try again"))?;
            }
        } else if self.needs_probe {
            self.exchange(CMD_MOUSE_INFO, TARGET_NONE, &[], 0, settle)?;
        }
        Ok(())
    }

    fn command(&mut self, cmd: u8, target: u8, payload: &[u8], declared: u8, settle_ms: u64) -> Result<Vec<u8>> {
        self.prepare()?;
        self.needs_probe = true;
        let r = self.exchange(cmd, target, payload, declared, settle_ms)?;
        self.needs_probe = false;
        Ok(r)
    }

    /// A1 12, poi GET 0xA0: il blocco da 1024 byte, controllato.
    fn read_blob(&mut self) -> Result<Vec<u8>> {
        self.prepare()?;
        self.needs_probe = true;
        let mut req = [0u8; CMD_LEN];
        req[0] = REPORT_CMD;
        req[1] = CMD_READ;
        self.set_feature(&req)?;
        sleep(Duration::from_millis(if self.family == Family::FourK { 400 } else { 80 }));
        let mut tries = 0;
        let blob = loop {
            let buf = self.get_feature(REPORT_BLOB, BLOB_REPORT_LEN)?;
            match parse_blob_reply(&buf) {
                Ok(b) => break b,
                Err(s) if matches!(s, ST_BUSY | ST_PENDING) && tries < 5 => {
                    tries += 1;
                    sleep(Duration::from_millis(200));
                }
                Err(s) if matches!(s, ST_BUSY | ST_PENDING | ST_UNREACHABLE) => return Err(self.unreachable(s)),
                Err(s) => bail!(mousecfg_core::tf!("lettura della configurazione fallita (stato 0x{s:02x})", "reading the configuration failed (status 0x{s:02x})")),
            }
        };
        check_blob(&blob, self.family).context(mousecfg_core::t!("configurazione letta non credibile: non scrivo niente", "the configuration read back is not plausible: nothing will be written"))?;
        self.needs_probe = false;
        Ok(blob)
    }

    fn init(&mut self) -> Result<()> {
        if self.family == Family::EightK {
            self.read_blob()?;
            self.v2 = true;
            return Ok(());
        }
        let r = self.command(CMD_MOUSE_INFO, TARGET_NONE, &[], 0, self.query_settle())?;
        let rvid = u16::from_le_bytes([r[16], r[17]]);
        let rpid = u16::from_le_bytes([r[18], r[19]]);
        self.firmware = Some(format!("{:x}.{:02x}", r[22], r[23]));
        let pid = if self.wireless {
            if rvid != VID {
                bail!(mousecfg_core::tf!("il ricevitore riporta un mouse sconosciuto ({rvid:04x}:{rpid:04x})", "the receiver reports an unknown mouse ({rvid:04x}:{rpid:04x})"));
            }
            rpid
        } else {
            if rvid == VID && model(rpid).is_some() && rpid != self.spec.pid {
                bail!(mousecfg_core::tf!("il mouse dice di essere {rpid:04x} ma è collegato come {:04x}", "the mouse reports {rpid:04x} but is connected as {:04x}", self.spec.pid));
            }
            self.spec.pid
        };
        let m = model(pid)
            .filter(|m| m.family == Family::FourK)
            .with_context(|| mousecfg_core::tf!("modello {pid:04x} sul ricevitore non supportato", "model {pid:04x} on the receiver is not supported"))?;
        self.v2 = m.v2;
        if self.wireless {
            self.spec.name = mousecfg_core::tf!("{} (ricevitore)", "{} (receiver)", m.name);
        }
        caps_4k(&mut self.spec.caps, m.v2);
        Ok(())
    }

    /// 4k: blocco 0x14 o 0x15, risposta attesa a lungo (il programma ufficiale aspetta 500 ms).
    fn write_block_4k(&mut self, cmd: u8, payload: &[u8], declared: u8) -> Result<()> {
        let settle = if self.wireless { 1000 } else { 500 };
        self.command(cmd, TARGET_MOUSE, payload, declared, settle)?;
        Ok(())
    }

    /// 8k: riscrive l'intero blocco (A0 11) e lo rilegge tutto per verifica.
    fn store_8k(&mut self, old: &[u8], mut new: Vec<u8>, allowed_ranges: &[(usize, usize)]) -> Result<()> {
        // Come il programma ufficiale (73 scritture catturate su 73): i byte 0x01-0x04 vanno a zero.
        // Il mouse li riporta 80 00 00 00, ma nessun programma li ha mai rimandati così.
        new[0x01..0x05].fill(0);
        let mut ranges = vec![(0x01, 0x05)];
        ranges.extend_from_slice(allowed_ranges);
        ensure_only(old, &new, &ranges)?;
        check_blob(&new, self.family)?;
        let mut buf = vec![0u8; BLOB_REPORT_LEN];
        buf[0] = REPORT_BLOB;
        buf[1] = CMD_STORE_8K;
        buf[PAYLOAD..PAYLOAD + BLOB_LEN].copy_from_slice(&new);
        // Il programma ufficiale ripete la scrittura fino a 3 volte (300/350/400 ms).
        let mut acked = false;
        for wait in [300, 350, 400] {
            self.set_feature(&buf)?;
            sleep(Duration::from_millis(wait));
            if self.get_feature(REPORT_CMD, CMD_LEN)?[1] == ST_OK {
                acked = true;
                break;
            }
        }
        if !acked {
            bail!(mousecfg_core::t!("il mouse non ha confermato la scrittura", "the mouse did not confirm the write"));
        }
        let back = self.read_blob()?;
        let diff: Vec<String> =
            (0..BLOB_LEN).filter(|&i| !(0x01..0x05).contains(&i) && back[i] != new[i]).map(|i| format!("0x{i:03x}")).collect();
        if !diff.is_empty() {
            bail!(mousecfg_core::tf!("verifica fallita: il mouse ha salvato valori diversi (byte {})", "verification failed: the mouse saved different values (bytes {})", diff.join(", ")));
        }
        Ok(())
    }

    /// 4k: rilegge e confronta i byte indicati.
    fn verify_4k(&mut self, new: &[u8], fields: &[usize]) -> Result<()> {
        let back = self.read_blob()?;
        if let Some(&i) = fields.iter().find(|&&i| back[i] != new[i]) {
            bail!(mousecfg_core::tf!("verifica fallita: il mouse ha salvato un valore diverso (byte 0x{i:02x}: {:02x} invece di {:02x})", "verification failed: the mouse saved a different value (byte 0x{i:02x}: {:02x} instead of {:02x})", back[i], new[i]));
        }
        Ok(())
    }
}

impl MouseDriver for Endgame {
    fn spec(&self) -> &DeviceSpec {
        &self.spec
    }

    fn identify(&mut self) -> Result<DeviceInfo> {
        Ok(DeviceInfo {
            name: self.spec.name.clone(),
            vid: self.spec.vid,
            pid: self.spec.pid,
            status: self.spec.status,
            firmware: self.firmware.clone(),
            serial: None,
        })
    }

    fn read_state(&mut self) -> Result<MouseState> {
        let blob = self.read_blob()?;
        Ok(decode_state(&blob, self.family))
    }

    fn set_dpi_stages(&mut self, stages: &[u16], active: usize) -> Result<()> {
        self.spec.caps.check_dpi_stages(stages, active)?;
        check_grid(stages, self.v2)?;
        let old = self.read_blob()?;
        let mut new = old.clone();
        // v1: lo stadio attivo si rimanda com'era (il mouse ignorerebbe comunque il cambio).
        let active_byte = if self.spec.caps.active_stage_settable { active as u8 } else { old[B_ACTIVE] };
        apply_stages(&mut new, stages, active_byte);
        let ranges = [(B_ACTIVE, B_COUNT + 1), (B_CPI, B_CPI + STAGES * CPI_STRIDE)];
        ensure_only(&old, &new, &ranges)?;
        check_blob(&new, self.family)?;
        match self.family {
            Family::FourK => {
                self.write_block_4k(CMD_SENSOR, &sensor_payload(&new, self.v2), 0x1C)?;
                let mut fields: Vec<usize> = (B_CPI..B_CPI + STAGES * CPI_STRIDE).collect();
                fields.push(B_COUNT);
                if self.spec.caps.active_stage_settable {
                    fields.push(B_ACTIVE);
                }
                self.verify_4k(&new, &fields)
            }
            Family::EightK => self.store_8k(&old, new, &ranges),
        }
    }

    fn set_polling(&mut self, hz: u16) -> Result<()> {
        self.spec.caps.check_polling(hz)?;
        let code = polling_code(self.family, self.v2, hz).context(mousecfg_core::t!("polling non gestito", "polling rate not supported"))?;
        let old = self.read_blob()?;
        // Già impostato (anche come "1000 Hz risparmio energetico"): non si riscrive.
        if polling_hz(self.family, old[B_POLLING]) == Some(hz) {
            return Ok(());
        }
        let mut new = old.clone();
        new[B_POLLING] = code;
        match self.family {
            Family::FourK => {
                // Via cavo il mouse va comunque a 1000 Hz: il valore salvato vale per il ricevitore.
                self.write_block_4k(CMD_POWER, &power_payload(&new, self.v2), 0x0A)?;
                self.verify_4k(&new, &[B_POLLING])
            }
            Family::EightK => self.store_8k(&old, new, &[(B_POLLING, B_POLLING + 1)]),
        }
    }

    fn set_lighting(&mut self, _: &str, _: &Effect) -> Result<()> {
        bail!(mousecfg_core::t!("questo mouse non ha luci configurabili: il LED sotto il mouse ha colori fissi", "this mouse has no configurable lighting: the LED under the mouse has fixed colors"))
    }

    fn set_brightness(&mut self, _: &str, _: u8) -> Result<()> {
        bail!(mousecfg_core::t!("questo mouse non ha luci configurabili", "this mouse has no configurable lighting"))
    }
}

/// Apre il mouse e restituisce il driver.
pub fn open(api: &HidApi, spec: &DeviceSpec, vid: u16, pid: u16) -> Result<Box<dyn MouseDriver>> {
    let cfg: EndgameConfig = spec.section("endgame")?;
    if BOOTLOADER_PIDS.contains(&pid) {
        bail!(mousecfg_core::tf!("{} è in modalità aggiornamento firmware: scollegalo e ricollegalo", "{} is in firmware update mode: unplug it and plug it back in", spec.name));
    }
    if vid != VID || !known_pid(cfg.family, pid) {
        bail!(mousecfg_core::tf!("{vid:04x}:{pid:04x} non è un modello Endgame Gear supportato da questo driver", "{vid:04x}:{pid:04x} is not an Endgame Gear model supported by this driver"));
    }
    let mut last_err = None;
    for info in api
        .device_list()
        .filter(|d| d.vendor_id() == vid && d.product_id() == pid && d.usage_page() == USAGE_PAGE && d.usage() == USAGE)
    {
        let Ok(dev) = info.open_device(api) else { continue };
        // La collezione giusta dichiara 0xA1 da 63 byte e 0xA0 da 1040 (senza ID).
        if crate::hid::feature_len(&dev, REPORT_CMD) != Some(CMD_LEN - 1)
            || crate::hid::feature_len(&dev, REPORT_BLOB) != Some(BLOB_REPORT_LEN - 1)
        {
            continue;
        }
        let mut spec = spec.clone();
        (spec.vid, spec.pid) = (vid, pid);
        let mut d = Endgame {
            dev,
            spec,
            family: cfg.family,
            wireless: pid == PID_DONGLE,
            v2: false,
            firmware: None,
            needs_probe: false,
        };
        match d.init() {
            Ok(()) => return Ok(Box::new(d)),
            Err(e) => last_err = Some(e),
        }
    }
    match last_err {
        Some(e) => Err(e.context(mousecfg_core::tf!("{} trovato ma non risponde. Chiudi il software Endgame Gear e riprova.", "{} found but not responding. Close the Endgame Gear software and try again.", spec.name))),
        None => bail!(mousecfg_core::tf!("{}: collezione di configurazione (0xFF01) non trovata", "{}: configuration collection (0xFF01) not found", spec.name)),
    }
}

/// Controlla che la sezione del driver nel file TOML sia valida (usato dai test del database).
pub fn check_config(spec: &DeviceSpec) -> Result<()> {
    let cfg: EndgameConfig = spec.section("endgame")?;
    if spec.vid != VID {
        bail!(mousecfg_core::tf!("VID 0x{:04x} non gestito (solo 0x{VID:04x})", "VID 0x{:04x} not supported (only 0x{VID:04x})", spec.vid));
    }
    for [v, p] in std::iter::once([spec.vid, spec.pid]).chain(spec.aliases.iter().copied()) {
        if v != VID || !known_pid(cfg.family, p) {
            bail!(mousecfg_core::tf!("{v:04x}:{p:04x} non è un modello della famiglia {:?}", "{v:04x}:{p:04x} is not a model of the {:?} family", cfg.family));
        }
    }
    if !spec.caps.zones.is_empty() || !spec.caps.effects.is_empty() {
        bail!(mousecfg_core::t!("nessun modello Endgame Gear ha luci configurabili: zones ed effects devono essere vuoti", "no Endgame Gear model has configurable lighting: zones and effects must be empty"));
    }
    if cfg.family == Family::FourK {
        // Le capacità dei 4k le decide il driver: il TOML deve coincidere (ricevitore = v1, prudente).
        let v2 = model(spec.pid).is_some_and(|m| m.v2);
        let mut want = spec.caps.clone();
        caps_4k(&mut want, v2);
        let c = &spec.caps;
        if (c.dpi_min, c.dpi_max, c.dpi_step, c.max_stages, c.active_stage_settable, &c.polling)
            != (want.dpi_min, want.dpi_max, want.dpi_step, want.max_stages, want.active_stage_settable, &want.polling)
        {
            bail!(mousecfg_core::tf!("[caps] diverse da quelle del modello ({}-{} passo {}, polling {:?})", "[caps] differ from the model's ({}-{} step {}, polling {:?})", want.dpi_min, want.dpi_max, want.dpi_step, want.polling));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Blocco con i valori di fabbrica dei 4k (vole, PROTOCOL.md): 400/800/1600/3200, 4 stadi, LOD 1 mm.
    fn factory_blob() -> Vec<u8> {
        let mut b = vec![0u8; BLOB_LEN];
        b[B_ANGLE] = 0x80;
        b[B_POLLING] = 0x08;
        b[0x08] = 0x01;
        b[0x09] = 0x03;
        b[B_COUNT] = 4;
        let recs = [0x00, 0x90, 0x01, 0x90, 0x01, 0x00, 0x20, 0x03, 0x20, 0x03, 0x00, 0x40, 0x06, 0x40, 0x06, 0x00, 0x80, 0x0C, 0x80, 0x0C];
        b[B_CPI..B_CPI + 20].copy_from_slice(&recs);
        b
    }

    #[test]
    fn stage_encoding_matches_capture() {
        assert_eq!(encode_stage(400), [0x00, 0x90, 0x01, 0x90, 0x01]);
        assert_eq!(encode_stage(3200), [0x00, 0x80, 0x0C, 0x80, 0x0C]);
        assert_eq!(stage_dpi(&[0x00, 0x40, 0x06, 0x40, 0x06]), 1600);
        let s = decode_state(&factory_blob(), Family::FourK);
        assert_eq!(s.dpi_stages, Some(DpiStages { stages: vec![400, 800, 1600, 3200], active: 0 }));
        assert_eq!(s.polling_hz, Some(1000));
    }

    #[test]
    fn sensor_payload_matches_vendor_defaults() {
        let want = [
            0x00, 0x01, 0x03, 0x00, 0x00, 0x00, 0x04, 0x00, 0x00, 0x90, 0x01, 0x90, 0x01, 0x00, 0x20, 0x03, 0x20, 0x03, 0x00, 0x40,
            0x06, 0x40, 0x06, 0x00, 0x80, 0x0C, 0x80, 0x0C,
        ];
        let mut b = factory_blob();
        // v1: +5 sempre 0 anche se il blocco ha 0x80 in 0x01.
        assert_eq!(sensor_payload(&b, false), want);
        b[B_ANGLE] = 0xF6; // -10 gradi
        let p = sensor_payload(&b, true);
        assert_eq!(p[5], 0xF6);
        assert_eq!(p[..5], want[..5]);
        assert_eq!(p[6..], want[6..]);
    }

    #[test]
    fn power_payload_lengths_and_order() {
        let mut b = factory_blob();
        b[B_MOTION_SYNC] = 1;
        b[B_FLAGS] = 0x21;
        b[B_POWER_SAVING] = 0x0A;
        b[B_DEEP_SLEEP] = 0x85;
        for (i, &o) in B_CLICK_FILTERS.iter().enumerate() {
            b[o] = 0x10 + i as u8;
        }
        b[B_GLASS] = 1;
        let v1 = power_payload(&b, false);
        assert_eq!(v1, [0x01, 0x08, 0x21, 0x0A, 0x10, 0x11, 0x12, 0x13, 0x14, 0x85]);
        let v2 = power_payload(&b, true);
        assert_eq!(v2.len(), 11);
        assert_eq!(v2[10], 1);
    }

    #[test]
    fn polling_tables() {
        assert_eq!(polling_code(Family::FourK, false, 4000), Some(0x02));
        assert_eq!(polling_code(Family::FourK, false, 125), None);
        assert_eq!(polling_code(Family::FourK, true, 125), Some(0x40));
        assert_eq!(polling_hz(Family::FourK, 0x80), Some(1000));
        assert_eq!(polling_code(Family::EightK, true, 8000), Some(0x01));
        assert_eq!(polling_code(Family::EightK, true, 125), Some(0x40));
        assert_eq!(polling_hz(Family::EightK, 0x08), Some(1000));
        assert_eq!(polling_hz(Family::EightK, 0x03), None);
    }

    #[test]
    fn blob_reply_both_layouts() {
        let blob = factory_blob();
        let mut a = vec![0u8; BLOB_REPORT_LEN];
        a[0] = REPORT_BLOB;
        a[1] = ST_OK;
        a[16..1040].copy_from_slice(&blob);
        assert_eq!(parse_blob_reply(&a), Ok(blob.clone()));
        let mut b = vec![0u8; BLOB_REPORT_LEN];
        b[0] = REPORT_BLOB;
        b[1] = REPORT_CMD;
        b[2] = ST_OK;
        b[17..1041].copy_from_slice(&blob);
        assert_eq!(parse_blob_reply(&b), Ok(blob));
        b[2] = ST_PENDING;
        assert_eq!(parse_blob_reply(&b), Err(ST_PENDING));
    }

    #[test]
    fn stale_reply_is_rejected() {
        // Risposta vecchia da 64 byte (stato OK, resto a zero) letta al posto del blocco.
        let mut stale = vec![0u8; BLOB_REPORT_LEN];
        stale[0] = REPORT_CMD;
        stale[1] = ST_OK;
        let blob = parse_blob_reply(&stale).unwrap();
        assert!(check_blob(&blob, Family::FourK).is_err());
        assert!(check_blob(&factory_blob(), Family::FourK).is_ok());
        // Blocco spostato di un byte: X e Y non coincidono più.
        let mut shifted = vec![0u8];
        shifted.extend_from_slice(&factory_blob()[..BLOB_LEN - 1]);
        assert!(check_blob(&shifted, Family::FourK).is_err());
    }

    #[test]
    fn allowlist() {
        let cmd = |c: u8, t: u8| {
            let mut b = [0u8; CMD_LEN];
            b[0] = REPORT_CMD;
            b[1] = c;
            b[2] = t;
            b
        };
        assert!(allowed(Family::FourK, true, &cmd(CMD_PROBE, TARGET_DONGLE)));
        assert!(!allowed(Family::FourK, false, &cmd(CMD_PROBE, TARGET_DONGLE)));
        assert!(allowed(Family::FourK, false, &cmd(CMD_SENSOR, TARGET_MOUSE)));
        assert!(!allowed(Family::FourK, true, &cmd(0x13, 0))); // reset di fabbrica
        assert!(!allowed(Family::FourK, true, &cmd(0x16, TARGET_MOUSE))); // tasti
        assert!(!allowed(Family::FourK, true, &cmd(0x70, 0))); // abbinamento
        assert!(!allowed(Family::EightK, false, &cmd(0x3A, 0))); // bootloader
        assert!(!allowed(Family::EightK, false, &cmd(CMD_SENSOR, TARGET_MOUSE)));
        let mut big = vec![0u8; BLOB_REPORT_LEN];
        big[0] = REPORT_BLOB;
        assert!(!allowed(Family::EightK, false, &big));
        big[1] = CMD_STORE_8K;
        assert!(allowed(Family::EightK, false, &big));
        assert!(!allowed(Family::FourK, false, &big));
    }

    #[test]
    fn stages_change_only_their_bytes() {
        let old = factory_blob();
        let mut new = old.clone();
        apply_stages(&mut new, &[800, 12000], 1);
        assert_eq!(new[B_COUNT], 2);
        assert_eq!(new[B_ACTIVE], 1);
        assert_eq!(stage_dpi(&new[B_CPI..]), 800);
        assert_eq!(stage_dpi(&new[B_CPI + 5..]), 12000);
        assert_eq!(new[B_CPI + 10..B_CPI + 20], old[B_CPI + 10..B_CPI + 20]);
        assert!(ensure_only(&old, &new, &[(B_ACTIVE, B_COUNT + 1), (B_CPI, B_CPI + 20)]).is_ok());
        assert!(ensure_only(&old, &new, &[(B_CPI, B_CPI + 20)]).is_err());
        assert!(check_grid(&[10010], true).is_err());
        assert!(check_grid(&[10050, 9990], true).is_ok());
    }
}
