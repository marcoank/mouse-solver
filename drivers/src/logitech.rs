//! Driver Logitech HID++ 2.0, per ora SOLO LETTURA. Protocollo reimplementato leggendo libratbag
//! (src/hidpp20.c, licenza MIT): nessun codice copiato.
//!
//! HID++ 2.0 è autodescrittivo: si chiede al mouse l'indice di ogni "feature" (0x2201 DPI,
//! 0x8060 polling, 0x8100 profili onboard...) e si usano solo quelle presenti. Così un solo driver
//! copre quasi tutti i Logitech G recenti, col cavo o col ricevitore Lightspeed.
//!
//! Scrittura: sui Logitech DPI e polling scritti con 0x2201/0x8060 si perdono allo scollegamento
//! se il mouse usa i profili onboard. Per salvarli davvero bisogna scrivere il profilo nella
//! memoria del mouse (0x8100): da fare quando avremo un Logitech su cui provarlo.

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

/// (feature, funzione) ammesse: solo letture.
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
    (ONBOARD, 2),     // getOnboardMode
    (ONBOARD, 4),     // getCurrentProfile
];

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
                bail!("il mouse risponde errore 0x{:02x}", buf[5]);
            }
            // Errore HID++ 1.0 (dispositivo vecchio o ricevitore senza mouse)
            if buf[2] == 0x8F {
                bail!("dispositivo HID++ 1.0 o mouse spento");
            }
            if buf[2] == feat_idx && buf[3] == req[3] {
                let mut out = [0u8; LONG_LEN];
                out[..n.min(LONG_LEN)].copy_from_slice(&buf[..n.min(LONG_LEN)]);
                return Ok(out);
            }
        }
        bail!("nessuna risposta")
    }

    /// Chiama la funzione `func` della feature `id`, se il mouse la ha.
    fn call(&mut self, id: u16, func: u8, params: &[u8]) -> Result<[u8; LONG_LEN]> {
        if !ALLOWED.contains(&(id, func)) {
            bail!("comando HID++ {id:04x}/{func} bloccato: non è nella lista dei comandi sicuri");
        }
        let idx = match id {
            ROOT => 0,
            _ => self.feature(id)?.with_context(|| format!("il mouse non ha la funzione HID++ 0x{id:04x}"))?,
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

pub struct Logitech {
    hidpp: Hidpp,
    spec: DeviceSpec,
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
                    let mut l = Logitech { hidpp: h, spec };
                    l.fill_caps();
                    return Ok(l);
                }
            }
        }
        bail!("mouse Logitech non raggiungibile via HID++ 2.0 (se è wireless, muovilo per svegliarlo; chiudi G HUB)")
    }

    /// Le capacità vengono dal mouse stesso, non dal file TOML.
    fn fill_caps(&mut self) {
        if let Ok((min, max, step, list)) = self.dpi_list() {
            let caps = &mut self.spec.caps;
            (caps.dpi_min, caps.dpi_max, caps.dpi_step) = (min, max, step.max(1));
            caps.dpi_values = list;
        }
        if let Ok(rates) = self.rate_list() {
            self.spec.caps.polling = rates;
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
        let (min, max) = (*list.iter().min().context("lista DPI vuota")?, *list.iter().max().unwrap());
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

    fn set_dpi_stages(&mut self, _: &[u16], _: usize) -> Result<()> {
        bail!("{}", READ_ONLY)
    }

    fn set_polling(&mut self, _: u16) -> Result<()> {
        bail!("{}", READ_ONLY)
    }

    fn set_lighting(&mut self, _: &str, _: &Effect) -> Result<()> {
        bail!("{}", READ_ONLY)
    }

    fn set_brightness(&mut self, _: &str, _: u8) -> Result<()> {
        bail!("{}", READ_ONLY)
    }
}

const READ_ONLY: &str = "sui Logitech per ora mostriamo solo le impostazioni: la scrittura nei profili \
del mouse arriverà quando potremo provarla su un mouse vero";

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
        notes: None, aliases: vec![], match_names: vec![], any_pid: false, extra: Default::default(),
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
                "state": l.read_state().ok(),
            })
        }
    }
}
