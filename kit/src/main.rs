//! Kit per raccogliere informazioni sui mouse che non supportiamo ancora.
//!
//! Modalità A (`info`, predefinita): sola lettura, nessuna scrittura sul mouse. Raccoglie VID/PID,
//! interfacce HID e descrittori e, se la marca è nota, le risposte ai comandi di lettura.
//! Modalità B (`cattura`): registra con USBPcap il traffico del software ufficiale mentre l'utente
//! cambia le impostazioni passo per passo (vedi capture.rs).

mod capture;
mod pcap;

use anyhow::Result;
use mousecfg_core::{t, tf, vendor, Db};
use mousecfg_drivers::{asus, hidapi::HidApi, logitech, open, razer, scan, vendor_name};
use serde_json::{json, Value};

fn main() {
    println!("mouse-solver-kit {}\n", env!("CARGO_PKG_VERSION"));
    let mode = match std::env::args().nth(1).as_deref() {
        Some("info") => "1".to_string(),
        Some("cattura") => "2".to_string(),
        _ => {
            println!("{}", t!("1  Raccolta informazioni (1 minuto, sola lettura, consigliata)", "1  Collect information (1 minute, read-only, recommended)"));
            println!("{}", t!("2  Cattura guidata col software ufficiale (10 minuti, serve USBPcap)\n", "2  Guided capture with the official software (10 minutes, needs USBPcap)\n"));
            print!("{}", t!("Scelta [1]: ", "Choice [1]: "));
            let _ = std::io::Write::flush(&mut std::io::stdout());
            let mut s = String::new();
            let _ = std::io::stdin().read_line(&mut s);
            s.trim().to_string()
        }
    };
    let res = if mode == "2" { capture_mode() } else { run() };
    if let Err(e) = res {
        eprintln!("{}", tf!("\nErrore: {e:#}", "\nError: {e:#}"));
    }
    println!("{}", t!("\nPremi Invio per chiudere.", "\nPress Enter to close."));
    let _ = std::io::stdin().read_line(&mut String::new());
}

fn capture_mode() -> Result<()> {
    let found = scan(&HidApi::new()?, &Db::load()?);
    if found.is_empty() {
        anyhow::bail!(t!("nessun mouse trovato", "no mouse found"));
    }
    capture::run(&found)
}

fn run() -> Result<()> {
    println!(
        "{}",
        t!(
            "Questo programma non modifica il mouse. Legge solo come è fatto e come è impostato.",
            "This program does not change the mouse. It only reads how it is built and how it is set up."
        )
    );
    println!(
        "{}",
        t!(
            "Il file prodotto non contiene seriali, nomi utente o altro dal tuo PC.\n",
            "The file it creates contains no serial numbers, user names or anything else from your PC.\n"
        )
    );
    let db = Db::load()?;
    let api = HidApi::new()?;
    let found = scan(&api, &db);
    if found.is_empty() {
        println!("{}", t!("Nessun mouse trovato. Collegalo col cavo e riprova.", "No mouse found. Connect it with the cable and try again."));
        return Ok(());
    }
    let running = vendor::running();
    if !running.is_empty() {
        println!(
            "{}",
            tf!(
                "Nota: è aperto {}. Va bene lo stesso, ma se qualcosa non risponde chiudilo e riprova.\n",
                "Note: {} is running. That's fine, but if something doesn't respond, close it and try again.\n",
                running.join(", ")
            )
        );
    }

    for f in &found {
        println!("{}", tf!("Trovato: {} ({:04x}:{:04x})", "Found: {} ({:04x}:{:04x})", f.display_name(), f.vid, f.pid));
        let interfaces: Vec<Value> = api
            .device_list()
            .filter(|d| d.vendor_id() == f.vid && d.product_id() == f.pid)
            .map(|d| {
                let dev = d.open_device(&api).ok();
                let raw = dev.as_ref().and_then(|dev| {
                    let mut buf = [0u8; 4096];
                    dev.get_report_descriptor(&mut buf).ok().map(|n| buf[..n].to_vec())
                });
                // Solo per le collezioni del produttore e solo per i mouse che non conosciamo:
                // leggo (GET_FEATURE, sola lettura) ogni feature report dichiarato nel descrittore.
                let features = match (&dev, &raw) {
                    (Some(dev), Some(raw)) if f.spec.is_none() && d.usage_page() >= 0xFF00 => mousecfg_drivers::hid::feature_reports(raw)
                        .into_iter()
                        .map(|(id, len)| {
                            let mut buf = vec![0u8; len + 1];
                            buf[0] = id;
                            let v = dev.get_feature_report(&mut buf).map(|n| hex(&buf[..n])).unwrap_or_else(|e| format!("errore: {e}"));
                            (format!("0x{id:02x} ({len} byte)"), json!(v))
                        })
                        .collect::<serde_json::Map<_, _>>(),
                    _ => Default::default(),
                };
                json!({
                    "interface": d.interface_number(),
                    "usage_page": format!("0x{:04x}", d.usage_page()),
                    "usage": format!("0x{:02x}", d.usage()),
                    "release": format!("0x{:04x}", d.release_number()),
                    "report_descriptor": raw.as_deref().map(hex),
                    "feature_reports": features,
                })
            })
            .collect();

        let probe = match &f.spec {
            Some(spec) if spec.driver == "logitech" => {
                println!("{}", t!("  Logitech: leggo l'elenco delle funzioni HID++...", "  Logitech: reading the list of HID++ features..."));
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
                println!("{}", t!("  Razer non ancora nel database: provo i comandi di lettura...", "  Razer not in the database yet: trying the read commands..."));
                json!({ "driver": "razer (probe)", "result": razer::probe_unknown(&api, f.vid, f.pid) })
            }
            None if f.vid == 0x0B05 => {
                println!("{}", t!("  ASUS non ancora nel database: provo i comandi di lettura...", "  ASUS not in the database yet: trying the read commands..."));
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
        println!("{}", tf!("  Salvato: {}", "  Saved: {}", path.display()));
    }
    println!(
        "{}",
        t!(
            "\nFatto. Mandaci i file report_*.json (puoi aprirli col Blocco note per vedere cosa contengono).",
            "\nDone. Send us the report_*.json files (you can open them with Notepad to see what they contain)."
        )
    );
    Ok(())
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect::<Vec<_>>().join(" ")
}
