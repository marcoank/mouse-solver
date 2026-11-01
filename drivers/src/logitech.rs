//! Driver Logitech HID++ 2.0. Protocollo reimplementato leggendo libratbag
//! (src/hidpp20.c, licenza MIT): nessun codice copiato.
//!
//! HID++ 2.0 è autodescrittivo: si chiede al mouse l'indice di ogni "feature" (0x2201 DPI,
//! 0x8060 polling, 0x8100 profili onboard...) e si usano solo quelle presenti. Così un solo driver
//! copre quasi tutti i Logitech G recenti, col cavo o col ricevitore Lightspeed.
//!
//! Scrittura: DPI, polling e luci restano nel mouse solo se scritti nel profilo onboard (feature
//! 0x8100, settori da 256 byte con CRC). A differenza di libratbag, che ricostruisce i profili da
//! zero e cancella nome, G-shift e altre impostazioni di G HUB, qui si fa lettura-modifica-scrittura:
//! si cambiano solo i byte di DPI, polling e LED del profilo attivo, si ricalcola il CRC e si
//! rilegge il settore per verifica. Si scrivono solo il settore del profilo utente (1..N) e, se
//! serve, la directory (0); mai la ROM (0x01xx). Senza 0x8100 o con settori diversi da 256 byte
//! il mouse resta in sola lettura.

use anyhow::{bail, Context, Result};
use hidapi::{HidApi, HidDevice};
use mousecfg_core::*;
use std::time::{Duration, Instant};

const LONG: u8 = 0x11;
const LONG_LEN: usize = 20;
const SW_ID: u8 = 0x0A;

const ROOT: u16 = 0x0000;
const FW_INFO: u16 = 0x0003;
const DEVICE_NAME: u16 = 0x0005;
const ADJ_DPI: u16 = 0x2201;
const REPORT_RATE: u16 = 0x8060;
const ONBOARD: u16 = 0x8100;

/// (feature, funzione) ammesse. Le scritture in memoria passano solo da `write_sector`,
/// che controlla il numero di settore.
const ALLOWED: &[(u16, u8)] = &[
    (ROOT, 0),        // getFeature
    (ROOT, 1),        // getProtocolVersion (ping)
    (FW_INFO, 0),     // getEntityCount
    (FW_INFO, 1),     // getFwInfo
    (DEVICE_NAME, 0), // getCount
    (DEVICE_NAME, 1), // getName
    (ADJ_DPI, 0),     // getSensorCount
    (ADJ_DPI, 1),     // getSensorDpiList
    (ADJ_DPI, 2),     // getSensorDpi
    (REPORT_RATE, 0), // getReportRateList
    (REPORT_RATE, 1), // getReportRate
    (ONBOARD, 0),     // getProfilesDescr
    (ONBOARD, 1),     // setOnboardMode
    (ONBOARD, 2),     // getOnboardMode
    (ONBOARD, 4),     // getCurrentProfile
    (ONBOARD, 5),     // memoryRead
    (ONBOARD, 6),     // memoryAddrWrite
    (ONBOARD, 7),     // memoryWrite
    (ONBOARD, 8),     // memoryWriteEnd
    (ONBOARD, 0xB),   // getCurrentDpiIndex
    (ONBOARD, 0xC),   // setCurrentDpiIndex
];

const SECTOR: usize = 256;
const LED_MODES: [u8; 8] = [0x00, 0x01, 0x03, 0x04, 0x05, 0x0A, 0x0B, 0x0C];

/// CRC-16/CCITT-FALSE (poly 0x1021, init 0xFFFF), come nei settori dei profili Logitech.
pub fn crc_ccitt(data: &[u8]) -> u16 {
    let mut crc: u16 = 0xFFFF;
    for &b in data {
        crc ^= (b as u16) << 8;
        for _ in 0..8 {
            crc = if crc & 0x8000 != 0 { (crc << 1) ^ 0x1021 } else { crc << 1 };
        }
    }
    crc
}

