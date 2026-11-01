use anyhow::{bail, Context, Result};
use mousecfg_core::{backup, driver::{from_pct, pct}, t, tf, vendor, Db, Effect, MouseDriver, MouseState, Rgb};
use mousecfg_drivers::{hidapi::HidApi, measure, open, scan};
use std::path::PathBuf;

const HELP_IT: &str = "\
mouse-solver - DPI, polling e luci del mouse, senza software del produttore

Uso:
  mouse-solver list                              mouse collegati
  mouse-solver status [--json]                   impostazioni attuali
  mouse-solver dpi 800,1600,3200 [--active 2]    stadi DPI (stadio attivo da 1)
  mouse-solver polling 1000                      polling rate in Hz
  mouse-solver rgb <zona|all> <effetto> [colore] effetti: static breathing spectrum wave reactive off
  mouse-solver brightness <zona|all> <0-100>     luminosità in percentuale
  mouse-solver backup                            salva lo stato attuale
  mouse-solver restore [file.json]               ripristina un backup (senza file: l'ultimo)
  mouse-solver measure [secondi]                 misura il polling reale (muovi il mouse in cerchio)

Opzioni:
  --device N     scegli il mouse N della lista (default: il primo supportato)
  --no-backup    non salvare il backup automatico prima di scrivere
";

const HELP_EN: &str = "\
mouse-solver - mouse DPI, polling and lighting, without the vendor software

Usage:
  mouse-solver list                              connected mice
  mouse-solver status [--json]                   current settings
  mouse-solver dpi 800,1600,3200 [--active 2]    DPI stages (active stage counts from 1)
  mouse-solver polling 1000                      polling rate in Hz
  mouse-solver rgb <zone|all> <effect> [color]   effects: static breathing spectrum wave reactive off
  mouse-solver brightness <zone|all> <0-100>     brightness in percent
  mouse-solver backup                            save the current state
  mouse-solver restore [file.json]               restore a backup (no file: the latest one)
  mouse-solver measure [seconds]                 measure the real polling rate (move the mouse in circles)

Options:
  --device N     pick mouse N from the list (default: the first supported one)
  --no-backup    don't save the automatic backup before writing
";

struct Opts {
    args: Vec<String>,
    device: Option<usize>,
    json: bool,
    no_backup: bool,
    active: Option<usize>,
}

fn parse_opts() -> Result<Opts> {
    let mut o = Opts { args: vec![], device: None, json: false, no_backup: false, active: None };
    let mut it = std::env::args().skip(1);
    while let Some(a) = it.next() {
        match a.as_str() {
            "--device" => o.device = Some(it.next().context(t!("--device vuole un numero", "--device needs a number"))?.parse()?),
            "--active" => o.active = Some(it.next().context(t!("--active vuole un numero", "--active needs a number"))?.parse()?),
            "--json" => o.json = true,
            "--no-backup" => o.no_backup = true,
            "-h" | "--help" => o.args.push("help".into()),
            _ => o.args.push(a),
        }
    }
    Ok(o)
}

fn main() {
    if let Err(e) = run() {
        eprintln!("{}", tf!("Errore: {e:#}", "Error: {e:#}"));
        std::process::exit(1);
    }
}

fn run() -> Result<()> {
    let o = parse_opts()?;
    let cmd = o.args.first().map(String::as_str).unwrap_or("help");
    if cmd == "help" {
        print!("{}", if mousecfg_core::i18n::is_en() { HELP_EN } else { HELP_IT });
        return Ok(());
    }

    let db = Db::load()?;
    let api = HidApi::new().context(t!("impossibile inizializzare HID", "could not initialize HID"))?;
    let found = scan(&api, &db);

    if cmd == "measure" {
        let secs: f32 = o.args.get(1).map(|s| s.parse()).transpose()?.unwrap_or(4.0);
        println!("{}", tf!("Muovi il mouse in cerchio, veloce e senza fermarti, per {secs} secondi...", "Move the mouse in circles, fast and without stopping, for {secs} seconds..."));
        let res = measure::measure(std::time::Duration::from_secs_f32(secs), true)?;
        if res.is_empty() {
            println!("{}", t!("Nessun movimento rilevato (o troppo poco). Riprova muovendo il mouse di continuo.", "No movement detected (or too little). Try again, moving the mouse continuously."));
        }
        for m in res {
            let id = match (m.vid, m.pid) {
                (Some(v), Some(p)) => {
                    let name = found.iter().find(|f| f.vid == v && f.pid == p).map(|f| f.display_name()).unwrap_or_default();
                    format!("{v:04x}:{p:04x} {name}")
                }
                _ => t!("mouse sconosciuto", "unknown mouse").into(),
            };
            println!("{}", tf!("{id}: circa {:.0} Hz  ->  {} Hz  ({} pacchetti)", "{id}: about {:.0} Hz  ->  {} Hz  ({} packets)", m.measured_hz, m.nearest_hz, m.events));
        }
        return Ok(());
    }

    if cmd == "list" {
        if found.is_empty() {
            println!("{}", t!("Nessun mouse trovato.", "No mouse found."));
        }
        for (i, f) in found.iter().enumerate() {
            let badge = f.spec.as_ref().map(|s| s.status.label()).unwrap_or(t!("non supportato", "not supported"));
            println!("{}  {:04x}:{:04x}  {:<28} [{badge}]", i + 1, f.vid, f.pid, f.display_name());
        }
        return Ok(());
    }

    let pick = match o.device {
        Some(n) => found.get(n.wrapping_sub(1)).with_context(|| tf!("nessun mouse numero {n}", "no mouse number {n}"))?,
        None => found.iter().find(|f| f.spec.is_some()).context(t!("nessun mouse supportato collegato (vedi `mouse-solver list`)", "no supported mouse connected (see `mouse-solver list`)"))?,
    };
    let spec = pick.spec.as_ref().with_context(|| {
        tf!(
            "{} non è ancora supportato. Usa mouse-solver-kit.exe per raccogliere le informazioni.",
            "{} is not supported yet. Use mouse-solver-kit.exe to collect the information.",
            pick.display_name()
        )
    })?;

    let running = vendor::running();
    if !running.is_empty() && !o.json {
        eprintln!(
            "{}",
            tf!(
                "Attenzione: è aperto {}. Può sovrascrivere le impostazioni: meglio chiuderlo.\n",
                "Warning: {} is running. It may overwrite the settings: better close it.\n",
                running.join(", ")
            )
        );
    }

    let mut drv = open(&api, pick)?;
    let state = drv.read_state()?;
    let a = |i: usize| o.args.get(i).map(String::as_str);

    match cmd {
        "status" => {
            let info = drv.identify()?;
            if o.json {
                println!("{}", serde_json::to_string_pretty(&serde_json::json!({ "device": info, "state": state }))?);
            } else {
                println!("{}  [{}]", info.name, info.status.label());
                println!("  Firmware:   {}", info.firmware.as_deref().unwrap_or("?"));
                print_state(&state);
                if let Some(n) = spec.notes_text() {
                    println!("
{n}");
                }
            }
        }
        "backup" => {
            let p = backup::save(&drv.identify()?, &state)?;
            println!("{}", tf!("Backup salvato in {}", "Backup saved to {}", p.display()));
        }
        "restore" => {
            let path = match a(1) {
                Some(p) => PathBuf::from(p),
                None => backup::list(spec.vid, spec.pid).into_iter().next().context(t!("nessun backup per questo mouse", "no backup for this mouse"))?,
            };
            let b = backup::load(&path)?;
            if b.device.vid != spec.vid || b.device.pid != spec.pid {
                bail!(tf!("il backup è di un altro modello ({})", "the backup is from another model ({})", b.device.name));
            }
            write(&mut *drv, &state, &b.state, !o.no_backup)?;
        }
        "dpi" => {
            let stages: Vec<u16> = a(1)
                .context(t!("indica gli stadi, es. `mouse-solver dpi 800,1600,3200`", "give the stages, e.g. `mouse-solver dpi 800,1600,3200`"))?
                .split(',')
                .map(|s| s.trim().parse().with_context(|| tf!("'{s}' non è un numero", "'{s}' is not a number")))
                .collect::<Result<_>>()?;
            let active = match o.active {
                Some(_) if !drv.capabilities().active_stage_settable => {
                    bail!(t!("su questo mouse lo stadio attivo si cambia solo col tasto DPI", "on this mouse the active stage can only be changed with the DPI button"))
                }
                Some(0) => bail!(t!("--active parte da 1", "--active starts from 1")),
                Some(n) => n - 1,
                None => state.dpi_stages.as_ref().map(|s| s.active).unwrap_or(0).min(stages.len() - 1),
            };
            drv.capabilities().check_dpi_stages(&stages, active)?;
            let mut t = state.clone();
            t.dpi_stages = Some(mousecfg_core::DpiStages { stages, active });
            write(&mut *drv, &state, &t, !o.no_backup)?;
        }
        "polling" => {
            let hz: u16 = a(1).context(t!("indica gli Hz, es. `mouse-solver polling 1000`", "give the Hz, e.g. `mouse-solver polling 1000`"))?.parse()?;
            drv.capabilities().check_polling(hz)?;
            let t = MouseState { polling_hz: Some(hz), ..state.clone() };
            write(&mut *drv, &state, &t, !o.no_backup)?;
        }
        "rgb" => {
            let zone = a(1).context(t!("indica la zona, es. `mouse-solver rgb logo static ff0000`", "give the zone, e.g. `mouse-solver rgb logo static ff0000`"))?;
            let name = a(2).context(t!("indica l'effetto", "give the effect"))?;
            let color = match a(3) {
                Some(c) => Rgb::parse(c).with_context(|| tf!("colore '{c}' non valido, usa es. ff0000", "invalid color '{c}', use e.g. ff0000"))?,
                None if Effect::needs_color(name) => bail!(tf!("l'effetto '{name}' vuole un colore, es. ff0000", "the '{name}' effect needs a color, e.g. ff0000")),
                None => Rgb::default(),
            };
            let effect = Effect::from_name(name, color).with_context(|| tf!("effetto '{name}' sconosciuto", "unknown effect '{name}'"))?;
            let mut t = state.clone();
            for z in zones_for(&mut t, zone, drv.as_ref())? {
                drv.capabilities().check_effect(&z.zone, &effect)?;
                z.effect = Some(effect.clone());
            }
            write(&mut *drv, &state, &t, !o.no_backup)?;
        }
        "brightness" => {
            let zone = a(1).context(t!("indica la zona", "give the zone"))?;
            let p: u8 = a(2).context(t!("indica la percentuale 0-100", "give the percentage 0-100"))?.parse()?;
            if p > 100 {
                bail!(t!("la luminosità va da 0 a 100", "brightness goes from 0 to 100"));
            }
            let mut t = state.clone();
            for z in zones_for(&mut t, zone, drv.as_ref())? {
                z.brightness = Some(from_pct(p));
            }
            write(&mut *drv, &state, &t, !o.no_backup)?;
        }
        other => bail!(tf!("comando '{other}' sconosciuto. Usa `mouse-solver help`", "unknown command '{other}'. Use `mouse-solver help`")),
    }
    Ok(())
}

fn zones_for<'a>(t: &'a mut MouseState, zone: &str, drv: &dyn MouseDriver) -> Result<Vec<&'a mut mousecfg_core::ZoneState>> {
    let v: Vec<_> = t.zones.iter_mut().filter(|z| zone == "all" || z.zone == zone).collect();
    if v.is_empty() {
        bail!(tf!("zona '{zone}' non esiste (zone: {}, all)", "zone '{zone}' does not exist (zones: {}, all)", drv.capabilities().zones.join(", ")));
    }
    Ok(v)
}

