//! Kit, Modalità B: cattura guidata del traffico tra il software ufficiale e il mouse.
//!
//! Usa USBPcapCMD (installato con USBPcap o con Wireshark). Cattura SOLO l'indirizzo USB del mouse:
//! prima una lettura di un secondo dei soli descrittori per trovare l'indirizzo (scartata subito),
//! poi la cattura vera con il filtro `--devices`. Alla fine controlla che nel file ci sia solo il mouse.
//! Durante la cattura l'utente cambia le impostazioni nel software ufficiale e preme Invio a ogni passo:
//! ogni passo diventa un "marcatore" con orario di inizio e fine, usato da tools/analyze_capture.py.

use crate::pcap;
use anyhow::{bail, Context, Result};
use mousecfg_core::{t, tf};
use mousecfg_drivers::Found;
use serde_json::json;
use std::io::{BufRead, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// Passi proposti: (categoria, valore, (istruzione in italiano, in inglese)).
const STEPS: &[(&str, &str, (&str, &str))] = &[
    ("dpi", "400", ("Imposta lo stadio DPI 1 a 400 e applica/salva", "Set DPI stage 1 to 400 and apply/save")),
    ("dpi", "800", ("Ora lo stadio DPI 1 a 800", "Now DPI stage 1 to 800")),
    ("dpi", "1600", ("Ora lo stadio DPI 1 a 1600", "Now DPI stage 1 to 1600")),
    ("dpi", "3200", ("Ora lo stadio DPI 1 a 3200", "Now DPI stage 1 to 3200")),
    ("dpi_active", "2", ("Rendi attivo lo stadio DPI 2", "Make DPI stage 2 active")),
    ("dpi_active", "1", ("Rendi attivo lo stadio DPI 1", "Make DPI stage 1 active")),
    ("polling", "125", ("Imposta il polling a 125 Hz", "Set polling to 125 Hz")),
    ("polling", "500", ("Imposta il polling a 500 Hz", "Set polling to 500 Hz")),
    ("polling", "1000", ("Imposta il polling a 1000 Hz", "Set polling to 1000 Hz")),
    ("color", "ff0000", ("Luce (logo o principale) fissa ROSSA", "Light (logo or main) static RED")),
    ("color", "00ff00", ("Ora fissa VERDE", "Now static GREEN")),
    ("color", "0000ff", ("Ora fissa BLU", "Now static BLUE")),
    ("effect", "breathing", ("Effetto respiro (breathing)", "Breathing effect")),
    ("effect", "spectrum", ("Effetto ciclo colori (spectrum/rainbow)", "Color cycle effect (spectrum/rainbow)")),
    ("effect", "off", ("Luci spente", "Lights off")),
    ("brightness", "100", ("Luci accese, luminosità 100%", "Lights on, brightness 100%")),
    ("brightness", "50", ("Luminosità 50%", "Brightness 50%")),
];

pub fn usbpcap_cmd() -> Option<PathBuf> {
    let pf = std::env::var_os("ProgramFiles").map(PathBuf::from).unwrap_or_else(|| PathBuf::from(r"C:\Program Files"));
    [pf.join(r"USBPcap\USBPcapCMD.exe"), pf.join(r"Wireshark\extcap\USBPcapCMD.exe"), pf.join(r"Wireshark\extcap\wireshark\USBPcapCMD.exe")]
        .into_iter()
        .find(|p| p.exists())
}

fn now() -> f64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs_f64()).unwrap_or(0.0)
}

fn ask(prompt: &str) -> String {
    print!("{prompt}");
    let _ = std::io::stdout().flush();
    let mut s = String::new();
    let _ = std::io::stdin().lock().read_line(&mut s);
    s.trim().to_string()
}

