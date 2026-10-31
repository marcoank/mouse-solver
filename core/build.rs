// Incorpora nell'eseguibile tutti i file devices/*.toml.
// Aggiungere un modello = aggiungere un file TOML, senza toccare il codice.
use std::{env, fs, path::Path};

fn main() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("devices");
    println!("cargo:rerun-if-changed={}", dir.display());

    let mut files: Vec<_> = fs::read_dir(&dir)
        .expect("cartella devices/ mancante")
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "toml"))
        .collect();
    files.sort();

    let mut out = String::from("pub static BUILTIN: &[(&str, &str)] = &[\n");
    for f in &files {
        println!("cargo:rerun-if-changed={}", f.display());
        let name = f.file_name().unwrap().to_string_lossy();
        let abs = fs::canonicalize(f).unwrap();
        out += &format!("    ({name:?}, include_str!({:?})),\n", abs.display().to_string());
    }
    out += "];\n";

    let dest = Path::new(&env::var("OUT_DIR").unwrap()).join("builtin_devices.rs");
    fs::write(dest, out).unwrap();
}