/// Informazioni sui profili onboard (getProfilesDescr) e posizione del profilo attivo.
#[derive(Debug, Clone)]
struct Onboard {
    profile_count: u8,
    sector_count: u8,
    /// Indice (da 0) del profilo attivo.
    active: usize,
    /// Directory valida (CRC giusto) già scritta da un software.
    dir_valid: bool,
    /// Settore da cui leggere il profilo attivo (utente o ROM se non ancora scritto).
    source: u16,
}

pub struct Hidpp {
    dev: HidDevice,
    index: u8,
    features: std::collections::HashMap<u16, u8>,
}

impl Hidpp {
    fn raw(&self, feat_idx: u8, func: u8, params: &[u8]) -> Result<[u8; LONG_LEN]> {
        let mut req = [0u8; LONG_LEN];
        req[0] = LONG;
        req[1] = self.index;
        req[2] = feat_idx;
        req[3] = (func << 4) | SW_ID;
        req[4..4 + params.len()].copy_from_slice(params);
        let mut junk = [0u8; 64];
        while self.dev.read_timeout(&mut junk, 0).unwrap_or(0) > 0 {}
        self.dev.write(&req)?;
        let deadline = Instant::now() + Duration::from_millis(500);
        while Instant::now() < deadline {
            let mut buf = [0u8; 64];
            let n = self.dev.read_timeout(&mut buf, 100)?;
            if n < 4 || buf[1] != self.index {
                continue;
            }
            // Errore HID++ 2.0: [.., 0xFF, feat_idx, fn|sw, codice]
            if buf[2] == 0xFF && buf[3] == feat_idx && buf[4] == req[3] {
                bail!(tf!("il mouse risponde errore 0x{:02x}", "the mouse replies with error 0x{:02x}", buf[5]));
            }
            // Errore HID++ 1.0 (dispositivo vecchio o ricevitore senza mouse)
            if buf[2] == 0x8F {
                bail!(t!("dispositivo HID++ 1.0 o mouse spento", "HID++ 1.0 device or mouse turned off"));
            }
            if buf[2] == feat_idx && buf[3] == req[3] {
                let mut out = [0u8; LONG_LEN];
                out[..n.min(LONG_LEN)].copy_from_slice(&buf[..n.min(LONG_LEN)]);
                return Ok(out);
            }
        }
        bail!(t!("nessuna risposta", "no response"))
    }

    /// Chiama la funzione `func` della feature `id`, se il mouse la ha.
    fn call(&mut self, id: u16, func: u8, params: &[u8]) -> Result<[u8; LONG_LEN]> {
        if !ALLOWED.contains(&(id, func)) {
            bail!(tf!("comando HID++ {id:04x}/{func} bloccato: non è nella lista dei comandi sicuri", "HID++ command {id:04x}/{func} blocked: not in the list of safe commands"));
        }
        let idx = match id {
            ROOT => 0,
            _ => self.feature(id)?.with_context(|| tf!("il mouse non ha la funzione HID++ 0x{id:04x}", "the mouse does not have HID++ feature 0x{id:04x}"))?,
        };
        self.raw(idx, func, params)
    }

    fn feature(&mut self, id: u16) -> Result<Option<u8>> {
        if let Some(&i) = self.features.get(&id) {
            return Ok((i != 0).then_some(i));
        }
        let r = self.raw(0, 0, &id.to_be_bytes())?;
        self.features.insert(id, r[4]);
        Ok((r[4] != 0).then_some(r[4]))
    }
}

impl Hidpp {
    fn read_sector(&mut self, sector: u16) -> Result<Vec<u8>> {
        let mut out = vec![0u8; SECTOR];
        for off in (0..SECTOR).step_by(16) {
            let [sh, sl] = sector.to_be_bytes();
            let [oh, ol] = (off as u16).to_be_bytes();
            let r = self.call(ONBOARD, 5, &[sh, sl, oh, ol])?;
            out[off..off + 16].copy_from_slice(&r[4..20]);
        }
        Ok(out)
    }

