//! Mouse "senza driver" (BenQ Zowie FK1, EC, S, ZA...): non accettano comandi via USB.
//! DPI e polling si cambiano dai tasti; l'app li riconosce, spiega come fare
//! e può misurare il polling reale (vedi `measure`).

use anyhow::{bail, Result};
use mousecfg_core::*;

pub struct Driverless {
    spec: DeviceSpec,
}

impl Driverless {
    pub fn new(spec: &DeviceSpec, vid: u16, pid: u16) -> Driverless {
        let mut spec = spec.clone();
        (spec.vid, spec.pid) = (vid, pid);
        Driverless { spec }
    }

    fn refuse(&self) -> Result<()> {
        match self.spec.notes_text() {
            Some(n) => bail!(tf!(
                "questo mouse si configura dai tasti, non via USB.\n{n}",
                "this mouse is configured with its buttons, not over USB.\n{n}"
            )),
            None => bail!(t!("questo mouse si configura dai tasti, non via USB.", "this mouse is configured with its buttons, not over USB.")),
        }
    }
}

impl MouseDriver for Driverless {
    fn spec(&self) -> &DeviceSpec {
        &self.spec
    }

    fn identify(&mut self) -> Result<DeviceInfo> {
        Ok(DeviceInfo {
            name: self.spec.name.clone(),
            vid: self.spec.vid,
            pid: self.spec.pid,
            status: self.spec.status,
            firmware: None,
            serial: None,
        })
    }

    fn read_state(&mut self) -> Result<MouseState> {
        Ok(MouseState::default())
    }

    fn set_dpi_stages(&mut self, _: &[u16], _: usize) -> Result<()> {
        self.refuse()
    }

    fn set_polling(&mut self, _: u16) -> Result<()> {
        self.refuse()
    }

    fn set_lighting(&mut self, _: &str, _: &Effect) -> Result<()> {
        self.refuse()
    }

    fn set_brightness(&mut self, _: &str, _: u8) -> Result<()> {
        self.refuse()
    }
}
