//! Ultimi valori scritti, per i mouse che non permettono di rileggere le impostazioni
//! (es. SteelSeries Rival 310). Salvati in %APPDATA%\mouse-solver\last\<vid>_<pid>.json.

use crate::types::MouseState;
use std::path::PathBuf;

fn path(vid: u16, pid: u16) -> PathBuf {
    crate::backup::dir().with_file_name("last").join(format!("{vid:04x}_{pid:04x}.json"))
}

/// Stato salvato, marcato `cached = true`. Se non c'è niente: stato vuoto.
pub fn load(vid: u16, pid: u16) -> MouseState {
    let mut s: MouseState = std::fs::read_to_string(path(vid, pid))
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_default();
    s.cached = true;
    s
}

pub fn save(vid: u16, pid: u16, state: &MouseState) {
    let p = path(vid, pid);
    if let Some(dir) = p.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    if let Ok(text) = serde_json::to_string_pretty(state) {
        let _ = std::fs::write(p, text);
    }
}