/// Avvia USBPcapCMD scrivendo la cattura su stdout; un thread raccoglie i byte.
fn start(cmd: &Path, args: &[String]) -> Result<(Child, Arc<Mutex<Vec<u8>>>, std::thread::JoinHandle<()>)> {
    let mut child = Command::new(cmd)
        .args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .stdin(Stdio::null())
        .spawn()
        .with_context(|| tf!("impossibile avviare {}", "could not start {}", cmd.display()))?;
    let mut out = child.stdout.take().unwrap();
    let buf = Arc::new(Mutex::new(Vec::new()));
    let b = buf.clone();
    let t = std::thread::spawn(move || {
        let mut chunk = [0u8; 65536];
        while let Ok(n) = out.read(&mut chunk) {
            if n == 0 {
                break;
            }
            b.lock().unwrap().extend_from_slice(&chunk[..n]);
        }
    });
    Ok((child, buf, t))
}

/// Cerca su quale hub USBPcap e con quale indirizzo si trova il mouse.
/// La cattura di prova dura un secondo e viene scartata subito (contiene tutto l'hub).
fn locate(cmd: &Path, vid: u16, pid: u16) -> Result<(String, u16)> {
    let mut any_hub = false;
    for n in 1..=10 {
        let hub = format!(r"\\.\USBPcap{n}");
        let args = vec!["-d".into(), hub.clone(), "-o".into(), "-".into(), "-A".into(), "--inject-descriptors".into()];
        let Ok((mut child, buf, t)) = start(cmd, &args) else { continue };
        std::thread::sleep(Duration::from_millis(1200));
        let _ = child.kill();
        let _ = t.join();
        let data = std::mem::take(&mut *buf.lock().unwrap());
        if data.len() >= 24 {
            any_hub = true;
        }
        let found = pcap::device_ids(&pcap::parse(&data)).into_iter().find(|&(_, v, p)| v == vid && p == pid);
        drop(data); // non teniamo niente della cattura di prova
        if let Some((addr, _, _)) = found {
            return Ok((hub, addr));
        }
    }
    if !any_hub {
        bail!(t!(
            "USBPcap non risponde. Avvia il kit come amministratore (tasto destro > Esegui come amministratore) e controlla che USBPcap sia installato e il PC riavviato dopo l'installazione.",
            "USBPcap is not responding. Run the kit as administrator (right click > Run as administrator) and check that USBPcap is installed and the PC was restarted after installing it."
        ));
    }
    bail!(t!(
        "non ho trovato il mouse su nessun hub USBPcap. Collegalo col cavo (non tramite hub esterni) e riprova.",
        "could not find the mouse on any USBPcap hub. Connect it with the cable (not through an external hub) and try again."
    ))
}

