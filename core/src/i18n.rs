//! Italiano / inglese. La lingua viene da Windows (italiano se l'interfaccia è in italiano,
//! altrimenti inglese) e si può forzare con MOUSE_SOLVER_LANG=it oppure =en.
//!
//! Uso: `t!("testo", "text")` per testi fissi, `tf!("ciao {nome}", "hi {nome}")` come `format!`.

use std::sync::OnceLock;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Lang {
    It,
    En,
}

static LANG: OnceLock<Lang> = OnceLock::new();

pub fn lang() -> Lang {
    *LANG.get_or_init(|| match std::env::var("MOUSE_SOLVER_LANG").ok().as_deref() {
        Some("it") => Lang::It,
        Some("en") => Lang::En,
        _ => system_lang(),
    })
}

pub fn is_en() -> bool {
    lang() == Lang::En
}

#[cfg(windows)]
fn system_lang() -> Lang {
    // LANGID: i 10 bit bassi sono la lingua principale; 0x10 = italiano.
    let id = unsafe { windows_sys::Win32::Globalization::GetUserDefaultUILanguage() };
    if id & 0x3FF == 0x10 {
        Lang::It
    } else {
        Lang::En
    }
}

#[cfg(not(windows))]
fn system_lang() -> Lang {
    match std::env::var("LANG") {
        Ok(l) if l.starts_with("it") => Lang::It,
        _ => Lang::En,
    }
}

/// Testo fisso nelle due lingue.
#[macro_export]
macro_rules! t {
    ($it:literal, $en:literal $(,)?) => {
        if $crate::i18n::is_en() { $en } else { $it }
    };
}

/// Come `format!`, nelle due lingue.
#[macro_export]
macro_rules! tf {
    ($it:literal, $en:literal $(, $args:expr)* $(,)?) => {
        if $crate::i18n::is_en() { format!($en $(, $args)*) } else { format!($it $(, $args)*) }
    };
}
