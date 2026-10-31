//! Utilità HID comuni ai driver.

/// Feature report dichiarati in un descrittore HID: (report ID, lunghezza in byte senza l'ID).
pub fn feature_reports(desc: &[u8]) -> Vec<(u8, usize)> {
    let (mut id, mut size, mut count) = (0u8, 0usize, 0usize);
    let mut bits: std::collections::BTreeMap<u8, usize> = Default::default();
    let mut i = 0;
    while i < desc.len() {
        let prefix = desc[i];
        let len = match prefix & 0x03 { 3 => 4, n => n as usize };
        let mut v = 0usize;
        for k in 0..len {
            v |= (*desc.get(i + 1 + k).unwrap_or(&0) as usize) << (8 * k);
        }
        match prefix & 0xFC {
            0x84 => id = v as u8,    // Report ID
            0x74 => size = v,        // Report Size
            0x94 => count = v,       // Report Count
            0xB0 => *bits.entry(id).or_default() += size * count, // Feature
            _ => {}
        }
        i += 1 + len;
    }
    bits.into_iter().map(|(id, b)| (id, b.div_ceil(8))).filter(|&(_, n)| n > 0 && n <= 4096).collect()
}


/// Lunghezza (senza l'ID) del feature report `id` dichiarata nel descrittore della collezione aperta.
pub fn feature_len(dev: &hidapi::HidDevice, id: u8) -> Option<usize> {
    let mut buf = [0u8; 4096];
    let n = dev.get_report_descriptor(&mut buf).ok()?;
    feature_reports(&buf[..n]).into_iter().find(|&(i, _)| i == id).map(|(_, len)| len)
}

#[cfg(test)]
mod tests {
    #[test]
    fn feature_reports_from_razer_descriptor() {
        // Interfaccia 3 del Basilisk V3: un feature report da 90 byte senza ID.
        let d = [0x05, 0x0c, 0x09, 0x01, 0xa1, 0x01, 0x75, 0x08, 0x95, 0x5a, 0xb1, 0x03, 0xc0];
        assert_eq!(super::feature_reports(&d), vec![(0, 90)]);
    }
}
