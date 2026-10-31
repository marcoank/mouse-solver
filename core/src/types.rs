use serde::{Deserialize, Serialize};
use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SupportStatus {
    Verified,
    Community,
    Experimental,
}

impl SupportStatus {
    pub fn label(self) -> &'static str {
        match self {
            Self::Verified => "verified",
            Self::Community => "community",
            Self::Experimental => "experimental",
        }
    }
}

/// Colore RGB. In JSON/TOML viene scritto come "ff0000".
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(into = "String", try_from = "String")]
pub struct Rgb(pub u8, pub u8, pub u8);

impl Rgb {
    pub fn parse(s: &str) -> Option<Rgb> {
        let s = s.trim().trim_start_matches('#');
        if s.len() != 6 {
            return None;
        }
        let v = u32::from_str_radix(s, 16).ok()?;
        Some(Rgb((v >> 16) as u8, (v >> 8) as u8, v as u8))
    }
}

impl fmt::Display for Rgb {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "{:02x}{:02x}{:02x}", self.0, self.1, self.2)
    }
}

impl From<Rgb> for String {
    fn from(c: Rgb) -> String {
        c.to_string()
    }
}

impl TryFrom<String> for Rgb {
    type Error = String;
    fn try_from(s: String) -> Result<Rgb, String> {
        Rgb::parse(&s).ok_or_else(|| format!("colore non valido: {s}"))
    }
}

/// Effetti luminosi salvati nel mouse (mai animazioni gestite dal PC).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum Effect {
    Off,
    Static { color: Rgb },
    Breathing { color: Rgb },
    Spectrum,
    Wave { reverse: bool },
    Reactive { color: Rgb },
    /// Effetto letto dal mouse che non sappiamo decodificare: non viene mai riscritto.
    Unknown { code: u8 },
}

impl Effect {
    pub const NAMES: [&'static str; 6] = ["static", "breathing", "spectrum", "wave", "reactive", "off"];

    pub fn name(&self) -> &'static str {
        match self {
            Effect::Off => "off",
            Effect::Static { .. } => "static",
            Effect::Breathing { .. } => "breathing",
            Effect::Spectrum => "spectrum",
            Effect::Wave { .. } => "wave",
            Effect::Reactive { .. } => "reactive",
            Effect::Unknown { .. } => "unknown",
        }
    }

    pub fn color(&self) -> Option<Rgb> {
        match self {
            Effect::Static { color } | Effect::Breathing { color } | Effect::Reactive { color } => Some(*color),
            _ => None,
        }
    }

    /// Costruisce un effetto dal nome; `color` serve solo agli effetti che lo usano.
    pub fn from_name(name: &str, color: Rgb) -> Option<Effect> {
        Some(match name {
            "off" => Effect::Off,
            "static" => Effect::Static { color },
            "breathing" => Effect::Breathing { color },
            "spectrum" => Effect::Spectrum,
            "wave" => Effect::Wave { reverse: false },
            "reactive" => Effect::Reactive { color },
            _ => return None,
        })
    }

    pub fn needs_color(name: &str) -> bool {
        matches!(name, "static" | "breathing" | "reactive")
    }
}

impl fmt::Display for Effect {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            Effect::Unknown { code } => write!(f, "sconosciuto (0x{code:02x})"),
            e => match e.color() {
                Some(c) => write!(f, "{} #{c}", e.name()),
                None => write!(f, "{}", e.name()),
            },
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeviceInfo {
    pub name: String,
    pub vid: u16,
    pub pid: u16,
    pub status: SupportStatus,
    pub firmware: Option<String>,
    pub serial: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DpiStages {
    pub stages: Vec<u16>,
    /// Indice (da 0) dello stadio attivo.
    pub active: usize,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ZoneState {
    pub zone: String,
    pub effect: Option<Effect>,
    /// 0-255 come nel mouse.
    pub brightness: Option<u8>,
}

/// Stato letto dal mouse. `None` = il mouse non permette di rileggerlo.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct MouseState {
    pub dpi: Option<u16>,
    pub dpi_stages: Option<DpiStages>,
    pub polling_hz: Option<u16>,
    pub zones: Vec<ZoneState>,
    /// true = il mouse non permette di rileggere: sono gli ultimi valori scritti da noi.
    #[serde(default)]
    pub cached: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Capabilities {
    pub dpi_min: u16,
    pub dpi_max: u16,
    pub dpi_step: u16,
    /// Se non vuoto: gli unici DPI accettati dal mouse (scala non regolare).
    #[serde(default)]
    pub dpi_values: Vec<u16>,
    #[serde(default = "one")]
    pub min_stages: u8,
    pub max_stages: u8,
    /// false = lo stadio attivo si cambia solo col tasto sul mouse.
    #[serde(default = "yes")]
    pub active_stage_settable: bool,
    pub polling: Vec<u16>,
    #[serde(default)]
    pub zones: Vec<String>,
    #[serde(default)]
    pub effects: Vec<String>,
}

fn one() -> u8 {
    1
}

fn yes() -> bool {
    true
}

impl Capabilities {
    pub fn check_dpi_stages(&self, stages: &[u16], active: usize) -> anyhow::Result<()> {
        if stages.len() < self.min_stages as usize || stages.len() > self.max_stages as usize {
            match self.min_stages == self.max_stages {
                true => anyhow::bail!("questo mouse ha esattamente {} stadi DPI", self.max_stages),
                false => anyhow::bail!("servono da {} a {} stadi DPI", self.min_stages, self.max_stages),
            }
        }
        if active >= stages.len() {
            anyhow::bail!("stadio attivo {} non esiste (ci sono {} stadi)", active + 1, stages.len());
        }
        for &d in stages {
            if d < self.dpi_min || d > self.dpi_max {
                anyhow::bail!("DPI {d} fuori dai limiti {}-{}", self.dpi_min, self.dpi_max);
            }
            if !self.dpi_values.is_empty() {
                if !self.dpi_values.contains(&d) {
                    anyhow::bail!("DPI {d} non disponibile (valori possibili: {:?})", self.dpi_values);
                }
            } else if self.dpi_step > 1 && (d - self.dpi_min) % self.dpi_step != 0 {
                anyhow::bail!("DPI {d} non è un multiplo di {}", self.dpi_step);
            }
        }
        Ok(())
    }

    /// Il DPI valido più vicino a `d`.
    pub fn snap_dpi(&self, d: u16) -> u16 {
        if let Some(&v) = self.dpi_values.iter().min_by_key(|&&v| v.abs_diff(d)) {
            return v;
        }
        let step = self.dpi_step.max(1);
        let d = d.clamp(self.dpi_min, self.dpi_max.max(self.dpi_min));
        (self.dpi_min + (d - self.dpi_min + step / 2) / step * step).min(self.dpi_max)
    }

    pub fn check_polling(&self, hz: u16) -> anyhow::Result<()> {
        if !self.polling.contains(&hz) {
            anyhow::bail!("polling {hz} Hz non supportato (valori possibili: {:?})", self.polling);
        }
        Ok(())
    }

    pub fn check_effect(&self, zone: &str, effect: &Effect) -> anyhow::Result<()> {
        if !self.zones.iter().any(|z| z == zone) {
            anyhow::bail!("zona '{zone}' non esiste (zone: {})", self.zones.join(", "));
        }
        if !self.effects.iter().any(|e| e == effect.name()) {
            anyhow::bail!("effetto '{}' non supportato (effetti: {})", effect.name(), self.effects.join(", "));
        }
        Ok(())
    }
}
