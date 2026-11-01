use crate::types::{Capabilities, SupportStatus};
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::Path;

include!(concat!(env!("OUT_DIR"), "/builtin_devices.rs"));

/// Un modello di mouse, letto da devices/*.toml.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeviceSpec {
    pub name: String,
    pub vid: u16,
    pub pid: u16,
    pub driver: String,
    pub status: SupportStatus,
    pub caps: Capabilities,
    #[serde(default)]
    pub notes: Option<String>,
    /// Le stesse note in inglese.
    #[serde(default)]
    pub notes_en: Option<String>,
    /// Altre coppie [vid, pid] dello stesso modello (revisioni, edizioni speciali).
    #[serde(default)]
    pub aliases: Vec<[u16; 2]>,
    /// Se VID/PID non corrispondono: parole cercate nel nome del produttore o del prodotto.
    #[serde(default)]
    pub match_names: Vec<String>,
    /// Vale per tutti i mouse con questo VID (driver che scoprono le capacità dal mouse, es. Logitech).
    #[serde(default)]
    pub any_pid: bool,
    /// Sezioni specifiche del driver (es. [razer]), lette dal driver stesso.
    #[serde(flatten)]
    pub extra: toml::Table,
}

impl DeviceSpec {
    /// Note nella lingua dell'interfaccia (in italiano se manca la traduzione).
    pub fn notes_text(&self) -> Option<&str> {
        match crate::i18n::is_en() {
            true => self.notes_en.as_deref().or(self.notes.as_deref()),
            false => self.notes.as_deref(),
        }
    }

    /// Legge la sezione del driver, es. `spec.section::<RazerConfig>("razer")`.
    pub fn section<T: serde::de::DeserializeOwned>(&self, key: &str) -> Result<T> {
        let v = self.extra.get(key).with_context(|| crate::tf!("{}: manca la sezione [{key}]", "{}: section [{key}] is missing", self.name))?;
        v.clone().try_into().with_context(|| crate::tf!("{}: sezione [{key}] non valida", "{}: invalid section [{key}]", self.name))
    }
}

#[derive(Debug, Default, Clone)]
pub struct Db {
    pub devices: Vec<DeviceSpec>,
}

impl Db {
    /// Modelli inclusi nell'exe + eventuali file in `devices/` accanto all'exe
    /// (per provare un TOML nuovo senza ricompilare; vincono su quelli inclusi).
    pub fn load() -> Result<Db> {
        let mut db = Db::default();
        for (name, text) in BUILTIN {
            db.add_toml(text).with_context(|| format!("devices/{name}"))?;
        }
        if let Some(dir) = std::env::current_exe().ok().and_then(|p| p.parent().map(|p| p.join("devices"))) {
            db.add_dir(&dir)?;
        }
        Ok(db)
    }

    pub fn add_dir(&mut self, dir: &Path) -> Result<()> {
        let Ok(entries) = std::fs::read_dir(dir) else { return Ok(()) };
        for e in entries.flatten() {
            let p = e.path();
            if p.extension().is_some_and(|x| x == "toml") {
                let text = std::fs::read_to_string(&p)?;
                self.add_toml(&text).with_context(|| p.display().to_string())?;
            }
        }
        Ok(())
    }

    pub fn add_toml(&mut self, text: &str) -> Result<()> {
        let spec: DeviceSpec = toml::from_str(text)?;
        self.devices.retain(|d| !(d.vid == spec.vid && d.pid == spec.pid && d.name == spec.name));
        self.devices.push(spec);
        Ok(())
    }

    pub fn find(&self, vid: u16, pid: u16) -> Option<&DeviceSpec> {
        self.devices
            .iter()
            .find(|d| !d.any_pid && ((d.vid == vid && d.pid == pid) || d.aliases.contains(&[vid, pid])))
    }

    /// Driver generico per tutto il VID (da usare solo per dispositivi che sono mouse).
    pub fn find_generic(&self, vid: u16) -> Option<&DeviceSpec> {
        self.devices.iter().find(|d| d.any_pid && d.vid == vid)
    }

    /// Come `find`, poi ripiega sulle parole chiave nel nome del dispositivo.
    pub fn find_for(&self, vid: u16, pid: u16, manufacturer: &str, product: &str) -> Option<&DeviceSpec> {
        self.find(vid, pid).or_else(|| {
            let text = format!("{manufacturer} {product}").to_lowercase();
            self.devices.iter().find(|d| d.match_names.iter().any(|m| text.contains(&m.to_lowercase())))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtin_devices_parse() {
        let db = Db::load().unwrap();
        assert!(!db.devices.is_empty());
        for d in &db.devices {
            assert!(d.caps.dpi_min <= d.caps.dpi_max, "{}", d.name);
            assert!(d.caps.min_stages <= d.caps.max_stages, "{}", d.name);
        }
    }
}