    /// Scrive un settore intero. Ammessi solo la directory (0) e i profili utente (1..=profili).
    fn write_sector(&mut self, sector: u16, data: &[u8], profile_count: u8) -> Result<()> {
        if sector > profile_count as u16 || data.len() != SECTOR {
            bail!(tf!("scrittura nel settore 0x{sector:04x} bloccata: non è un profilo utente", "write to sector 0x{sector:04x} blocked: it is not a user profile"));
        }
        let [sh, sl] = sector.to_be_bytes();
        let [ch, cl] = (SECTOR as u16).to_be_bytes();
        self.call(ONBOARD, 6, &[sh, sl, 0, 0, ch, cl])?;
        for chunk in data.chunks(16) {
            self.call(ONBOARD, 7, chunk)?;
        }
        self.call(ONBOARD, 8, &[])?;
        Ok(())
    }

    fn onboard(&mut self) -> Result<Onboard> {
        let d = self.call(ONBOARD, 0, &[])?;
        let p = &d[4..];
        let sector_size = u16::from_be_bytes([p[7], p[8]]) as usize;
        if p[0] != 0x01 || !(0x01..=0x05).contains(&p[1]) || p[2] != 0x01 || sector_size != SECTOR || p[3] == 0 {
            bail!(tf!(
                "formato dei profili onboard non gestito (memoria {:02x}, profilo {:02x}, settore {sector_size} byte)",
                "onboard profile format not supported (memory {:02x}, profile {:02x}, sector {sector_size} bytes)",
                p[0],
                p[1]
            ));
        }
        let (profile_count, rom_count, sector_count) = (p[3], p[4], p[6]);
        let dir = self.read_sector(0x0000).ok();
        let dir_valid = dir.as_ref().is_some_and(|d| crc_ccitt(&d[..SECTOR - 2]) == u16::from_be_bytes([d[SECTOR - 2], d[SECTOR - 1]]));

        // Profilo attivo: getCurrentProfile restituisce l'indirizzo del settore (o un indice).
        let cur = self.call(ONBOARD, 4, &[])?;
        let addr = u16::from_be_bytes([cur[4], cur[5]]);
        let mut active = (addr & 0xFF).saturating_sub(1) as usize;
        if dir_valid {
            if let Some(i) = dir.as_ref().unwrap().chunks(4).take(profile_count as usize).position(|e| u16::from_be_bytes([e[0], e[1]]) == addr) {
                active = i;
            }
        }
        let active = active.min(profile_count as usize - 1);
        let rom = 0x0100 + if (active as u8) < rom_count { active as u16 + 1 } else { 1 };
        let source = if dir_valid {
            let user = active as u16 + 1;
            match self.read_sector(user) {
                Ok(s) if crc_ccitt(&s[..SECTOR - 2]) == u16::from_be_bytes([s[SECTOR - 2], s[SECTOR - 1]]) => user,
                _ => rom,
            }
        } else {
            rom
        };
        Ok(Onboard { profile_count, sector_count, active, dir_valid, source })
    }
}

pub struct Logitech {
    hidpp: Hidpp,
    spec: DeviceSpec,
    onboard: Option<Onboard>,
    /// Perché il mouse è in sola lettura (se lo è).
    read_only: Option<String>,
}