pub fn run(found: &[Found]) -> Result<()> {
    let cmd = usbpcap_cmd().context(t!(
        "USBPcap non è installato. Scaricalo da https://desowin.org/usbpcap/ (oppure installa Wireshark e spunta USBPcap), riavvia il PC e rilancia il kit come amministratore.",
        "USBPcap is not installed. Download it from https://desowin.org/usbpcap/ (or install Wireshark and tick USBPcap), restart the PC and run the kit again as administrator.",
    ))?;
    println!("{}", t!("CATTURA GUIDATA\n", "GUIDED CAPTURE\n"));
    println!(
        "{}",
        t!(
            "Cosa fa: registra SOLO i dati scambiati tra il software del produttore e il mouse scelto.",
            "What it does: it records ONLY the data exchanged between the vendor software and the chosen mouse."
        )
    );
    println!(
        "{}",
        t!(
            "Tastiera e altri dispositivi NON vengono registrati: il kit lo controlla alla fine.",
            "Keyboard and other devices are NOT recorded: the kit checks this at the end."
        )
    );
    println!("{}", t!("Per sicurezza: durante la cattura non digitare password.\n", "To be safe: don't type any password during the capture.\n"));

    for (i, f) in found.iter().enumerate() {
        println!("  {}  {:04x}:{:04x}  {}", i + 1, f.vid, f.pid, f.display_name());
    }
    let n: usize = ask(t!("\nNumero del mouse da catturare: ", "\nNumber of the mouse to capture: "))
        .parse()
        .context(t!("numero non valido", "invalid number"))?;
    let f = found.get(n.wrapping_sub(1)).context(t!("numero non valido", "invalid number"))?;

    println!("{}", t!("\nCerco il mouse sugli hub USB (un secondo, non toccare la tastiera)...", "\nLooking for the mouse on the USB hubs (one second, don't touch the keyboard)..."));
    let (hub, addr) = locate(&cmd, f.vid, f.pid)?;
    println!("{}", tf!("Trovato su {hub}, indirizzo {addr}.\n", "Found on {hub}, address {addr}.\n"));

    ask(t!(
        "Apri il software ufficiale del mouse, poi premi Invio per iniziare la cattura...",
        "Open the official mouse software, then press Enter to start the capture..."
    ));
    let args = vec![
        "-d".into(), hub.clone(), "-o".into(), "-".into(),
        "--devices".into(), addr.to_string(), "--inject-descriptors".into(), "-s".into(), "65535".into(),
    ];
    let (mut child, buf, t) = start(&cmd, &args)?;
    std::thread::sleep(Duration::from_millis(500));

    println!("{}", t!("\nPer ogni passo: fai la modifica nel software ufficiale, poi premi Invio.", "\nFor each step: make the change in the official software, then press Enter."));
    println!(
        "{}",
        t!(
            "Se il mouse non ha quella funzione scrivi s e premi Invio per saltare. q per finire prima.\n",
            "If the mouse doesn't have that feature, type s and press Enter to skip. q to finish early.\n"
        )
    );
    let mut markers = Vec::new();
    let mut t0 = now();
    for (i, (kind, value, (text_it, text_en))) in STEPS.iter().enumerate() {
        let text = if mousecfg_core::i18n::is_en() { text_en } else { text_it };
        let a = ask(&format!("[{}/{}] {text}: ", i + 1, STEPS.len())).to_lowercase();
        let t1 = now();
        match a.as_str() {
            "q" => break,
            "s" => {}
            _ => markers.push(json!({ "kind": kind, "value": value, "label": text, "start": t0, "end": t1 })),
        }
        t0 = t1;
    }
    std::thread::sleep(Duration::from_millis(500));
    let _ = child.kill();
    let _ = t.join();
    let data = std::mem::take(&mut *buf.lock().unwrap());

    // Verifica privacy: solo l'indirizzo del mouse (0 = hub radice, ammesso).
    let packets = pcap::parse(&data);
    let others: Vec<u16> = packets.iter().map(|p| p.device).filter(|&d| d != addr && d != 0).collect();
    if !others.is_empty() {
        bail!(tf!(
            "la cattura contiene altri dispositivi (indirizzi {others:?}): il filtro non ha funzionato, file NON salvato.",
            "the capture contains other devices (addresses {others:?}): the filter didn't work, file NOT saved."
        ));
    }
    if packets.is_empty() {
        bail!(t!("cattura vuota: USBPcap non ha registrato nulla.", "empty capture: USBPcap recorded nothing."));
    }

    let dir = std::env::current_dir()?.join(format!("cattura_{:04x}_{:04x}_{}", f.vid, f.pid, now() as u64));
    std::fs::create_dir_all(&dir)?;
    let meta = json!({
        "vid": format!("0x{:04x}", f.vid), "pid": format!("0x{:04x}", f.pid), "name": f.display_name(),
        "usb_address": addr, "kit_version": env!("CARGO_PKG_VERSION"), "markers": markers,
    });
    std::fs::write(dir.join("capture.pcap"), &data)?;
    std::fs::write(dir.join("markers.json"), serde_json::to_string_pretty(&meta)?)?;

    println!("{}", tf!("\nFatto: {} pacchetti, {} passi registrati.", "\nDone: {} packets, {} steps recorded.", packets.len(), markers.len()));
    println!("{}", tf!("Cartella: {}", "Folder: {}", dir.display()));
    println!(
        "{}",
        tf!(
            "  capture.pcap   {} KB  (solo il traffico del mouse {:04x}:{:04x})",
            "  capture.pcap   {} KB  (only the traffic of mouse {:04x}:{:04x})",
            data.len() / 1024,
            f.vid,
            f.pid
        )
    );
    println!("{}", t!("  markers.json   orari dei passi", "  markers.json   step timestamps"));
    println!("{}", t!("Comprimi la cartella e mandacela.", "Zip the folder and send it to us."));
    Ok(())
}