fn write(drv: &mut dyn MouseDriver, current: &MouseState, target: &MouseState, do_backup: bool) -> Result<()> {
    if do_backup {
        let p = backup::save(&drv.identify()?, current)?;
        println!("{}", tf!("Backup dello stato precedente: {}", "Backup of the previous state: {}", p.display()));
    }
    let done = mousecfg_core::apply_state(drv, current, target)?;
    if done.is_empty() {
        println!("{}", t!("Niente da cambiare: il mouse è già impostato così.", "Nothing to change: the mouse is already set like this."));
    }
    let tag = if current.cached {
        t!("OK (inviato, il mouse non permette di rileggere)", "OK (sent, the mouse can't be read back)")
    } else {
        t!("OK (verificato)", "OK (verified)")
    };
    for d in done {
        println!("{tag}  {d}");
    }
    Ok(())
}

fn print_state(s: &MouseState) {
    if s.cached {
        println!("{}", t!("  (il mouse non permette di rileggere: valori dell'ultima scrittura fatta da qui)", "  (the mouse can't be read back: values from the last write made from here)"));
    }
    match &s.dpi_stages {
        Some(st) => {
            let txt: Vec<String> = st
                .stages
                .iter()
                .enumerate()
                .map(|(i, d)| if i == st.active { format!("[{d}]") } else { d.to_string() })
                .collect();
            println!("{}", tf!("  Stadi DPI:  {}   ([ ] = attivo)", "  DPI stages: {}   ([ ] = active)", txt.join("  ")));
        }
        None => println!("{}", t!("  Stadi DPI:  non leggibili", "  DPI stages: not readable")),
    }
    if let Some(d) = s.dpi {
        println!("{}", tf!("  DPI ora:    {d}", "  DPI now:    {d}"));
    }
    match s.polling_hz {
        Some(hz) => println!("  Polling:    {hz} Hz"),
        None => println!("{}", t!("  Polling:    non leggibile", "  Polling:    not readable")),
    }
    for z in &s.zones {
        let e = z.effect.as_ref().map(|e| e.to_string()).unwrap_or_else(|| "?".into());
        let b = z.brightness.map(|b| format!("{}%", pct(b))).unwrap_or_else(|| "?".into());
        println!("{}", tf!("  Luce {:<10} {e}, luminosità {b}", "  Light {:<9} {e}, brightness {b}", format!("{}:", z.zone)));
    }
}
