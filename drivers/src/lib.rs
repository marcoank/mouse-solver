pub mod asus;
pub mod logitech;
pub mod measure;
pub mod razer;
pub mod steelseries;
pub mod zowie;

use anyhow::{bail, Result};
use hidapi::HidApi;
use mousecfg_core::{Db, DeviceSpec, MouseDriver};

pub use hidapi;

/// Un mouse collegato, supportato o no.
#[derive(Debug, Clone)]
pub struct Found {
    pub vid: u16,
    pub pid: u16,
    pub manufacturer: String,
    pub product: String,
    pub spec: Option<DeviceSpec>,
}

impl Found {
    pub fn display_name(&self) -> String {
        match &self.spec {
            Some(s) => s.name.clone(),
            None if !self.product.is_empty() => format!("{} {}", self.manufacturer, self.product).trim().to_string(),
            None => format!("{:04x}:{:04x}", self.vid, self.pid),
        }
    }
}

/// VID dei marchi che ci interessano anche se il modello non è nel database.
pub const GAMING_VIDS: &[(u16, &str)] = &[
    (0x1532, "Razer"),
    (0x0B05, "ASUS"),
    (0x1038, "SteelSeries"),
    (0x046D, "Logitech"),
    (0x1B1C, "Corsair"),
    // Zowie: VID da verificare col mouse di test (Fase 5).
];

pub fn vendor_name(vid: u16) -> Option<&'static str> {
    GAMING_VIDS.iter().find(|(v, _)| *v == vid).map(|(_, n)| *n)
}

/// Elenca i mouse collegati: tutti quelli nel database, più i dispositivi
/// con un'interfaccia mouse (usage page 0x01, usage 0x02).
pub fn scan(api: &HidApi, db: &Db) -> Vec<Found> {
    let mice: std::collections::HashSet<(u16, u16)> = api
        .device_list()
        .filter(|d| d.usage_page() == 0x01 && d.usage() == 0x02)
        .map(|d| (d.vendor_id(), d.product_id()))
        .collect();
    let mut out: Vec<Found> = Vec::new();
    for d in api.device_list() {
        let (vid, pid) = (d.vendor_id(), d.product_id());
        let manufacturer = d.manufacturer_string().unwrap_or_default().trim().to_string();
        let product = d.product_string().unwrap_or_default().trim().to_string();
        let is_mouse = mice.contains(&(vid, pid));
        let spec = db
            .find_for(vid, pid, &manufacturer, &product)
            .or_else(|| if is_mouse { db.find_generic(vid) } else { None });
        if spec.is_none() && !is_mouse {
            continue;
        }
        if out.iter().any(|f| f.vid == vid && f.pid == pid) {
            continue;
        }
        out.push(Found {
            vid,
            pid,
            manufacturer,
            product,
            spec: spec.cloned(),
        });
    }
    out.sort_by_key(|f| (f.spec.is_none(), f.vid, f.pid));
    out
}

/// Apre il driver per un mouse trovato da `scan` (usa il suo VID/PID reale, che può essere un alias).
pub fn open(api: &HidApi, found: &Found) -> Result<Box<dyn MouseDriver>> {
    let Some(spec) = &found.spec else { bail!("{} non è ancora supportato", found.display_name()) };
    let (vid, pid) = (found.vid, found.pid);
    match spec.driver.as_str() {
        "razer" => Ok(Box::new(razer::Razer::open(api, spec, vid, pid)?)),
        "asus" => Ok(Box::new(asus::Asus::open(api, spec, vid, pid)?)),
        "steelseries" => Ok(Box::new(steelseries::SteelSeries::open(api, spec, vid, pid)?)),
        "zowie" => Ok(Box::new(zowie::Driverless::new(spec, vid, pid))),
        "logitech" => Ok(Box::new(logitech::Logitech::open(api, spec, vid, pid)?)),
        other => bail!("driver '{other}' non ancora implementato"),
    }
}

/// Apre il primo dispositivo HID `vid:pid` che soddisfa `ok`, provando prima le interfacce `preferred`.
/// `ok` deve usare solo comandi di lettura.
pub(crate) fn open_first(
    api: &HidApi,
    vid: u16,
    pid: u16,
    preferred: impl Fn(&hidapi::DeviceInfo) -> bool,
    ok: impl Fn(&hidapi::HidDevice) -> bool,
) -> Option<hidapi::HidDevice> {
    let all: Vec<_> = api.device_list().filter(|d| d.vendor_id() == vid && d.product_id() == pid).collect();
    let ordered = all.iter().filter(|d| preferred(d)).chain(all.iter().filter(|d| !preferred(d)));
    for info in ordered {
        if let Ok(dev) = info.open_device(api) {
            if ok(&dev) {
                return Some(dev);
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use mousecfg_core::Db;

    /// Ogni file devices/*.toml deve essere leggibile dal suo driver.
    #[test]
    fn every_device_file_matches_its_driver() {
        let db = Db::load().unwrap();
        let mut errors = Vec::new();
        for d in &db.devices {
            let res = match d.driver.as_str() {
                "razer" => d.section::<crate::razer::RazerConfig>("razer").map(|_| ()),
                "asus" => d.section::<crate::asus::AsusConfig>("asus").map(|_| ()),
                "steelseries" => d.section::<crate::steelseries::SteelConfig>("steelseries").map(|_| ()),
                "zowie" | "logitech" => Ok(()),
                other => Err(anyhow::anyhow!("driver sconosciuto {other}")),
            };
            if let Err(e) = res {
                errors.push(format!("{}: {e:#}", d.name));
            }
            for z in &d.caps.zones {
                let has_led = match d.driver.as_str() {
                    "razer" => d.extra["razer"].get("leds").and_then(|l| l.get(z)).is_some(),
                    "asus" => d.extra["asus"].get("zones").and_then(|l| l.get(z)).is_some(),
                    _ => true,
                };
                if !has_led {
                    errors.push(format!("{}: zona {z} senza ID", d.name));
                }
            }
        }
        // Nessun VID/PID duplicato tra file diversi.
        let mut seen = std::collections::HashMap::new();
        for d in db.devices.iter().filter(|d| !d.any_pid) {
            for id in std::iter::once([d.vid, d.pid]).chain(d.aliases.iter().copied()) {
                if let Some(other) = seen.insert(id, d.name.clone()) {
                    errors.push(format!("{:04x}:{:04x} sia in '{}' che in '{}'", id[0], id[1], other, d.name));
                }
            }
        }
        assert!(errors.is_empty(), "{} errori:\n{}", errors.len(), errors.join("\n"));
        assert!(db.devices.len() > 150, "solo {} modelli", db.devices.len());
    }
}
