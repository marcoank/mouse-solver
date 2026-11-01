//! Driver Roccat / Turtle Beach per i modelli NON coperti da libratbag (quelli coperti stanno in roccat.rs).
//! Protocollo reimplementato leggendo roccat-tools, OpenRGB, Kone-AIMO-Config, roccat-kone-aimo-linux,
//! macOS-Kova-Configurator, roccat-konepro-linux (solo fatti, nessuna licenza) e Kone-XP-Air-Manager:
//! nessun codice copiato.
//!
//! Due trasporti:
//! - R1 (Kova 2016, Kova AIMO, Kone AIMO Remastered, Kone Pro, Kone XP): solo feature report.
//!   0x04 = selezione "leggi le impostazioni del profilo p" e stato (1 pronto, 3 occupato),
//!   0x05 = profilo attivo, 0x06 = record impostazioni del profilo (DPI, polling, luci) con somma u16
//!   negli ultimi 2 byte, 0x0E = "usa le impostazioni salvate" dopo la scrittura (non Kova).
//! - R2 (Kone XP Air, ricevitore e cavo): busta da 30 byte nel feature report 0x06
//!   `[06 ctx cmd op len payload...]`; il record 0x46 (78 byte) viaggia a pezzi da 25 byte,
//!   ognuno preceduto da un selettore `[pezzo, profilo]`.
//!
//! Si modifica solo il record del profilo attivo, preservando tutti i byte non toccati, e si rilegge
//! tutto il record per verifica. Tutto resta salvato nel mouse.
//!
//! Lasciati fuori di proposito (vedi anche i file devices/roccat_*.toml):
//! - Kone XP: DPI e stadio attivo in sola lettura (codifica DPI/50 solo dedotta dai valori di
//!   fabbrica, nessuna fonte la scrive).
//! - Kone Pro: stadi sempre 5 (la maschera degli stadi è scritta a 0x1F come fa OpenRGB, altri
//!   valori non sono documentati); la tabella Y non viene toccata (la fonte che scrive i DPI non la usa).
//! - Kone AIMO originale (2e27), Kone Pro Air, Kain: nessuna fonte documenta il record impostazioni.
//! - Burst Core / Burst Pro: maschera e stadio attivo contraddittori tra le fonti, report 0x04/0x05
//!   descritti diversamente, codifica DPI del Burst Pro sconosciuta, luci in conflitto.
//! - Burst Pro Air (cavo): non rilegge le impostazioni, il record va riscritto intero da un modello
//!   fisso e la somma di controllo è in conflitto tra le fonti.

use anyhow::{bail, Context, Result};
use hidapi::{HidApi, HidDevice};
use mousecfg_core::*;
use serde::Deserialize;
use std::thread::sleep;
use std::time::Duration;

const SELECT: u8 = 0x04;
const PROFILE: u8 = 0x05;
const SETTINGS: u8 = 0x06;
const COMMIT: u8 = 0x0E;
/// Byte 2 del report 0x04: "voglio leggere le impostazioni" (0x90 sarebbero i tasti: mai usato).
const SELECT_SETTINGS: u8 = 0x80;
/// "Usa le impostazioni salvate" (luci gestite dal mouse). `0E 06 01 01 00 FF` darebbe le luci al PC: mai inviato.
const COMMIT_FRAME: [u8; 6] = [0x0E, 0x06, 0x01, 0x00, 0x00, 0xFF];
const RATES: [u16; 4] = [125, 250, 500, 1000];
const PROFILES: u8 = 5;
const STAGES: usize = 5;
/// Una sola zona: su tutti questi mouse l'effetto è unico per tutto il mouse.
const ZONE: &str = "mouse";
const STATIC: u8 = 1;
const BREATHING: u8 = 3;

