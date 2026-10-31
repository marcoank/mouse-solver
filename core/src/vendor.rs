//! Rileva il software ufficiale dei produttori, che va chiuso durante l'uso
//! perché può sovrascrivere le impostazioni appena scritte.

const KNOWN: &[(&str, &str)] = &[
    ("razerappengine.exe", "Razer Synapse"),
    ("razer synapse 3.exe", "Razer Synapse 3"),
    ("razer synapse service.exe", "Razer Synapse 3"),
    ("armourycrate.exe", "ASUS Armoury Crate"),
    ("armourycrate.service.exe", "ASUS Armoury Crate"),
    ("armourycratecontrolinterface.exe", "ASUS Armoury Crate"),
    ("steelseriesgg.exe", "SteelSeries GG"),
    ("steelseriesengine.exe", "SteelSeries GG"),
    ("zowie.exe", "BenQ Zowie"),
];

/// Nomi dei programmi dei produttori in esecuzione (senza duplicati).
pub fn running() -> Vec<&'static str> {
    let procs = process_names();
    let mut found: Vec<&'static str> = KNOWN
        .iter()
        .filter(|(exe, _)| procs.iter().any(|p| p == exe))
        .map(|(_, name)| *name)
        .collect();
    found.dedup();
    found
}

#[cfg(windows)]
fn process_names() -> Vec<String> {
    use windows_sys::Win32::Foundation::{CloseHandle, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::System::Diagnostics::ToolHelp::*;

    let mut out = Vec::new();
    unsafe {
        let snap = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0);
        if snap == INVALID_HANDLE_VALUE {
            return out;
        }
        let mut e: PROCESSENTRY32W = std::mem::zeroed();
        e.dwSize = std::mem::size_of::<PROCESSENTRY32W>() as u32;
        let mut ok = Process32FirstW(snap, &mut e);
        while ok != 0 {
            let len = e.szExeFile.iter().position(|&c| c == 0).unwrap_or(e.szExeFile.len());
            out.push(String::from_utf16_lossy(&e.szExeFile[..len]).to_lowercase());
            ok = Process32NextW(snap, &mut e);
        }
        CloseHandle(snap);
    }
    out
}

#[cfg(not(windows))]
fn process_names() -> Vec<String> {
    Vec::new()
}
