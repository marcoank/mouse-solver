//! Lettura minima dei file pcap prodotti da USBPcap (linktype 249, LINKTYPE_USBPCAP).
//! Serve a trovare l'indirizzo USB del mouse e a verificare che la cattura contenga solo lui.

/// Un pacchetto USB catturato. Alcuni campi servono solo ai test e a chi estende il kit.
#[allow(dead_code)]
#[derive(Debug, Clone)]
pub struct Packet {
    pub time: f64,
    pub irp_id: u64,
    /// 0 = dal PC al dispositivo, 1 = risposta del dispositivo (completamento)
    pub info: u8,
    pub bus: u16,
    pub device: u16,
    pub endpoint: u8,
    /// 0 isocrono, 1 interrupt, 2 control, 3 bulk
    pub transfer: u8,
    /// Solo per i control transfer: 0 setup, 1 dati, 2 stato, 3 completamento
    pub stage: Option<u8>,
    pub data: Vec<u8>,
}

/// Legge tutti i pacchetti completi presenti in `buf` (anche se il file è troncato).
pub fn parse(buf: &[u8]) -> Vec<Packet> {
    let mut out = Vec::new();
    if buf.len() < 24 {
        return out;
    }
    let le = match &buf[..4] {
        [0xD4, 0xC3, 0xB2, 0xA1] => true,
        [0xA1, 0xB2, 0xC3, 0xD4] => false,
        _ => return out,
    };
    let u32_at = |b: &[u8], o: usize| {
        let a = [b[o], b[o + 1], b[o + 2], b[o + 3]];
        if le { u32::from_le_bytes(a) } else { u32::from_be_bytes(a) }
    };
    let mut pos = 24;
    while pos + 16 <= buf.len() {
        let (sec, usec, incl) = (u32_at(buf, pos), u32_at(buf, pos + 4), u32_at(buf, pos + 8) as usize);
        let start = pos + 16;
        if start + incl > buf.len() {
            break;
        }
        let rec = &buf[start..start + incl];
        pos = start + incl;
        // Intestazione USBPcap (sempre little endian): 27 byte, +1 per i control transfer.
        if rec.len() < 27 {
            continue;
        }
        let hlen = u16::from_le_bytes([rec[0], rec[1]]) as usize;
        if hlen < 27 || hlen > rec.len() {
            continue;
        }
        let transfer = rec[22];
        out.push(Packet {
            time: sec as f64 + usec as f64 / 1e6,
            irp_id: u64::from_le_bytes(rec[2..10].try_into().unwrap()),
            info: rec[16] & 1,
            bus: u16::from_le_bytes([rec[17], rec[18]]),
            device: u16::from_le_bytes([rec[19], rec[20]]),
            endpoint: rec[21],
            transfer,
            stage: (transfer == 2 && hlen >= 28).then(|| rec[27]),
            data: rec[hlen..].to_vec(),
        });
    }
    out
}

/// Indirizzi USB con il loro VID/PID, dai descrittori di dispositivo presenti nella cattura
/// (USBPcapCMD --inject-descriptors li aggiunge all'inizio).
pub fn device_ids(packets: &[Packet]) -> Vec<(u16, u16, u16)> {
    let mut out = Vec::new();
    for p in packets {
        let d = &p.data;
        if p.transfer == 2 && p.info == 1 && d.len() >= 18 && d[0] == 0x12 && d[1] == 0x01 {
            let id = (p.device, u16::from_le_bytes([d[8], d[9]]), u16::from_le_bytes([d[10], d[11]]));
            if !out.contains(&id) {
                out.push(id);
            }
        }
    }
    out
}

#[cfg(test)]
pub mod tests {
    use super::*;

    /// Costruisce un pcap USBPcap minimo (usato anche dai test dell'analisi).
    pub fn build(packets: &[(u16, u8, u8, u8, Option<u8>, Vec<u8>)]) -> Vec<u8> {
        let mut f = vec![0xD4, 0xC3, 0xB2, 0xA1, 2, 0, 4, 0];
        f.extend_from_slice(&[0; 8]);
        f.extend_from_slice(&65535u32.to_le_bytes());
        f.extend_from_slice(&249u32.to_le_bytes());
        for (i, (device, info, endpoint, transfer, stage, data)) in packets.iter().enumerate() {
            let hlen = if stage.is_some() { 28 } else { 27 };
            let mut rec = Vec::new();
            rec.extend_from_slice(&(hlen as u16).to_le_bytes());
            rec.extend_from_slice(&(i as u64).to_le_bytes());
            rec.extend_from_slice(&0u32.to_le_bytes());
            rec.extend_from_slice(&0u16.to_le_bytes());
            rec.push(*info);
            rec.extend_from_slice(&1u16.to_le_bytes());
            rec.extend_from_slice(&device.to_le_bytes());
            rec.push(*endpoint);
            rec.push(*transfer);
            rec.extend_from_slice(&(data.len() as u32).to_le_bytes());
            if let Some(s) = stage {
                rec.push(*s);
            }
            rec.extend_from_slice(data);
            f.extend_from_slice(&(1000 + i as u32).to_le_bytes());
            f.extend_from_slice(&0u32.to_le_bytes());
            f.extend_from_slice(&(rec.len() as u32).to_le_bytes());
            f.extend_from_slice(&(rec.len() as u32).to_le_bytes());
            f.extend(rec);
        }
        f
    }

    #[test]
    fn finds_device_descriptor() {
        let mut desc = vec![0x12, 0x01, 0x00, 0x02, 0, 0, 0, 64];
        desc.extend_from_slice(&0x1532u16.to_le_bytes());
        desc.extend_from_slice(&0x0099u16.to_le_bytes());
        desc.extend_from_slice(&[0; 6]);
        let f = build(&[
            (3, 0, 0x80, 2, Some(0), vec![0x80, 0x06, 0, 1, 0, 0, 18, 0]),
            (3, 1, 0x80, 2, Some(3), desc),
            (5, 1, 0x81, 1, None, vec![1, 2, 3]),
        ]);
        let p = parse(&f);
        assert_eq!(p.len(), 3);
        assert_eq!(device_ids(&p), vec![(3, 0x1532, 0x0099)]);
        // un file troncato non deve far crashare
        assert_eq!(parse(&f[..f.len() - 2]).len(), 2);
    }
}