impl Logitech {
    pub fn open(api: &HidApi, spec: &DeviceSpec, vid: u16, pid: u16) -> Result<Logitech> {
        // Collezione "lunga" del produttore (usage page 0xFF00, usage 0x02).
        let infos: Vec<_> = api
            .device_list()
            .filter(|d| d.vendor_id() == vid && d.product_id() == pid && d.usage_page() == 0xFF00 && d.usage() == 0x02)
            .collect();
        for info in infos {
            let Ok(dev) = info.open_device(api) else { continue };
            let mut h = Hidpp { dev, index: 0xFF, features: Default::default() };
            // 0xFF = col cavo; 1..6 = dispositivi collegati al ricevitore
            for index in [0xFF, 1, 2, 3] {
                h.index = index;
                h.features.clear();
                if h.raw(0, 1, &[0, 0, 0x5A]).is_ok_and(|r| r[6] == 0x5A) && h.feature(ADJ_DPI).ok().flatten().is_some() {
                    let mut spec = spec.clone();
                    (spec.vid, spec.pid) = (vid, pid);
                    let mut l = Logitech { hidpp: h, spec, onboard: None, read_only: None };
                    match l.hidpp.feature(ONBOARD) {
                        Ok(Some(_)) => match l.hidpp.onboard() {
                            Ok(o) => l.onboard = Some(o),
                            Err(e) => l.read_only = Some(format!("{e:#}")),
                        },
                        _ => l.read_only = Some(t!("il mouse non ha profili onboard (0x8100)", "the mouse has no onboard profiles (0x8100)").into()),
                    }
                    l.fill_caps();
                    return Ok(l);
                }
            }
        }
        bail!(t!(
            "mouse Logitech non raggiungibile via HID++ 2.0 (se è wireless, muovilo per svegliarlo; chiudi G HUB)",
            "Logitech mouse not reachable via HID++ 2.0 (if it's wireless, move it to wake it up; close G HUB)"
        ))
    }

    /// Le capacità vengono dal mouse stesso, non dal file TOML.
    fn fill_caps(&mut self) {
        if let Ok((min, max, step, list)) = self.dpi_list() {
            let caps = &mut self.spec.caps;
            (caps.dpi_min, caps.dpi_max, caps.dpi_step) = (min, max, step.max(1));
            caps.dpi_values = list;
        }
        if let Ok(rates) = self.rate_list() {
            // nel profilo il polling è in millisecondi interi: niente oltre 1000 Hz
            self.spec.caps.polling = rates.into_iter().filter(|&hz| hz <= 1000).collect();
        }
        if self.onboard.is_some() {
            self.spec.caps.min_stages = 1;
            self.spec.caps.max_stages = 5;
            self.spec.caps.active_stage_settable = true;
            if let Ok(sec) = self.profile() {
                self.spec.caps.zones = (0..2).filter(|&i| LED_MODES.contains(&sec[208 + 11 * i])).map(|i| format!("led{}", i + 1)).collect();
                self.spec.caps.effects = if self.spec.caps.zones.is_empty() {
                    Vec::new()
                } else {
                    ["static", "breathing", "spectrum", "off"].map(String::from).to_vec()
                };
            }
        }
        if let Ok(name) = self.name() {
            self.spec.name = format!("Logitech {}", name.trim_start_matches("Logitech").trim());
        }
    }

    /// (min, max, passo, elenco esplicito se il mouse non usa un passo)
    fn dpi_list(&mut self) -> Result<(u16, u16, u16, Vec<u16>)> {
        let r = self.hidpp.call(ADJ_DPI, 1, &[0])?;
        let (mut list, mut step) = (Vec::new(), 0);
        let mut i = 5;
        while i + 1 < LONG_LEN {
            let v = u16::from_be_bytes([r[i], r[i + 1]]);
            if v == 0 {
                break;
            }
            if v > 0xE000 {
                step = v - 0xE000;
            } else {
                list.push(v);
            }
            i += 2;
        }
        let (min, max) = (*list.iter().min().context(t!("lista DPI vuota", "empty DPI list"))?, *list.iter().max().unwrap());
        Ok((min, max, step, if step > 0 { Vec::new() } else { list }))
    }

