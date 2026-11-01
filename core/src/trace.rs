//! Registro dei pacchetti scambiati con il mouse durante la sessione.
//!
//! Serve al report di feedback: chi prova l'app può allegarlo a una segnalazione senza dover
//! riavviare niente. Con la variabile d'ambiente MOUSE_SOLVER_DEBUG (o la vecchia MOUSECFG_DEBUG)
//! le righe vengono anche stampate su stderr.

use std::collections::VecDeque;
use std::sync::Mutex;

const MAX_LINES: usize = 5000;

static LINES: Mutex<VecDeque<String>> = Mutex::new(VecDeque::new());

/// Registrazione sempre attiva (il costo è trascurabile).
pub fn on() -> bool {
    true
}

fn echo() -> bool {
    std::env::var_os("MOUSE_SOLVER_DEBUG").is_some() || std::env::var_os("MOUSECFG_DEBUG").is_some()
}

pub fn log(line: String) {
    if echo() {
        eprintln!("{line}");
    }
    if let Ok(mut l) = LINES.lock() {
        if l.len() >= MAX_LINES {
            l.pop_front();
        }
        l.push_back(line);
    }
}

/// Copia del registro, dalla riga più vecchia.
pub fn snapshot() -> Vec<String> {
    LINES.lock().map(|l| l.iter().cloned().collect()).unwrap_or_default()
}

/// Come `format!`, ma la riga finisce nel registro.
#[macro_export]
macro_rules! trace {
    ($($t:tt)*) => {
        $crate::trace::log(format!($($t)*))
    };
}
