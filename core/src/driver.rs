use crate::db::DeviceSpec;
use crate::types::*;
use anyhow::Result;

/// Interfaccia comune a tutti i driver. Ogni scrittura deve essere seguita
/// da una rilettura di verifica dentro il driver stesso.
pub trait MouseDriver {
    fn spec(&self) -> &DeviceSpec;
    fn identify(&mut self) -> Result<DeviceInfo>;
    fn read_state(&mut self) -> Result<MouseState>;
    /// `active` è l'indice da 0 dello stadio attivo.
    fn set_dpi_stages(&mut self, stages: &[u16], active: usize) -> Result<()>;
    fn set_polling(&mut self, hz: u16) -> Result<()>;
    fn set_lighting(&mut self, zone: &str, effect: &Effect) -> Result<()>;
    /// 0-255.
    fn set_brightness(&mut self, zone: &str, value: u8) -> Result<()>;

    fn capabilities(&self) -> &Capabilities {
        &self.spec().caps
    }
}

/// Scrive sul mouse solo ciò che differisce tra `current` e `target`.
/// Restituisce l'elenco delle modifiche fatte.
pub fn apply_state(drv: &mut dyn MouseDriver, current: &MouseState, target: &MouseState) -> Result<Vec<String>> {
    let mut done = Vec::new();
    if let Some(t) = &target.dpi_stages {
        if current.dpi_stages.as_ref() != Some(t) {
            drv.set_dpi_stages(&t.stages, t.active)?;
            done.push(crate::tf!("stadi DPI {:?}, attivo {}", "DPI stages {:?}, active {}", t.stages, t.active + 1));
        }
    }
    for z in &target.zones {
        let cur = current.zones.iter().find(|c| c.zone == z.zone);
        if let Some(e) = &z.effect {
            if !matches!(e, Effect::Unknown { .. }) && cur.and_then(|c| c.effect.as_ref()) != Some(e) {
                drv.set_lighting(&z.zone, e)?;
                done.push(format!("{}: {e}", z.zone));
            }
        }
        if let Some(b) = z.brightness {
            if cur.and_then(|c| c.brightness) != Some(b) {
                drv.set_brightness(&z.zone, b)?;
                done.push(crate::tf!("{}: luminosità {}%", "{}: brightness {}%", z.zone, pct(b)));
            }
        }
    }
    // Il polling per ultimo: su alcuni mouse (Corsair) fa ricollegare il dispositivo.
    if let Some(hz) = target.polling_hz {
        if current.polling_hz != Some(hz) {
            drv.set_polling(hz)?;
            done.push(format!("polling {hz} Hz"));
        }
    }
    Ok(done)
}

pub fn pct(v: u8) -> u8 {
    ((v as u16 * 100 + 127) / 255) as u8
}

pub fn from_pct(p: u8) -> u8 {
    ((p.min(100) as u16 * 255 + 50) / 100) as u8
}
