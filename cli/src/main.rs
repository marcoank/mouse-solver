use anyhow::{bail, Context, Result};
use mousecfg_core::{backup, driver::{from_pct, pct}, vendor, Db, Effect, MouseDriver, MouseState, Rgb};
use mousecfg_drivers::{hidapi::HidApi, measure, open, scan};
use std::path::PathBuf;

const HELP: &str = "\
mouse-cfg - DPI, polling e luci del mouse, senza software del produttore

Uso:
  mouse-cfg list                              mouse collegati
  mouse-cfg status [--json]                   impostazioni attuali
  mouse-cfg dpi 800,1600,3200 [--active 2]    stadi DPI (stadio attivo da 1)
  mouse-cfg polling 1000                      polling rate in Hz
  mouse-cfg rgb <zona|all> <effetto> [colore] effetti: static breathing spectrum wave reactive off
  mouse-cfg brightness <zona|all> <0-100>     luminosità in percentuale
  mouse-cfg backup                            salva lo stato attuale
  mouse-cfg restore [file.json]               ripristina un backup (senza file: l'ultimo)
  mouse-cfg measure [secondi]                 misura il polling reale (muovi il mouse in cerchio)

Opzioni:
  --device N     scegli il mouse N della lista (default: il primo supportato)
  --no-backup    non salvare il backup automatico prima di scrivere
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
            "--device" => o.device = Some(it.next().context("--device vuole un numero")?.parse()?),
            "--active" => o.active = Some(it.next().context("--active vuole un numero")?.parse()?),
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
        eprintln!("Errore: {e:#}");
        std::process::exit(1);
    }
}

fn run() -> Result<()> {
    let o = parse_opts()?;
    let cmd = o.args.first().map(String::as_str).unwrap_or("help");
    if cmd == "help" {
        print!("{HELP}");
        return Ok(());
    }

    let db = Db::load()?;
    let api = HidApi::new().context("impossibile inizializzare HID")?;
    let found = scan(&api, &db);

    if cmd == "measure" {
        let secs: f32 = o.args.get(1).map(|s| s.parse()).transpose()?.unwrap_or(4.0);
        println!("Muovi il mouse in cerchio, veloce e senza fermarti, per {secs} secondi...");
        let res = measure::measure(std::time::Duration::from_secs_f32(secs), true)?;
        if res.is_empty() {
            println!("Nessun movimento rilevato (o troppo poco). Riprova muovendo il mouse di continuo.");
        }
        for m in res {
            let id = match (m.vid, m.pid) {
                (Some(v), Some(p)) => {
                    let name = found.iter().find(|f| f.vid == v && f.pid == p).map(|f| f.display_name()).unwrap_or_default();
                    format!("{v:04x}:{p:04x} {name}")
                }
                _ => "mouse sconosciuto".into(),
            };
            println!("{id}: circa {:.0} Hz  ->  {} Hz  ({} pacchetti)", m.measured_hz, m.nearest_hz, m.events);
        }
        return Ok(());
    }

    if cmd == "list" {
        if found.is_empty() {
            println!("Nessun mouse trovato.");
        }
        for (i, f) in found.iter().enumerate() {
            let badge = f.spec.as_ref().map(|s| s.status.label()).unwrap_or("non supportato");
            println!("{}  {:04x}:{:04x}  {:<28} [{badge}]", i + 1, f.vid, f.pid, f.display_name());
        }
        return Ok(());
    }

    let pick = match o.device {
        Some(n) => found.get(n.wrapping_sub(1)).with_context(|| format!("nessun mouse numero {n}"))?,
        None => found.iter().find(|f| f.spec.is_some()).context("nessun mouse supportato collegato (vedi `mouse-cfg list`)")?,
    };
    let spec = pick.spec.as_ref().with_context(|| {
        format!("{} non è ancora supportato. Usa mouse-cfg-kit.exe per raccogliere le informazioni.", pick.display_name())
    })?;

    let running = vendor::running();
    if !running.is_empty() && !o.json {
        eprintln!("Attenzione: è aperto {}. Può sovrascrivere le impostazioni: meglio chiuderlo.\n", running.join(", "));
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
                if let Some(n) = &spec.notes {
                    println!("
{n}");
                }
            }
        }
        "backup" => {
            let p = backup::save(&drv.identify()?, &state)?;
            println!("Backup salvato in {}", p.display());
        }
        "restore" => {
            let path = match a(1) {
                Some(p) => PathBuf::from(p),
                None => backup::list(spec.vid, spec.pid).into_iter().next().context("nessun backup per questo mouse")?,
            };
            let b = backup::load(&path)?;
            if b.device.vid != spec.vid || b.device.pid != spec.pid {
                bail!("il backup è di un altro modello ({})", b.device.name);
            }
            write(&mut *drv, &state, &b.state, !o.no_backup)?;
        }
        "dpi" => {
            let stages: Vec<u16> = a(1)
                .context("indica gli stadi, es. `mouse-cfg dpi 800,1600,3200`")?
                .split(',')
                .map(|s| s.trim().parse().with_context(|| format!("'{s}' non è un numero")))
                .collect::<Result<_>>()?;
            let active = match o.active {
                Some(_) if !drv.capabilities().active_stage_settable => {
                    bail!("su questo mouse lo stadio attivo si cambia solo col tasto DPI")
                }
                Some(0) => bail!("--active parte da 1"),
                Some(n) => n - 1,
                None => state.dpi_stages.as_ref().map(|s| s.active).unwrap_or(0).min(stages.len() - 1),
            };
            drv.capabilities().check_dpi_stages(&stages, active)?;
            let mut t = state.clone();
            t.dpi_stages = Some(mousecfg_core::DpiStages { stages, active });
            write(&mut *drv, &state, &t, !o.no_backup)?;
        }
        "polling" => {
            let hz: u16 = a(1).context("indica gli Hz, es. `mouse-cfg polling 1000`")?.parse()?;
            drv.capabilities().check_polling(hz)?;
            let t = MouseState { polling_hz: Some(hz), ..state.clone() };
            write(&mut *drv, &state, &t, !o.no_backup)?;
        }
        "rgb" => {
            let zone = a(1).context("indica la zona, es. `mouse-cfg rgb logo static ff0000`")?;
            let name = a(2).context("indica l'effetto")?;
            let color = match a(3) {
                Some(c) => Rgb::parse(c).with_context(|| format!("colore '{c}' non valido, usa es. ff0000"))?,
                None if Effect::needs_color(name) => bail!("l'effetto '{name}' vuole un colore, es. ff0000"),
                None => Rgb::default(),
            };
            let effect = Effect::from_name(name, color).with_context(|| format!("effetto '{name}' sconosciuto"))?;
            let mut t = state.clone();
            for z in zones_for(&mut t, zone, drv.as_ref())? {
                drv.capabilities().check_effect(&z.zone, &effect)?;
                z.effect = Some(effect.clone());
            }
            write(&mut *drv, &state, &t, !o.no_backup)?;
        }
        "brightness" => {
            let zone = a(1).context("indica la zona")?;
            let p: u8 = a(2).context("indica la percentuale 0-100")?.parse()?;
            if p > 100 {
                bail!("la luminosità va da 0 a 100");
            }
            let mut t = state.clone();
            for z in zones_for(&mut t, zone, drv.as_ref())? {
                z.brightness = Some(from_pct(p));
            }
            write(&mut *drv, &state, &t, !o.no_backup)?;
        }
        other => bail!("comando '{other}' sconosciuto. Usa `mouse-cfg help`"),
    }
    Ok(())
}

