//! Report di feedback: un file JSON in sola lettura da allegare a una segnalazione su GitHub.
//!
//! Contiene: versione dell'app, mouse (VID/PID, nome, driver, stato nel database, firmware), le
//! interfacce HID con i descrittori, lo stato letto, l'esito di ogni "Applica" della sessione, le
//! risposte della persona e il registro dei pacchetti. Niente numeri di serie, nomi utente o
//! percorsi del PC. L'app non lo spedisce: lo salva e basta.

use mousecfg_core::{DeviceInfo, MouseState};
use mousecfg_drivers::{hidapi::HidApi, Found};
use serde_json::{json, Value};
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

/// Esito di un "Applica".
#[derive(Clone)]
pub struct Attempt {
    pub changes: String,
    pub ok: bool,
    pub message: String,
}

/// Risposte della persona che prova.
#[derive(Default, Clone)]
pub struct Answers {
    pub dpi: Option<bool>,
    pub active_stage: Option<bool>,
    pub polling: Option<bool>,
    pub lighting: Option<bool>,
    pub persists: Option<bool>,
    pub notes: String,
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect::<Vec<_>>().join(" ")
}

pub fn build(
    api: &HidApi,
    found: &Found,
    info: Option<&DeviceInfo>,
    state: Option<&MouseState>,
    attempts: &[Attempt],
    answers: &Answers,
) -> Value {
    let interfaces: Vec<Value> = api
        .device_list()
        .filter(|d| d.vendor_id() == found.vid && d.product_id() == found.pid)
        .map(|d| {
            let descriptor = d.open_device(api).ok().and_then(|dev| {
                let mut buf = [0u8; 4096];
                dev.get_report_descriptor(&mut buf).ok().map(|n| hex(&buf[..n]))
            });
            json!({
                "interface": d.interface_number(),
                "usage_page": format!("0x{:04x}", d.usage_page()),
                "usage": format!("0x{:02x}", d.usage()),
                "report_descriptor": descriptor,
            })
        })
        .collect();
    json!({
        "app": "mouse solver",
        "version": env!("CARGO_PKG_VERSION"),
        "language": if mousecfg_core::i18n::is_en() { "en" } else { "it" },
        "os": format!("{} {}", std::env::consts::OS, std::env::consts::ARCH),
        "mouse": {
            "vid": format!("0x{:04x}", found.vid),
            "pid": format!("0x{:04x}", found.pid),
            "product": found.product,
            "manufacturer": found.manufacturer,
            "model": found.spec.as_ref().map(|s| s.name.clone()),
            "driver": found.spec.as_ref().map(|s| s.driver.clone()),
            "status": found.spec.as_ref().map(|s| s.status.label()),
            "firmware": info.and_then(|i| i.firmware.clone()),
        },
        "interfaces": interfaces,
        "state": state,
        "attempts": attempts.iter().map(|a| json!({ "changes": a.changes, "ok": a.ok, "message": a.message })).collect::<Vec<_>>(),
        "answers": {
            "dpi_works": answers.dpi,
            "active_stage_works": answers.active_stage,
            "polling_works": answers.polling,
            "lighting_works": answers.lighting,
            "settings_persist_after_replug": answers.persists,
            "notes": answers.notes,
        },
        "packets": mousecfg_core::trace::snapshot(),
    })
}

/// Cartella dove salvare: il Desktop (anche quello di OneDrive), altrimenti la cartella corrente.
fn target_dir() -> PathBuf {
    if let Some(home) = std::env::var_os("USERPROFILE").map(PathBuf::from) {
        for d in [home.join("Desktop"), home.join("OneDrive").join("Desktop")] {
            if d.is_dir() {
                return d;
            }
        }
    }
    std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
}

pub fn save(report: &Value, vid: u16, pid: u16) -> anyhow::Result<PathBuf> {
    let now = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    let path = target_dir().join(format!("mouse-solver-report-{vid:04x}-{pid:04x}-{now}.json"));
    std::fs::write(&path, serde_json::to_string_pretty(report)?)?;
    Ok(path)
}
