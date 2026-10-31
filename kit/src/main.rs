//! Kit di cattura, Modalità A: sola lettura, nessuna scrittura sul mouse.
//! Raccoglie VID/PID, interfacce HID e descrittori dei mouse collegati e, se la marca
//! è nota, le risposte ai comandi di lettura. Produce un file JSON da mandarci.

use anyhow::Result;
use mousecfg_core::{vendor, Db};
use mousecfg_drivers::{asus, hidapi::HidApi, logitech, open, razer, scan, vendor_name};
use serde_json::{json, Value};

fn main() {
    println!("mouse-cfg-kit {} - raccolta informazioni (SOLO LETTURA)\n", env!("CARGO_PKG_VERSION"));
    println!("Questo programma non modifica il mouse. Legge solo come è fatto e come è impostato.");
    println!("Il file prodotto non contiene seriali, nomi utente o altro dal tuo PC.\n");
    if let Err(e) = run() {
        eprintln!("Errore: {e:#}");
    }
    println!("\nPremi Invio per chiudere.");
    let _ = std::io::stdin().read_line(&mut String::new());
}

fn run() -> Result<()> {
    let db = Db::load()?;
    let api = HidApi::new()?;
    let found = scan(&api, &db);
    if found.is_empty() {
        println!("Nessun mouse trovato. Collegalo col cavo e riprova.");
        return Ok(());
    }
    let running = vendor::running();
    if !running.is_empty() {
        println!("Nota: è aperto {}. Va bene lo stesso, ma se qualcosa non risponde chiudilo e riprova.\n", running.join(", "));
    }

    for f in &found {
        println!("Trovato: {} ({:04x}:{:04x})", f.display_name(), f.vid, f.pid);
        let interfaces: Vec<Value> = api
            .device_list()
            .filter(|d| d.vendor_id() == f.vid && d.product_id() == f.pid)
            .map(|d| {
                let descriptor = d.open_device(&api).ok().and_then(|dev| {
                    let mut buf = [0u8; 4096];
                    dev.get_report_descriptor(&mut buf).ok().map(|n| hex(&buf[..n]))
                });
                json!({
                    "interface": d.interface_number(),
                    "usage_page": format!("0x{:04x}", d.usage_page()),
                    "usage": format!("0x{:02x}", d.usage()),
                    "release": format!("0x{:04x}", d.release_number()),
                    "report_descriptor": descriptor,
                })
            })
            .collect();

        let probe = match &f.spec {
            Some(spec) if spec.driver == "logitech" => {
                println!("  Logitech: leggo l'elenco delle funzioni HID++...");
                json!({ "driver": "logitech", "hidpp": logitech::probe(&api, f.vid, f.pid) })
            }
            Some(spec) => match open(&api, f) {
                Ok(mut drv) => json!({
                    "driver": spec.driver,
                    "firmware": drv.identify().ok().and_then(|i| i.firmware),
                    "state": drv.read_state().ok(),
                }),
                Err(e) => json!({ "driver": spec.driver, "error": e.to_string() }),
            },
            None if f.vid == 0x1532 => {
                println!("  Razer non ancora nel database: provo i comandi di lettura...");
                json!({ "driver": "razer (probe)", "result": razer::probe_unknown(&api, f.vid, f.pid) })
            }
            None if f.vid == 0x0B05 => {
                println!("  ASUS non ancora nel database: provo i comandi di lettura...");
                json!({ "driver": "asus (probe)", "result": asus::probe_unknown(&api, f.vid, f.pid) })
            }
            None => json!(null),
        };

        let report = json!({
            "kit_version": env!("CARGO_PKG_VERSION"),
            "vid": format!("0x{:04x}", f.vid),
            "pid": format!("0x{:04x}", f.pid),
            "vendor": vendor_name(f.vid),
            "manufacturer": f.manufacturer,
            "product": f.product,
            "in_database": f.spec.as_ref().map(|s| s.status.label()),
            "vendor_software_running": running,
            "interfaces": interfaces,
            "probe": probe,
        });

        let brand = vendor_name(f.vid).unwrap_or("sconosciuto").to_lowercase().replace(' ', "_");
        let name = format!("report_{brand}_{:04x}.json", f.pid);
        std::fs::write(&name, serde_json::to_string_pretty(&report)?)?;
        let path = std::env::current_dir()?.join(&name);
        println!("  Salvato: {}", path.display());
    }
    println!("\nFatto. Mandaci i file report_*.json (puoi aprirli col Blocco note per vedere cosa contengono).");
    Ok(())
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect::<Vec<_>>().join(" ")
}