fn zones_for<'a>(t: &'a mut MouseState, zone: &str, drv: &dyn MouseDriver) -> Result<Vec<&'a mut mousecfg_core::ZoneState>> {
    let v: Vec<_> = t.zones.iter_mut().filter(|z| zone == "all" || z.zone == zone).collect();
    if v.is_empty() {
        bail!("zona '{zone}' non esiste (zone: {}, all)", drv.capabilities().zones.join(", "));
    }
    Ok(v)
}

fn write(drv: &mut dyn MouseDriver, current: &MouseState, target: &MouseState, do_backup: bool) -> Result<()> {
    if do_backup {
        let p = backup::save(&drv.identify()?, current)?;
        println!("Backup dello stato precedente: {}", p.display());
    }
    let done = mousecfg_core::apply_state(drv, current, target)?;
    if done.is_empty() {
        println!("Niente da cambiare: il mouse è già impostato così.");
    }
    let tag = if current.cached { "OK (inviato, il mouse non permette di rileggere)" } else { "OK (verificato)" };
    for d in done {
        println!("{tag}  {d}");
    }
    Ok(())
}

fn print_state(s: &MouseState) {
    if s.cached {
        println!("  (il mouse non permette di rileggere: valori dell'ultima scrittura fatta da qui)");
    }
    match &s.dpi_stages {
        Some(st) => {
            let txt: Vec<String> = st
                .stages
                .iter()
                .enumerate()
                .map(|(i, d)| if i == st.active { format!("[{d}]") } else { d.to_string() })
                .collect();
            println!("  Stadi DPI:  {}   ([ ] = attivo)", txt.join("  "));
        }
        None => println!("  Stadi DPI:  non leggibili"),
    }
    if let Some(d) = s.dpi {
        println!("  DPI ora:    {d}");
    }
    match s.polling_hz {
        Some(hz) => println!("  Polling:    {hz} Hz"),
        None => println!("  Polling:    non leggibile"),
    }
    for z in &s.zones {
        let e = z.effect.as_ref().map(|e| e.to_string()).unwrap_or_else(|| "?".into());
        let b = z.brightness.map(|b| format!("{}%", pct(b))).unwrap_or_else(|| "?".into());
        println!("  Luce {:<10} {e}, luminosità {b}", format!("{}:", z.zone));
    }
}