    fn rate_list(&mut self) -> Result<Vec<u16>> {
        let flags = self.hidpp.call(REPORT_RATE, 0, &[])?[4];
        let mut v: Vec<u16> = (0..8).filter(|b| flags & (1 << b) != 0).map(|b| 1000 / (b + 1) as u16).collect();
        v.sort();
        Ok(v)
    }

    fn name(&mut self) -> Result<String> {
        let len = self.hidpp.call(DEVICE_NAME, 0, &[])?[4] as usize;
        let mut out = Vec::new();
        while out.len() < len.min(64) {
            let r = self.hidpp.call(DEVICE_NAME, 1, &[out.len() as u8])?;
            let chunk: Vec<u8> = r[4..].iter().copied().take_while(|&b| b != 0).collect();
            if chunk.is_empty() {
                break;
            }
            out.extend(chunk);
        }
        out.truncate(len);
        Ok(String::from_utf8_lossy(&out).into_owned())
    }
}

impl Logitech {
    fn profile(&mut self) -> Result<Vec<u8>> {
        let o = self.onboard.clone().context(t!("profili onboard non disponibili", "onboard profiles not available"))?;
        self.hidpp.read_sector(o.source)
    }

    fn led_offset(zone: &str) -> Result<usize> {
        match zone {
            "led1" => Ok(208),
            "led2" => Ok(219),
            _ => bail!(tf!("zona '{zone}' sconosciuta", "unknown zone '{zone}'")),
        }
    }

    /// Lettura-modifica-scrittura del profilo attivo, con CRC e verifica.
    fn modify(&mut self, change: impl FnOnce(&mut [u8]) -> Result<()>) -> Result<()> {
        if let Some(why) = &self.read_only {
            bail!(tf!("su questo mouse la scrittura non è possibile: {why}", "writing is not possible on this mouse: {why}"));
        }
        let o = self.onboard.clone().context(t!("profili onboard non disponibili", "onboard profiles not available"))?;
        if self.hidpp.call(ONBOARD, 2, &[])?[4] != 0x01 {
            // I profili salvati valgono solo in modalità onboard (libratbag fa lo stesso).
            self.hidpp.call(ONBOARD, 1, &[0x01])?;
        }
        let mut sec = self.hidpp.read_sector(o.source)?;
        change(&mut sec)?;
        let crc = crc_ccitt(&sec[..SECTOR - 2]);
        sec[SECTOR - 2..].copy_from_slice(&crc.to_be_bytes());
        let target = o.active as u16 + 1;
        if o.sector_count > 0 && target >= o.sector_count as u16 {
            bail!(tf!("settore {target} fuori dalla memoria del mouse", "sector {target} is outside the mouse memory"));
        }
        self.hidpp.write_sector(target, &sec, o.profile_count)?;
        if self.hidpp.read_sector(target)? != sec {
            bail!(t!("verifica fallita: il profilo riletto dal mouse è diverso da quello scritto", "check failed: the profile read back from the mouse differs from the one written"));
        }
        if !o.dir_valid {
            // Primo salvataggio: la directory elenca i profili utente, attivo solo quello scritto.
            let mut dir = vec![0xFFu8; SECTOR];
            for i in 0..o.profile_count as usize {
                dir[4 * i..4 * i + 4].copy_from_slice(&[0x00, i as u8 + 1, (i == o.active) as u8, 0x00]);
            }
            let end = 4 * o.profile_count as usize;
            dir[end..end + 4].copy_from_slice(&[0xFF, 0xFF, 0x00, 0x00]);
            let crc = crc_ccitt(&dir[..SECTOR - 2]);
            dir[SECTOR - 2..].copy_from_slice(&crc.to_be_bytes());
            self.hidpp.write_sector(0x0000, &dir, o.profile_count)?;
        }
        let mut o = o;
        o.dir_valid = true;
        o.source = target;
        self.onboard = Some(o);
        Ok(())
    }