// Busta R2 (Kone XP Air).
const R2_LEN: usize = 30;
const R2_PAYLOAD: usize = 25;
const CMD_PROFILE_STATE: u8 = 0x45;
const CMD_MAIN: u8 = 0x46;
/// Sessione software: `[01 00 00 FF]` apre, `[00 00 00 FF]` chiude. `[01 01 01 FF]` (luci dal PC) mai inviato.
const CMD_SOFTWARE: u8 = 0x4E;
const OP_WRITE: u8 = 0x06;
const OP_READ: u8 = 0x07;
const OP_OK: u8 = 0x08;
const XP_AIR_RECEIVER: u16 = 0x5017;
/// Contesto usato dall'unica fonte (Kone-XP-Air-Manager) sia per il ricevitore sia per il cavo.
const XP_AIR_CTX: u8 = 1;
const SESSION_OPEN: [u8; 4] = [0x01, 0x00, 0x00, 0xFF];
const SESSION_CLOSE: [u8; 4] = [0x00, 0x00, 0x00, 0xFF];

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RoccatNewConfig {
    /// "kova_2016", "kova_aimo", "kone_aimo", "kone_pro", "kone_xp" o "kone_xp_air".
    pub family: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Family {
    Kova2016,
    KovaAimo,
    KoneAimo,
    KonePro,
    KoneXp,
    KoneXpAir,
}

impl Family {
    fn parse(s: &str) -> Result<Family> {
        Ok(match s {
            "kova_2016" => Family::Kova2016,
            "kova_aimo" => Family::KovaAimo,
            "kone_aimo" => Family::KoneAimo,
            "kone_pro" => Family::KonePro,
            "kone_xp" => Family::KoneXp,
            "kone_xp_air" => Family::KoneXpAir,
            other => bail!(
                tf!("famiglia '{other}' sconosciuta (valori: kova_2016, kova_aimo, kone_aimo, kone_pro, kone_xp, kone_xp_air)", "unknown family '{other}' (values: kova_2016, kova_aimo, kone_aimo, kone_pro, kone_xp, kone_xp_air)")
            ),
        })
    }

    fn is_r2(self) -> bool {
        self == Family::KoneXpAir
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DpiEnc {
    /// Un byte, DPI/50 (Kova).
    U8Div50,
    /// u16 LE, DPI/50 (Kone AIMO, Kone Pro, Kone XP).
    U16Div50,
    /// u16 LE, DPI così com'è (Kone XP Air).
    U16Raw,
}

#[derive(Debug, Clone)]
enum Lights {
    /// Kova: [14] bit0-1 accese, bit4-5 colori personalizzati; [15] color flow; [16] effetto
    /// (1 fisso, 3 respiro); [17] velocità 1-3; due luci {indice, R, G, B} a 18 e 22.
    Kova {
        /// Kova AIMO: la scala della velocità non è nota (letto 6, fuori da 1-3): mai toccata.
        touch_speed: bool,
    },
    /// Kone: un byte effetto, velocità e luminosità, slot colore con un byte di intensità prima dell'RGB.
    Coded {
        effect: usize,
        /// (byte, minimo, massimo, valore da usare se quello letto è fuori scala).
        speed: Option<(usize, u8, u8, u8)>,
        brightness: usize,
        /// (byte intensità, primo byte RGB) di ogni LED.
        slots: Vec<(usize, usize)>,
        /// Byte del tema: bit 7 = tema Roccat al posto dei colori propri.
        theme: Option<usize>,
        off: Option<u8>,
        wave: u8,
    },
}

/// Posizioni nel record impostazioni (offset compreso il byte 0 = 0x06).
#[derive(Debug, Clone)]
struct Layout {
    len: usize,
    mask: usize,
    active: usize,
    dpi: usize,
    dpi_y: Option<usize>,
    enc: DpiEnc,
    dpi_writable: bool,
    /// false: si scrivono sempre 5 stadi con maschera 0x1F (l'unico valore scritto dalle fonti).
    mask_writable: bool,
    active_writable: bool,
    polling: usize,
    lights: Lights,
    /// Dopo la scrittura serve `0E 06 01 00 00 FF`.
    commit: bool,
    /// true: stato 2 = rifiutato e timeout = errore (roccat-tools). false (Kone Pro/XP): il significato
    /// del byte di stato è discusso tra le fonti, si continua e decide la rilettura.
    strict_status: bool,
}

fn layout(f: Family) -> Layout {
    match f {
        // roccat-tools kova2016_profile_settings.h; Kova AIMO: stesso schema, record da 37 byte.
        Family::Kova2016 | Family::KovaAimo => Layout {
            len: if f == Family::Kova2016 { 28 } else { 37 },
            mask: 6,
            active: 12,
            dpi: 7,
            dpi_y: None,
            enc: DpiEnc::U8Div50,
            dpi_writable: true,
            mask_writable: true,
            active_writable: true,
            polling: 13,
            lights: Lights::Kova { touch_speed: f == Family::Kova2016 },
            commit: false,
            strict_status: true,
        },
        // Kone-AIMO-Config e roccat-kone-aimo-linux (concordano byte per byte). Velocità: scala non
        // nota con certezza (le fonti la scrivono solo se l'utente la cambia), mai toccata.
        Family::KoneAimo => Layout {
            len: 126,
            mask: 4,
            active: 5,
            dpi: 6,
            dpi_y: Some(16),
            enc: DpiEnc::U16Div50,
            dpi_writable: true,
            mask_writable: true,
            active_writable: true,
            polling: 26,
            lights: Lights::Coded {
                effect: 33,
                speed: None,
                brightness: 35,
                slots: [61, 66, 71, 76, 81, 86, 91, 96, 101, 106, 111].iter().map(|&o| (o, o + 1)).collect(),
                theme: Some(116),
                off: Some(0),
                wave: 7,
            },
            commit: true,
            strict_status: true,
        },
        // roccat-konepro-linux (DPI, stadio, polling, effetti) e OpenRGB (luci, commit).
        Family::KonePro => Layout {
            len: 69,
            mask: 5,
            active: 6,
            dpi: 7,
            dpi_y: None,
            enc: DpiEnc::U16Div50,
            dpi_writable: true,
            mask_writable: false,
            active_writable: true,
            polling: 29,
            lights: Lights::Coded {
                effect: 30,
                speed: Some((31, 1, 11, 6)),
                brightness: 32,
                slots: vec![(37, 38), (42, 43)],
                theme: None,
                off: Some(0),
                wave: 10,
            },
            commit: true,
            strict_status: false,
        },
        // OpenRGB RoccatKoneXPController: 20 slot da 6 byte {?, intensità, R, G, B, ?}.
        Family::KoneXp => Layout {
            len: 174,
            mask: 5,
            active: 6,
            dpi: 7,
            dpi_y: None,
            enc: DpiEnc::U16Div50,
            dpi_writable: false,
            mask_writable: false,
            active_writable: false,
            polling: 29,
            lights: Lights::Coded {
                effect: 30,
                speed: Some((31, 1, 11, 6)),
                brightness: 32,
                slots: (0..20).map(|i| (37 + 6 * i, 38 + 6 * i)).collect(),
                theme: Some(163),
                off: Some(0),
                wave: 10,
            },
            commit: true,
            strict_status: false,
        },
        // Kone-XP-Air-Manager (dal modulo del produttore + rilettura dal vivo). Nessun valore "spento"
        // documentato: si usa la luminosità 0. Byte 17-26 (tabella "riservata") mai toccati.
        Family::KoneXpAir => Layout {
            len: 78,
            mask: 5,
            active: 6,
            dpi: 7,
            dpi_y: None,
            enc: DpiEnc::U16Raw,
            dpi_writable: true,
            mask_writable: true,
            active_writable: true,
            polling: 29,
            lights: Lights::Coded {
                effect: 30,
                speed: Some((31, 1, 8, 6)),
                brightness: 32,
                slots: (0..7).map(|i| (37 + 5 * i, 38 + 5 * i)).collect(),
                theme: None,
                off: None,
                wave: 10,
            },
            commit: false,
            strict_status: false,
        },
    }
}

fn supported_effects(l: &Layout) -> Vec<&'static str> {
    match &l.lights {
        Lights::Kova { .. } => vec!["static", "breathing", "spectrum", "off"],
        Lights::Coded { off, .. } => {
            let mut v = vec!["static", "breathing", "wave"];
            if off.is_some() {
                v.push("off");
            }
            v
        }
    }
}

// ---------------------------------------------------------------- funzioni pure (record)

fn sum16(b: &[u8]) -> u16 {
    b.iter().fold(0u16, |a, &x| a.wrapping_add(x as u16))
}

/// Somma u16 LE di tutti i byte prima degli ultimi due (ID e lunghezza compresi).
fn checksum_ok(rec: &[u8]) -> bool {
    let n = rec.len();
    n > 2 && sum16(&rec[..n - 2]) == u16::from_le_bytes([rec[n - 2], rec[n - 1]])
}

fn fix_checksum(rec: &mut [u8]) {
    let n = rec.len();
    let c = sum16(&rec[..n - 2]);
    rec[n - 2..].copy_from_slice(&c.to_le_bytes());
}

/// Controlla intestazione, profilo e somma prima di usare (o scrivere) un record.
fn validate(l: &Layout, rec: &[u8], profile: u8) -> Result<()> {
    if rec.len() != l.len || rec[0] != SETTINGS || rec[1] as usize != l.len {
        bail!(tf!("record impostazioni non valido (intestazione {:02x?})", "invalid settings record (header {:02x?})", &rec[..rec.len().min(3)]));
    }
    if rec[2] != profile {
        bail!(tf!("il record è del profilo {} invece che del profilo {}", "the record is for profile {} instead of profile {}", rec[2] + 1, profile + 1));
    }
    if !checksum_ok(rec) {
        bail!(t!("somma di controllo del record errata: lettura non affidabile", "record checksum mismatch: the read is not reliable"));
    }
    Ok(())
}

fn dpi_raw(enc: DpiEnc, d: u16) -> Result<u16> {
    match enc {
        DpiEnc::U8Div50 if d % 50 != 0 || d / 50 > 255 || d == 0 => bail!(tf!("DPI {d} non codificabile", "DPI {d} cannot be encoded")),
        DpiEnc::U16Div50 if d % 50 != 0 || d == 0 => bail!(tf!("DPI {d} non è un multiplo di 50", "DPI {d} is not a multiple of 50")),
        DpiEnc::U8Div50 | DpiEnc::U16Div50 => Ok(d / 50),
        DpiEnc::U16Raw if d == 0 => bail!(t!("DPI 0 non valido", "DPI 0 is not valid")),
        DpiEnc::U16Raw => Ok(d),
    }
}

fn read_dpi(l: &Layout, rec: &[u8], i: usize) -> u16 {
    match l.enc {
        DpiEnc::U8Div50 => rec[l.dpi + i] as u16 * 50,
        DpiEnc::U16Div50 => u16::from_le_bytes([rec[l.dpi + 2 * i], rec[l.dpi + 2 * i + 1]]).saturating_mul(50),
        DpiEnc::U16Raw => u16::from_le_bytes([rec[l.dpi + 2 * i], rec[l.dpi + 2 * i + 1]]),
    }
}

fn write_dpi(l: &Layout, rec: &mut [u8], base: usize, i: usize, d: u16) -> Result<()> {
    let raw = dpi_raw(l.enc, d)?;
    match l.enc {
        DpiEnc::U8Div50 => rec[base + i] = raw as u8,
        DpiEnc::U16Div50 | DpiEnc::U16Raw => rec[base + 2 * i..base + 2 * i + 2].copy_from_slice(&raw.to_le_bytes()),
    }
    Ok(())
}

fn decode_effect(l: &Layout, rec: &[u8]) -> (Effect, Option<u8>) {
    match &l.lights {
        Lights::Kova { .. } => {
            let color = Rgb(rec[19], rec[20], rec[21]);
            let e = if rec[14] & 0x03 == 0 {
                Effect::Off
            } else if rec[15] == 1 {
                Effect::Spectrum
            } else {
                match rec[16] {
                    STATIC => Effect::Static { color },
                    BREATHING => Effect::Breathing { color },
                    code => Effect::Unknown { code },
                }
            };
            (e, None)
        }
        Lights::Coded { effect, brightness, slots, off, wave, .. } => {
            let o = slots[0].1;
            let color = Rgb(rec[o], rec[o + 1], rec[o + 2]);
            let code = rec[*effect];
            let e = match code {
                c if Some(c) == *off => Effect::Off,
                STATIC => Effect::Static { color },
                BREATHING => Effect::Breathing { color },
                c if c == *wave => Effect::Wave { reverse: false },
                code => Effect::Unknown { code },
            };
            (e, Some(rec[*brightness]))
        }
    }
}

fn decode(l: &Layout, rec: &[u8]) -> MouseState {
    let mask = match rec[l.mask] & 0x1F {
        0 => 0x1F,
        m => m,
    };
    let mut stages = Vec::new();
    let mut active = 0;
    for i in 0..STAGES {
        if mask & (1 << i) == 0 {
            continue;
        }
        if i == rec[l.active] as usize {
            active = stages.len();
        }
        stages.push(read_dpi(l, rec, i));
    }
    // Valori assurdi = codifica sbagliata: meglio non mostrare niente.
    let plausible = stages.iter().all(|&d| (50..=30000).contains(&d));
    let (effect, brightness) = decode_effect(l, rec);
    MouseState {
        dpi: if plausible { stages.get(active).copied() } else { None },
        dpi_stages: plausible.then_some(DpiStages { stages, active }),
        polling_hz: RATES.get(rec[l.polling] as usize).copied(),
        zones: vec![ZoneState { zone: ZONE.into(), effect: Some(effect), brightness }],
        cached: false,
    }
}

fn encode_stages(l: &Layout, rec: &mut [u8], stages: &[u16], active: usize) -> Result<()> {
    if !l.dpi_writable {
        bail!(t!("su questo modello i DPI sono in sola lettura: nessuna fonte conferma come scriverli", "DPI is read-only on this model: no source confirms how to write it"));
    }
    if stages.is_empty() || stages.len() > STAGES || active >= stages.len() {
        bail!(t!("servono da 1 a 5 stadi DPI", "1 to 5 DPI stages required"));
    }
    if !l.mask_writable && stages.len() != STAGES {
        bail!(t!("questo modello accetta solo 5 stadi DPI", "this model only accepts 5 DPI stages"));
    }
    for (i, &d) in stages.iter().enumerate() {
        write_dpi(l, rec, l.dpi, i, d)?;
        if let Some(y) = l.dpi_y {
            write_dpi(l, rec, y, i, d)?;
        }
    }
    // Stadi oltre l'ultimo disattivati, i loro DPI vecchi restano.
    rec[l.mask] = ((1u16 << stages.len()) - 1) as u8;
    if l.active_writable {
        rec[l.active] = active as u8;
    }
    Ok(())
}

fn encode_polling(l: &Layout, rec: &mut [u8], hz: u16) -> Result<()> {
    let code = RATES.iter().position(|&r| r == hz).context(t!("polling non gestito", "polling rate not supported"))?;
    rec[l.polling] = code as u8;
    Ok(())
}

fn encode_effect(l: &Layout, rec: &mut [u8], effect: &Effect) -> Result<()> {
    match &l.lights {
        Lights::Kova { touch_speed } => {
            match effect {
                Effect::Off => rec[14] &= !0x03,
                Effect::Static { .. } | Effect::Breathing { .. } => {
                    rec[14] |= 0x33; // accese + colori personalizzati (come OpenRGB)
                    rec[15] = 0;
                    rec[16] = if matches!(effect, Effect::Static { .. }) { STATIC } else { BREATHING };
                    if *touch_speed && rec[16] == BREATHING && !(1..=3).contains(&rec[17]) {
                        rec[17] = 2;
                    }
                }
                Effect::Spectrum => {
                    rec[14] |= 0x33;
                    rec[15] = 1; // color flow
                    if !(1..=3).contains(&rec[16]) {
                        rec[16] = STATIC;
                    }
                }
                e => bail!(tf!("effetto '{}' non gestito da questo mouse", "effect '{}' not supported by this mouse", e.name())),
            }
            if let Some(c) = effect.color() {
                // Indice del colore predefinito (18, 22) lasciato com'è: valgono i colori personalizzati.
                for o in [19, 23] {
                    rec[o..o + 3].copy_from_slice(&[c.0, c.1, c.2]);
                }
            }
        }
        Lights::Coded { effect: at, speed, slots, theme, off, wave, .. } => {
            let code = match effect {
                Effect::Off => off.context(t!("questo mouse non ha l'effetto 'spento': usa la luminosità 0", "this mouse has no 'off' effect: use brightness 0"))?,
                Effect::Static { .. } => STATIC,
                Effect::Breathing { .. } => BREATHING,
                Effect::Wave { reverse: false } => *wave,
                Effect::Wave { reverse: true } => bail!(t!("la direzione dell'onda non è configurabile", "the wave direction is not configurable")),
                e => bail!(tf!("effetto '{}' non gestito da questo mouse", "effect '{}' not supported by this mouse", e.name())),
            };
            rec[*at] = code;
            if matches!(effect, Effect::Breathing { .. } | Effect::Wave { .. }) {
                if let Some((i, lo, hi, def)) = *speed {
                    if !(lo..=hi).contains(&rec[i]) {
                        rec[i] = def;
                    }
                }
            }
            if let Some(c) = effect.color() {
                for &(a, o) in slots {
                    rec[a] = 0xFF;
                    rec[o..o + 3].copy_from_slice(&[c.0, c.1, c.2]);
                }
                if let Some(t) = theme {
                    rec[*t] &= 0x7F; // colori propri invece del tema
                }
            }
        }
    }
    Ok(())
}

fn encode_brightness(l: &Layout, rec: &mut [u8], v: u8) -> Result<()> {
    match &l.lights {
        Lights::Coded { brightness, .. } => {
            rec[*brightness] = v;
            Ok(())
        }
        Lights::Kova { .. } => bail!(t!("questo mouse non ha un comando per la luminosità: usa un colore più scuro", "this mouse has no brightness command: use a darker color")),
    }
}

// ---------------------------------------------------------------- lista dei comandi permessi

/// R1: solo selezione impostazioni, record impostazioni completo e valido, commit "usa le impostazioni salvate".
/// Tutto il resto (tasti 0x07, luci dal PC 0x0D, SROM/firmware 0x0D/0x1B/0x1C sul Kova, ...) è rifiutato.
fn allowed_r1(l: &Layout, f: &[u8]) -> bool {
    match f.first() {
        Some(&SELECT) => f.len() >= 3 && f[1] < PROFILES && f[2] == SELECT_SETTINGS && f[3..].iter().all(|&b| b == 0),
        Some(&SETTINGS) => f.len() == l.len && f[1] as usize == l.len && f[2] < PROFILES && checksum_ok(f),
        Some(&COMMIT) => l.commit && f == COMMIT_FRAME,
        _ => false,
    }
}

/// R2: interrogazione del ricevitore, apertura/chiusura sessione, letture 0x45/0x46, scrittura a pezzi di 0x46.
/// Rifiutati tra gli altri 0x49 (ripristino di fabbrica), l'abbinamento, 0x4C/0x4D e le luci dal PC.
fn allowed_r2(f: &[u8]) -> bool {
    if f.len() != R2_LEN || f[0] != SETTINGS {
        return false;
    }
    let n = f[4] as usize;
    if n > R2_PAYLOAD || f[5 + n..].iter().any(|&b| b != 0) {
        return false;
    }
    let p = &f[5..5 + n];
    if f[2] == 0x00 {
        // Fasi 4 e 5 dell'interrogazione del ricevitore: `06 00 00 04` / `06 00 00 05`.
        return f[1] == 0 && matches!(f[3], 0x04 | 0x05) && n == 0;
    }
    if f[1] != XP_AIR_CTX {
        return false;
    }
    match (f[2], f[3]) {
        (CMD_SOFTWARE, OP_WRITE) => p == SESSION_OPEN || p == SESSION_CLOSE,
        (CMD_PROFILE_STATE, OP_READ) | (CMD_MAIN, OP_READ) => n == 0,
        // Selettore [pezzo, profilo] oppure un pezzo del record (78 = 25 + 25 + 25 + 3).
        (CMD_MAIN, OP_WRITE) => (n == 2 && p[0] < 4 && p[1] < PROFILES) || n == R2_PAYLOAD || n == 3,
        _ => false,
    }
}

fn r2_frame(cmd: u8, op: u8, payload: &[u8]) -> Vec<u8> {
    let mut b = vec![0u8; R2_LEN];
    b[0] = SETTINGS;
    b[1] = XP_AIR_CTX;
    b[2] = cmd;
    b[3] = op;
    b[4] = payload.len() as u8;
    b[5..5 + payload.len()].copy_from_slice(payload);
    b
}

/// Risposta R2: stesso contesto e comando, operazione 0x08 (0x0C = rifiutato), payload della lunghezza dichiarata.
fn parse_r2(b: &[u8], cmd: u8) -> Result<Vec<u8>> {
    if b.len() < 5 || b[0] != SETTINGS || b[1] != XP_AIR_CTX || b[2] != cmd {
        bail!(tf!("risposta inattesa dal mouse ({:02x?})", "unexpected response from the mouse ({:02x?})", &b[..b.len().min(5)]));
    }
    if b[3] != OP_OK {
        bail!(tf!("il mouse ha rifiutato il comando {cmd:02x} (risposta {:02x})", "the mouse rejected command {cmd:02x} (response {:02x})", b[3]));
    }
    let n = b[4] as usize;
    if n > R2_PAYLOAD || b.len() < 5 + n {
        bail!(t!("risposta del mouse troppo corta", "mouse response too short"));
    }
    Ok(b[5..5 + n].to_vec())
}

fn debug(dir: &str, b: &[u8]) {
    if mousecfg_core::trace::on() {
        mousecfg_core::trace!("{dir} {}", b.iter().map(|x| format!("{x:02x}")).collect::<Vec<_>>().join(" "));
    }
}

// ---------------------------------------------------------------- driver

pub struct RoccatNew {
    dev: HidDevice,
    spec: DeviceSpec,
    fam: Family,
    l: Layout,
    /// Feature report dichiarati dalla collezione aperta: (ID, lunghezza compreso l'ID).
    lens: Vec<(u8, usize)>,
    /// Lunghezza massima dei feature report della collezione (come fa Windows con HidD_*Feature).
    pad: usize,
    /// Sessione software aperta sul Kone XP Air: va chiusa all'uscita.
    session: bool,
}

/// Controlla che la sezione del driver nel file TOML sia valida (usato dai test del database).
pub fn check_config(spec: &DeviceSpec) -> Result<()> {
    let cfg: RoccatNewConfig = spec.section("roccat_new")?;
    let fam = Family::parse(&cfg.family)?;
    check_caps(&layout(fam), &spec.caps).with_context(|| tf!("[caps] non coerente con la famiglia {}", "[caps] inconsistent with family {}", cfg.family))
}

fn check_caps(l: &Layout, c: &Capabilities) -> Result<()> {
    if c.max_stages as usize > STAGES || c.min_stages < 1 {
        bail!(t!("gli stadi vanno da 1 a 5", "stages range from 1 to 5"));
    }
    if l.dpi_writable && !l.mask_writable && c.min_stages as usize != STAGES {
        bail!(t!("min_stages deve essere 5", "min_stages must be 5"));
    }
    if !l.active_writable && c.active_stage_settable {
        bail!(t!("active_stage_settable deve essere false", "active_stage_settable must be false"));
    }
    if let Some(p) = c.polling.iter().find(|p| !RATES.contains(p)) {
        bail!(tf!("polling {p} non codificabile", "polling {p} cannot be encoded"));
    }
    for d in [c.dpi_min, c.dpi_max] {
        dpi_raw(l.enc, d)?;
    }
    if l.enc != DpiEnc::U16Raw && c.dpi_step % 50 != 0 {
        bail!(t!("dpi_step deve essere un multiplo di 50", "dpi_step must be a multiple of 50"));
    }
    if c.zones.iter().any(|z| z != ZONE) || (!c.effects.is_empty() && c.zones.is_empty()) {
        bail!(tf!("l'unica zona è \"{ZONE}\"", "the only zone is \"{ZONE}\""));
    }
    let ok = supported_effects(l);
    if let Some(e) = c.effects.iter().find(|e| !ok.contains(&e.as_str())) {
        bail!(tf!("effetto '{e}' non gestito (possibili: {})", "effect '{e}' not supported (possible: {})", ok.join(", ")));
    }
    Ok(())
}

/// Apre il mouse e restituisce il driver.
pub fn open(api: &HidApi, spec: &DeviceSpec, vid: u16, pid: u16) -> Result<Box<dyn MouseDriver>> {
    check_config(spec)?;
    let cfg: RoccatNewConfig = spec.section("roccat_new")?;
    let fam = Family::parse(&cfg.family)?;
    let l = layout(fam);
    let mut spec = spec.clone();
    (spec.vid, spec.pid) = (vid, pid);
    let mut last_err = None;
    for info in api.device_list().filter(|d| d.vendor_id() == vid && d.product_id() == pid) {
        let Ok(dev) = info.open_device(api) else { continue };
        let mut desc = [0u8; 4096];
        let Ok(n) = dev.get_report_descriptor(&mut desc) else { continue };
        let lens: Vec<(u8, usize)> = crate::hid::feature_reports(&desc[..n]).into_iter().map(|(i, n)| (i, n + 1)).collect();
        // La collezione giusta dichiara il report 0x06 della lunghezza attesa.
        let Some(&(_, n06)) = lens.iter().find(|(i, _)| *i == SETTINGS) else { continue };
        if (fam.is_r2() && n06 != R2_LEN) || (!fam.is_r2() && n06 < l.len) {
            continue;
        }
        let pad = lens.iter().map(|x| x.1).max().unwrap_or(0);
        let mut drv = RoccatNew { dev, spec: spec.clone(), fam, l: l.clone(), lens, pad, session: false };
        let res = if fam.is_r2() { drv.start_session(pid).and_then(|_| drv.read().map(|_| ())) } else { drv.read().map(|_| ()) };
        match res {
            Ok(()) => return Ok(Box::new(drv)),
            Err(e) => last_err = Some(e),
        }
    }
    let app = if fam.is_r2() { "Turtle Beach Swarm II" } else { "Roccat Swarm" };
    match last_err {
        Some(e) => bail!(tf!("{} trovato ma non risponde ({e:#}). Chiudi {app} e riprova.", "{} found but not responding ({e:#}). Close {app} and try again.", spec.name)),
        None => bail!(tf!("{}: interfaccia di configurazione non trovata. Chiudi {app} e riprova.", "{}: configuration interface not found. Close {app} and try again.", spec.name)),
    }
}

impl RoccatNew {
    fn declared(&self, id: u8) -> Option<usize> {
        self.lens.iter().find(|(i, _)| *i == id).map(|x| x.1)
    }

    /// Invia un feature report, solo se è nella lista dei comandi permessi.
    fn send(&self, frame: &[u8]) -> Result<()> {
        let ok = if self.fam.is_r2() { allowed_r2(frame) } else { allowed_r1(&self.l, frame) };
        if !ok {
            bail!(tf!("comando {:02x?} non permesso: bloccato prima di arrivare al mouse", "command {:02x?} not allowed: blocked before reaching the mouse", &frame[..frame.len().min(6)]));
        }
        let mut b = frame.to_vec();
        if let Some(n) = self.declared(frame[0]) {
            b.resize(b.len().max(n), 0);
        }
        debug(">", &b);
        match self.dev.send_feature_report(&b) {
            Ok(()) => Ok(()),
            Err(_) if self.pad > b.len() => {
                b.resize(self.pad, 0);
                debug(">", &b);
                self.dev.send_feature_report(&b).context(t!("scrittura sul mouse fallita", "writing to the mouse failed"))
            }
            Err(e) => Err(e).context(t!("scrittura sul mouse fallita", "writing to the mouse failed")),
        }
    }

    /// Legge i primi `n` byte (ID compreso) del feature report `id`.
    fn get(&self, id: u8, n: usize) -> Result<Vec<u8>> {
        if !matches!(id, SELECT | PROFILE | SETTINGS) || (self.fam.is_r2() && id != SETTINGS) {
            bail!(tf!("lettura del report {id:02x} non permessa", "reading report {id:02x} not allowed"));
        }
        let first = self.declared(id).unwrap_or(n).max(n);
        let mut b = vec![0u8; first];
        b[0] = id;
        if let Err(e) = self.dev.get_feature_report(&mut b) {
            if self.pad <= first {
                return Err(e).context(t!("lettura dal mouse fallita", "reading from the mouse failed"));
            }
            b = vec![0u8; self.pad];
            b[0] = id;
            self.dev.get_feature_report(&mut b).context(t!("lettura dal mouse fallita", "reading from the mouse failed"))?;
        }
        debug("<", &b);
        if b[0] != id {
            bail!(tf!("risposta inattesa dal mouse (report {:02x} invece di {id:02x})", "unexpected response from the mouse (report {:02x} instead of {id:02x})", b[0]));
        }
        b.truncate(n);
        Ok(b)
    }

    // ------------------------------------------------------------ R1

    fn wait_ready(&self) -> Result<()> {
        sleep(Duration::from_millis(50));
        let mut last = 0;
        for _ in 0..40 {
            last = self.get(SELECT, 3)?[1];
            match last {
                0x01 => return Ok(()),
                0x02 if self.l.strict_status => bail!(t!("il mouse ha rifiutato il comando", "the mouse rejected the command")),
                _ => sleep(Duration::from_millis(50)),
            }
        }
        if self.l.strict_status {
            bail!(tf!("il mouse non risponde (stato {last:02x})", "the mouse is not responding (status {last:02x})"));
        }
        // Kone Pro/XP: si prosegue, la validità è controllata dalla rilettura.
        debug("stato", &[last]);
        Ok(())
    }

    fn read_r1(&self) -> Result<(u8, Vec<u8>)> {
        let p = self.get(PROFILE, 3)?[2];
        if p >= PROFILES {
            bail!(tf!("profilo attivo non valido ({p})", "invalid active profile ({p})"));
        }
        self.send(&[SELECT, p, SELECT_SETTINGS])?;
        self.wait_ready()?;
        let rec = self.get(SETTINGS, self.l.len)?;
        validate(&self.l, &rec, p)?;
        Ok((p, rec))
    }

    fn write_r1(&self, p: u8, rec: &[u8]) -> Result<()> {
        self.send(&[SELECT, p, SELECT_SETTINGS])?;
        self.wait_ready()?;
        self.send(rec)?;
        self.wait_ready()?;
        if self.l.commit {
            self.send(&COMMIT_FRAME)?;
            self.wait_ready()?;
        }
        Ok(())
    }

    // ------------------------------------------------------------ R2

    fn start_session(&mut self, pid: u16) -> Result<()> {
        if pid == XP_AIR_RECEIVER {
            let mut q = vec![0u8; R2_LEN];
            q[0] = SETTINGS;
            q[3] = 0x04;
            self.send(&q)?;
            sleep(Duration::from_millis(50));
            let a = self.get(SETTINGS, R2_LEN)?;
            if a[1] != 0 || a[3] != OP_OK {
                bail!(t!("il ricevitore non risponde", "the receiver is not responding"));
            }
            q[3] = 0x05;
            self.send(&q)?;
            sleep(Duration::from_millis(50));
            let b = self.get(SETTINGS, R2_LEN)?;
            if b[1] != 0 || b[3] != OP_OK || b[13] != 1 {
                bail!(t!("il mouse non è collegato al ricevitore: accendilo o muovilo", "the mouse is not connected to the receiver: turn it on or move it"));
            }
        }
        self.send(&r2_frame(CMD_SOFTWARE, OP_WRITE, &SESSION_OPEN))?;
        self.session = true;
        sleep(Duration::from_millis(80));
        Ok(())
    }

    fn r2_read(&self, cmd: u8) -> Result<Vec<u8>> {
        self.send(&r2_frame(cmd, OP_READ, &[]))?;
        sleep(Duration::from_millis(90));
        parse_r2(&self.get(SETTINGS, R2_LEN)?, cmd)
    }

    fn read_r2(&self) -> Result<(u8, Vec<u8>)> {
        let st = self.r2_read(CMD_PROFILE_STATE)?;
        if st.len() < 2 || st[0] >= PROFILES || st[0] >= st[1] {
            bail!(tf!("stato dei profili non valido ({st:02x?})", "invalid profile state ({st:02x?})"));
        }
        let p = st[0];
        let mut rec = Vec::with_capacity(self.l.len);
        for k in 0..4u8 {
            if rec.len() >= self.l.len {
                break;
            }
            self.send(&r2_frame(CMD_MAIN, OP_WRITE, &[k, p]))?;
            sleep(Duration::from_millis(90));
            let part = self.r2_read(CMD_MAIN)?;
            if part.is_empty() {
                bail!(t!("il mouse ha risposto con un pezzo vuoto", "the mouse replied with an empty chunk"));
            }
            let take = (self.l.len - rec.len()).min(part.len());
            rec.extend_from_slice(&part[..take]);
        }
        if rec.len() != self.l.len {
            bail!(tf!("record impostazioni incompleto ({} byte)", "incomplete settings record ({} bytes)", rec.len()));
        }
        validate(&self.l, &rec, p)?;
        Ok((p, rec))
    }

    fn write_r2(&self, p: u8, rec: &[u8]) -> Result<()> {
        for (k, part) in rec.chunks(R2_PAYLOAD).enumerate() {
            self.send(&r2_frame(CMD_MAIN, OP_WRITE, &[k as u8, p]))?;
            sleep(Duration::from_millis(30));
            self.send(&r2_frame(CMD_MAIN, OP_WRITE, part))?;
            sleep(Duration::from_millis(30));
        }
        sleep(Duration::from_millis(120));
        Ok(())
    }

    // ------------------------------------------------------------ comune

    fn read(&self) -> Result<(u8, Vec<u8>)> {
        if self.fam.is_r2() {
            self.read_r2()
        } else {
            self.read_r1()
        }
    }

    /// Legge il record del profilo attivo, lo modifica, lo scrive e lo rilegge per verifica.
    fn modify(&self, change: impl FnOnce(&Layout, &mut [u8]) -> Result<()>) -> Result<()> {
        let (p, old) = self.read()?;
        let mut rec = old.clone();
        change(&self.l, &mut rec)?;
        rec[2] = p;
        fix_checksum(&mut rec);
        if rec == old {
            return Ok(());
        }
        validate(&self.l, &rec, p)?;
        if self.fam.is_r2() {
            self.write_r2(p, &rec)?;
        } else {
            self.write_r1(p, &rec)?;
        }
        let (p2, back) = self.read()?;
        if p2 != p || back != rec {
            bail!(t!("verifica fallita: il mouse non ha salvato le impostazioni come inviate", "verification failed: the mouse did not save the settings as sent"));
        }
        Ok(())
    }
}

impl Drop for RoccatNew {
    fn drop(&mut self) {
        if self.session {
            // Come Swarm: la sessione software è temporanea e va chiusa, altrimenti gli effetti possono fermarsi.
            let _ = self.send(&r2_frame(CMD_SOFTWARE, OP_WRITE, &SESSION_CLOSE));
            sleep(Duration::from_millis(80));
        }
    }
}

impl MouseDriver for RoccatNew {
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
        let (_, rec) = self.read()?;
        Ok(decode(&self.l, &rec))
    }

    fn set_dpi_stages(&mut self, stages: &[u16], active: usize) -> Result<()> {
        let caps = &self.spec.caps;
        // Se lo stadio attivo non è scrivibile viene ignorato (resta quello del mouse).
        let check_active = if caps.active_stage_settable { active } else { 0 };
        caps.check_dpi_stages(stages, check_active)?;
        self.modify(|l, rec| encode_stages(l, rec, stages, check_active))
    }

    fn set_polling(&mut self, hz: u16) -> Result<()> {
        self.spec.caps.check_polling(hz)?;
        self.modify(|l, rec| encode_polling(l, rec, hz))
    }

    fn set_lighting(&mut self, zone: &str, effect: &Effect) -> Result<()> {
        self.spec.caps.check_effect(zone, effect)?;
        self.modify(|l, rec| encode_effect(l, rec, effect))
    }

    fn set_brightness(&mut self, zone: &str, value: u8) -> Result<()> {
        if !self.spec.caps.zones.iter().any(|z| z == zone) {
            bail!(tf!("zona '{zone}' non esiste", "zone '{zone}' does not exist"));
        }
        self.modify(|l, rec| encode_brightness(l, rec, value))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hex(s: &str) -> Vec<u8> {
        s.split_whitespace().map(|x| u8::from_str_radix(x, 16).unwrap()).collect()
    }

    /// Record di fabbrica del Kova 2016 (roccat-tools kova2016_profile_settings.c).
    fn kova_default() -> Vec<u8> {
        hex("06 1C 00 00 06 FF 1F 08 10 20 46 8C 01 03 03 01 03 02 00 05 90 FE 00 05 90 FE 83 05")
    }

    /// Profilo 0 letto da un Kova AIMO vero (macOS-Kova-Configurator fixtures.json).
    fn kova_aimo_fixture() -> Vec<u8> {
        hex("06 25 00 00 06 ff 1f 08 10 31 46 8c 03 03 33 00 01 06 ff 8b 01 33 ff 74 0b 56 00 00 00 00 00 00 00 00 00 3c 06")
    }

    fn blank(f: Family, profile: u8) -> Vec<u8> {
        let l = layout(f);
        let mut r = vec![0u8; l.len];
        r[0] = SETTINGS;
        r[1] = l.len as u8;
        r[2] = profile;
        r[l.mask] = 0x1F;
        fix_checksum(&mut r);
        r
    }

    #[test]
    fn checksum_matches_real_records() {
        assert!(checksum_ok(&kova_default()));
        let a = kova_aimo_fixture();
        assert_eq!(sum16(&a[..35]), 0x063C);
        assert!(checksum_ok(&a));
        let mut b = a.clone();
        b[9] = 0x32;
        assert!(!checksum_ok(&b));
        fix_checksum(&mut b);
        assert!(checksum_ok(&b));
    }

    #[test]
    fn decode_kova_records() {
        let l = layout(Family::Kova2016);
        let s = decode(&l, &kova_default());
        assert_eq!(s.dpi_stages, Some(DpiStages { stages: vec![400, 800, 1600, 3500, 7000], active: 1 }));
        assert_eq!(s.polling_hz, Some(1000));
        assert_eq!(s.zones[0].effect, Some(Effect::Spectrum)); // color flow acceso
        assert_eq!(s.zones[0].brightness, None);

        let l = layout(Family::KovaAimo);
        let s = decode(&l, &kova_aimo_fixture());
        assert_eq!(s.dpi_stages, Some(DpiStages { stages: vec![400, 800, 2450, 3500, 7000], active: 3 }));
        assert_eq!(s.dpi, Some(3500));
        assert_eq!(s.polling_hz, Some(1000));
        assert_eq!(s.zones[0].effect, Some(Effect::Static { color: Rgb(0x8b, 0x01, 0x33) }));
    }

    #[test]
    fn dpi_encodings() {
        assert_eq!(dpi_raw(DpiEnc::U8Div50, 1600).unwrap(), 32);
        assert_eq!(dpi_raw(DpiEnc::U8Div50, 7000).unwrap(), 140);
        assert!(dpi_raw(DpiEnc::U8Div50, 12800).is_err());
        assert!(dpi_raw(DpiEnc::U16Div50, 1625).is_err());
        assert_eq!(dpi_raw(DpiEnc::U16Div50, 16000).unwrap(), 320);
        assert_eq!(dpi_raw(DpiEnc::U16Raw, 1600).unwrap(), 1600);

        // Kone AIMO: X e Y, u16 LE DPI/50.
        let l = layout(Family::KoneAimo);
        let mut r = blank(Family::KoneAimo, 2);
        encode_stages(&l, &mut r, &[400, 1600, 16000], 1).unwrap();
        assert_eq!(&r[6..12], &[8, 0, 32, 0, 0x40, 0x01]);
        assert_eq!(&r[16..22], &[8, 0, 32, 0, 0x40, 0x01]);
        assert_eq!((r[4], r[5]), (0x07, 1));
        let s = decode(&l, &r);
        assert_eq!(s.dpi_stages, Some(DpiStages { stages: vec![400, 1600, 16000], active: 1 }));

        // Kone XP Air: valore grezzo, la tabella 17-26 non si tocca.
        let l = layout(Family::KoneXpAir);
        let mut r = blank(Family::KoneXpAir, 0);
        encode_stages(&l, &mut r, &[400, 800, 1200, 1600, 3200], 1).unwrap();
        assert_eq!(&r[7..9], &[0x90, 0x01]);
        assert_eq!(&r[15..17], &[0x80, 0x0C]);
        assert!(r[17..27].iter().all(|&b| b == 0));
        assert_eq!((r[5], r[6]), (0x1F, 1));
    }

    #[test]
    fn kova_stage_edit_preserves_other_bytes() {
        let l = layout(Family::Kova2016);
        let old = kova_default();
        let mut r = old.clone();
        encode_stages(&l, &mut r, &[800, 1600], 0).unwrap();
        fix_checksum(&mut r);
        assert_eq!(&r[6..13], &[0x03, 16, 32, 0x20, 0x46, 0x8C, 0]);
        assert_eq!(&r[13..26], &old[13..26]);
        assert!(checksum_ok(&r));
        validate(&l, &r, 0).unwrap();
    }

    #[test]
    fn restricted_models() {
        // Kone Pro: solo 5 stadi, maschera 0x1F come OpenRGB.
        let l = layout(Family::KonePro);
        let mut r = blank(Family::KonePro, 0);
        assert!(encode_stages(&l, &mut r, &[800, 1600], 0).is_err());
        encode_stages(&l, &mut r, &[400, 800, 1200, 1600, 19000], 4).unwrap();
        assert_eq!(&r[15..17], &380u16.to_le_bytes());
        assert!(r[17..27].iter().all(|&b| b == 0));
        // Kone XP: DPI in sola lettura.
        let l = layout(Family::KoneXp);
        let mut r = blank(Family::KoneXp, 0);
        assert!(encode_stages(&l, &mut r, &[400, 800, 1200, 1600, 3200], 0).is_err());
        // Kone XP Air: nessun "spento".
        let l = layout(Family::KoneXpAir);
        let mut r = blank(Family::KoneXpAir, 0);
        assert!(encode_effect(&l, &mut r, &Effect::Off).is_err());
    }

    #[test]
    fn lighting_roundtrip() {
        let c = Rgb(0x12, 0x34, 0x56);
        for f in [Family::Kova2016, Family::KovaAimo, Family::KoneAimo, Family::KonePro, Family::KoneXp, Family::KoneXpAir] {
            let l = layout(f);
            let mut r = blank(f, 0);
            for e in [Effect::Static { color: c }, Effect::Breathing { color: c }] {
                encode_effect(&l, &mut r, &e).unwrap();
                assert_eq!(decode(&l, &r).zones[0].effect, Some(e), "{f:?}");
            }
            for name in supported_effects(&l) {
                let e = Effect::from_name(name, c).unwrap();
                encode_effect(&l, &mut r, &e).unwrap();
                assert_eq!(decode(&l, &r).zones[0].effect.as_ref().map(|x| x.name()), Some(name), "{f:?}");
            }
        }
        // Kone AIMO: colore negli 11 slot {FF, R, G, B, 0}, tema Roccat disattivato.
        let l = layout(Family::KoneAimo);
        let mut r = blank(Family::KoneAimo, 0);
        r[116] = 0x83;
        encode_effect(&l, &mut r, &Effect::Static { color: c }).unwrap();
        assert_eq!(&r[111..116], &[0xFF, 0x12, 0x34, 0x56, 0x00]);
        assert_eq!((r[33], r[116]), (1, 0x03));
        // Kone XP: i byte prima e dopo l'RGB di ogni slot restano.
        let l = layout(Family::KoneXp);
        let mut r = blank(Family::KoneXp, 0);
        r[36] = 0x14;
        r[41] = 0x64;
        encode_effect(&l, &mut r, &Effect::Breathing { color: c }).unwrap();
        assert_eq!(&r[36..42], &[0x14, 0xFF, 0x12, 0x34, 0x56, 0x64]);
        assert_eq!((r[30], r[31]), (3, 6));
        encode_brightness(&l, &mut r, 0x80).unwrap();
        assert_eq!(decode(&l, &r).zones[0].brightness, Some(0x80));
    }

    #[test]
    fn allowlist_r1() {
        let l = layout(Family::KoneAimo);
        assert!(allowed_r1(&l, &[SELECT, 2, 0x80]));
        assert!(!allowed_r1(&l, &[SELECT, 2, 0x90])); // tasti
        assert!(!allowed_r1(&l, &[SELECT, 7, 0x80]));
        assert!(allowed_r1(&l, &COMMIT_FRAME));
        assert!(!allowed_r1(&l, &[0x0E, 0x06, 0x01, 0x01, 0x00, 0xFF])); // luci dal PC
        assert!(!allowed_r1(&l, &[0x0D, 0, 0]));
        assert!(!allowed_r1(&l, &[0x1B, 0, 0]));
        assert!(!allowed_r1(&l, &[0x07, 0x4B]));
        let mut r = blank(Family::KoneAimo, 0);
        assert!(allowed_r1(&l, &r));
        r[50] ^= 1; // somma sbagliata
        assert!(!allowed_r1(&l, &r));
        // Kova: niente 0x0E (sul Kova è un'altra cosa).
        assert!(!allowed_r1(&layout(Family::Kova2016), &COMMIT_FRAME));
    }

    #[test]
    fn allowlist_r2() {
        assert!(allowed_r2(&r2_frame(CMD_SOFTWARE, OP_WRITE, &SESSION_OPEN)));
        assert!(allowed_r2(&r2_frame(CMD_SOFTWARE, OP_WRITE, &SESSION_CLOSE)));
        assert!(!allowed_r2(&r2_frame(CMD_SOFTWARE, OP_WRITE, &[1, 1, 1, 0xFF]))); // luci dal PC
        assert!(!allowed_r2(&r2_frame(0x49, OP_WRITE, &[0x01]))); // ripristino di fabbrica
        assert!(!allowed_r2(&r2_frame(0x47, OP_WRITE, &[0, 0]))); // tasti
        assert!(allowed_r2(&r2_frame(CMD_MAIN, OP_WRITE, &[3, 4])));
        assert!(!allowed_r2(&r2_frame(CMD_MAIN, OP_WRITE, &[4, 0])));
        assert!(allowed_r2(&r2_frame(CMD_MAIN, OP_READ, &[])));
        assert!(allowed_r2(&r2_frame(CMD_PROFILE_STATE, OP_READ, &[])));
        assert!(!allowed_r2(&r2_frame(CMD_PROFILE_STATE, OP_WRITE, &[0, 5])));
        let mut q = vec![0u8; R2_LEN];
        q[0] = SETTINGS;
        q[3] = 0x04;
        assert!(allowed_r2(&q));
        q[2] = 0x01;
        q[3] = 0x06; // abbinamento del ricevitore
        assert!(!allowed_r2(&q));
        // Ogni pezzo del record 0x46 da 78 byte passa.
        let rec = blank(Family::KoneXpAir, 0);
        assert!(rec.chunks(R2_PAYLOAD).all(|p| allowed_r2(&r2_frame(CMD_MAIN, OP_WRITE, p))));
    }

    #[test]
    fn r2_responses() {
        let mut b = vec![0u8; R2_LEN];
        b[..7].copy_from_slice(&[0x06, 0x01, 0x45, 0x08, 0x02, 0x01, 0x05]);
        assert_eq!(parse_r2(&b, CMD_PROFILE_STATE).unwrap(), vec![1, 5]);
        b[3] = 0x0C;
        assert!(parse_r2(&b, CMD_PROFILE_STATE).is_err());
        b[3] = 0x08;
        assert!(parse_r2(&b, CMD_MAIN).is_err());
    }
}
