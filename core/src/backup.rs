use crate::types::{DeviceInfo, MouseState};
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Backup {
    pub app_version: String,
    pub created_unix: u64,
    pub device: DeviceInfo,
    pub state: MouseState,
}

/// %APPDATA%\mouse-cfg\backups
pub fn dir() -> PathBuf {
    let base = std::env::var_os("APPDATA").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("."));
    base.join("mouse-cfg").join("backups")
}

pub fn save(device: &DeviceInfo, state: &MouseState) -> Result<PathBuf> {
    let now = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    let b = Backup { app_version: env!("CARGO_PKG_VERSION").into(), created_unix: now, device: device.clone(), state: state.clone() };
    let d = dir();
    std::fs::create_dir_all(&d).with_context(|| d.display().to_string())?;
    let path = d.join(format!("{:04x}_{:04x}_{now}.json", device.vid, device.pid));
    std::fs::write(&path, serde_json::to_string_pretty(&b)?)?;
    Ok(path)
}

pub fn load(path: &Path) -> Result<Backup> {
    let text = std::fs::read_to_string(path).with_context(|| path.display().to_string())?;
    Ok(serde_json::from_str(&text)?)
}

/// Backup di un modello, dal più recente.
pub fn list(vid: u16, pid: u16) -> Vec<PathBuf> {
    let prefix = format!("{vid:04x}_{pid:04x}_");
    let mut v: Vec<PathBuf> = std::fs::read_dir(dir())
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.file_name().is_some_and(|n| n.to_string_lossy().starts_with(&prefix)))
        .collect();
    v.sort();
    v.reverse();
    v
}