    fn decode_led(b: &[u8]) -> Effect {
        let color = Rgb(b[1], b[2], b[3]);
        match b[0] {
            0x00 => Effect::Off,
            0x01 => Effect::Static { color },
            0x03 => Effect::Spectrum,
            0x0A => Effect::Breathing { color },
            c => Effect::Unknown { code: c },
        }
    }
}

impl MouseDriver for Logitech {
    fn spec(&self) -> &DeviceSpec {
        &self.spec
    }

    fn identify(&mut self) -> Result<DeviceInfo> {
        let firmware = self.hidpp.call(FW_INFO, 1, &[0]).ok().map(|r| {
            let prefix = String::from_utf8_lossy(&r[5..8]).trim_matches(char::from(0)).to_string();
            format!("{prefix} {:x}.{:02x}", r[8], r[9])
        });
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
        if self.onboard.is_some() {
            if let Ok(sec) = self.profile() {
                let slots: Vec<u16> = (0..5).map(|i| u16::from_le_bytes([sec[3 + 2 * i], sec[4 + 2 * i]])).collect();
                let live = self.hidpp.call(ONBOARD, 0xB, &[]).ok().map(|r| r[4] as usize).unwrap_or(sec[1] as usize);
                let stages: Vec<u16> = slots.iter().copied().filter(|&d| d != 0).collect();
                let active = slots.iter().take(live.min(4) + 1).filter(|&&d| d != 0).count().saturating_sub(1);
                let zones = self
                    .spec
                    .caps
                    .zones
                    .clone()
                    .into_iter()
                    .map(|z| {
                        let o = Self::led_offset(&z).unwrap();
                        ZoneState { zone: z, effect: Some(Self::decode_led(&sec[o..o + 11])), brightness: None }
                    })
                    .collect();
                return Ok(MouseState {
                    dpi: stages.get(active).copied(),
                    dpi_stages: (!stages.is_empty()).then(|| DpiStages { stages, active }),
                    polling_hz: (sec[0] > 0).then(|| 1000 / sec[0] as u16),
                    zones,
                    cached: false,
                });
            }
        }
        let dpi = self.hidpp.call(ADJ_DPI, 2, &[0]).ok().map(|r| u16::from_be_bytes([r[5], r[6]]));
        let polling_hz = self.hidpp.call(REPORT_RATE, 1, &[0]).ok().map(|r| r[4]).filter(|&ms| ms > 0).map(|ms| 1000 / ms as u16);
        Ok(MouseState {
            dpi,
            dpi_stages: dpi.map(|d| DpiStages { stages: vec![d], active: 0 }),
            polling_hz,
            zones: Vec::new(),
            cached: false,
        })
    }

    fn set_dpi_stages(&mut self, stages: &[u16], active: usize) -> Result<()> {
        self.spec.caps.check_dpi_stages(stages, active)?;
        let stages = stages.to_vec();
        self.modify(|sec| {
            for i in 0..5 {
                let d = stages.get(i).copied().unwrap_or(0);
                sec[3 + 2 * i..5 + 2 * i].copy_from_slice(&d.to_le_bytes());
            }
            sec[1] = active as u8; // stadio predefinito al caricamento del profilo
            Ok(())
        })?;
        // stadio attivo subito, senza aspettare di ricollegare il mouse
        let _ = self.hidpp.call(ONBOARD, 0xC, &[active as u8]);
        Ok(())
    }

    fn set_polling(&mut self, hz: u16) -> Result<()> {
        self.spec.caps.check_polling(hz)?;
        if hz > 1000 || 1000 % hz != 0 {
            bail!(tf!("polling {hz} Hz non salvabile nel profilo", "polling {hz} Hz cannot be saved in the profile"));
        }
        self.modify(|sec| {
            sec[0] = (1000 / hz) as u8;
            Ok(())
        })
    }

    fn set_lighting(&mut self, zone: &str, effect: &Effect) -> Result<()> {
        self.spec.caps.check_effect(zone, effect)?;
        let o = Self::led_offset(zone)?;
        let mut led = [0u8; 11];
        match *effect {
            Effect::Off => {}
            Effect::Static { color } => led[..4].copy_from_slice(&[0x01, color.0, color.1, color.2]),
            Effect::Breathing { color } => {
                led[..4].copy_from_slice(&[0x0A, color.0, color.1, color.2]);
                led[4..6].copy_from_slice(&4000u16.to_be_bytes()); // periodo in ms
            }
            Effect::Spectrum => {
                led[0] = 0x03;
                led[6..8].copy_from_slice(&10000u16.to_be_bytes());
            }
            ref e => bail!(tf!("effetto '{}' non gestito sui Logitech", "effect '{}' not supported on Logitech mice", e.name())),
        }
        self.modify(|sec| {
            sec[o..o + 11].copy_from_slice(&led);
            sec[o + 22..o + 33].copy_from_slice(&led); // copia per la modalità G-shift, come libratbag
            Ok(())
        })
    }

    fn set_brightness(&mut self, _: &str, _: u8) -> Result<()> {
        bail!(t!("luminosità non ancora gestita sui Logitech: usa un colore più scuro", "brightness not supported yet on Logitech mice: use a darker color"))
    }
}

/// Probe di sola lettura per il kit: elenco delle feature HID++ presenti (utile per il reverse engineering).
pub fn probe(api: &HidApi, vid: u16, pid: u16) -> serde_json::Value {
    use serde_json::json;
    const KNOWN: &[(u16, &str)] = &[
        (0x0001, "FeatureSet"), (0x0003, "FirmwareInfo"), (0x0005, "DeviceName"), (0x1000, "Battery"),
        (0x1004, "UnifiedBattery"), (0x1B04, "ReprogControls"), (0x2201, "AdjustableDPI"),
        (0x2202, "ExtendedAdjustableDPI"), (0x8060, "ReportRate"), (0x8061, "ExtendedReportRate"),
        (0x8070, "ColorLEDEffects"), (0x8071, "RGBEffects"), (0x8100, "OnboardProfiles"),
    ];
    let spec_like = DeviceSpec {
        name: String::new(), vid, pid, driver: "logitech".into(), status: SupportStatus::Experimental,
        caps: Capabilities { dpi_min: 0, dpi_max: 0, dpi_step: 1, dpi_values: vec![], min_stages: 1, max_stages: 1,
            active_stage_settable: false, polling: vec![], zones: vec![], effects: vec![] },
        notes: None, notes_en: None, aliases: vec![], match_names: vec![], any_pid: false, extra: Default::default(),
    };
    match Logitech::open(api, &spec_like, vid, pid) {
        Err(e) => json!({ "error": e.to_string() }),
        Ok(mut l) => {
            let mut feats = serde_json::Map::new();
            for &(id, name) in KNOWN {
                if let Ok(Some(i)) = l.hidpp.feature(id) {
                    feats.insert(format!("0x{id:04x}"), json!({ "name": name, "index": i }));
                }
            }
            json!({
                "device_index": format!("0x{:02x}", l.hidpp.index),
                "name": l.spec.name,
                "features": feats,
                "dpi_range": [l.spec.caps.dpi_min, l.spec.caps.dpi_max, l.spec.caps.dpi_step],
                "polling": l.spec.caps.polling,
                "onboard": l.onboard.as_ref().map(|o| format!("{o:?}")),
                "read_only": l.read_only,
                "state": l.read_state().ok(),
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::crc_ccitt;

    #[test]
    fn crc_matches_libratbag_vectors() {
        assert_eq!(crc_ccitt(b"123456789"), 0x29B1);
        assert_eq!(crc_ccitt(&[0xFF; 254]), 0x0202);
        assert_eq!(crc_ccitt(&[0x00; 254]), 0x592D);
    }
}
