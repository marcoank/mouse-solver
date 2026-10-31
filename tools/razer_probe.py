"""
Razer Basilisk V3 (1532:0099) - test di sola LETTURA.
Non scrive nulla sul mouse: legge firmware, seriale, DPI, stadi DPI e polling rate.

Requisiti:  pip install hidapi
Uso:        python razer_probe.py
"""
import time
import hid

VID, PID = 0x1532, 0x0099
TRANSACTION_IDS = [0x1F, 0xFF, 0x3F]   # 0x1F è quello usato da OpenRazer per il Basilisk V3

STATUS = {0x00: "nuovo", 0x01: "occupato", 0x02: "ok", 0x03: "errore",
          0x04: "timeout", 0x05: "non supportato"}


def build_report(tid, cmd_class, cmd_id, size, args=b""):
    r = bytearray(90)
    r[1] = tid
    r[5] = size
    r[6] = cmd_class
    r[7] = cmd_id
    r[8:8 + len(args)] = args
    crc = 0
    for b in r[2:88]:
        crc ^= b
    r[88] = crc
    return r


def transact(dev, tid, cmd_class, cmd_id, size, args=b""):
    req = build_report(tid, cmd_class, cmd_id, size, args)
    dev.send_feature_report(bytes([0x00]) + bytes(req))   # 0x00 = report ID
    time.sleep(0.06)
    resp = dev.get_feature_report(0x00, 91)
    resp = bytes(resp)
    if len(resp) == 91:
        resp = resp[1:]                                   # togli il report ID
    if len(resp) < 90:
        return None
    if resp[6] != cmd_class or resp[7] != cmd_id:
        return None
    return resp


def try_interface(info):
    dev = hid.device()
    try:
        dev.open_path(info["path"])
    except Exception:
        return None
    try:
        for tid in TRANSACTION_IDS:
            try:
                r = transact(dev, tid, 0x00, 0x81, 0x02)  # versione firmware
            except Exception:
                continue
            if r and r[0] == 0x02:
                return dev, tid
    except Exception:
        pass
    dev.close()
    return None


def main():
    infos = hid.enumerate(VID, PID)
    if not infos:
        print("Mouse non trovato. È collegato col cavo?")
        return

    print(f"Trovate {len(infos)} interfacce HID del Basilisk V3. Provo a parlarci...\n")
    found = None
    for info in infos:
        res = try_interface(info)
        tag = f"interfaccia {info['interface_number']}  usage {info['usage_page']:#06x}/{info['usage']:#04x}"
        if res:
            print(f"  [OK]  {tag}")
            found = res
            break
        else:
            print(f"  [--]  {tag}")

    if not found:
        print("\nNessuna interfaccia ha risposto.")
        print("Prova a chiudere Razer Synapse (anche dall'icona vicino all'orologio) e rilancia.")
        return

    dev, tid = found
    print(f"\nComunicazione riuscita (transaction id {tid:#04x}).\n")

    r = transact(dev, tid, 0x00, 0x81, 0x02)
    print(f"Firmware:      v{r[8]}.{r[9]}")

    r = transact(dev, tid, 0x00, 0x82, 0x16)
    if r and r[0] == 0x02:
        serial = bytes(r[8:8 + 22]).split(b"\x00")[0].decode(errors="ignore")
        print(f"Seriale:       {serial}")

    r = transact(dev, tid, 0x04, 0x85, 0x07, b"\x01")
    if r and r[0] == 0x02:
        x = (r[9] << 8) | r[10]
        y = (r[11] << 8) | r[12]
        print(f"DPI attuali:   {x} x {y}")
    else:
        print(f"DPI attuali:   risposta {STATUS.get(r[0], '?') if r else 'nessuna'}")

    r = transact(dev, tid, 0x04, 0x86, 0x26, b"\x01")
    if r and r[0] == 0x02:
        active, count = r[9], r[10]
        stages = []
        for i in range(count):
            o = 11 + i * 7
            stages.append((r[o], (r[o + 1] << 8) | r[o + 2]))
        txt = ", ".join(f"{'*' if sid == active else ''}{dpi}" for sid, dpi in stages)
        print(f"Stadi DPI:     {txt}   (* = attivo)")

    r = transact(dev, tid, 0x00, 0x85, 0x01)
    if r and r[0] == 0x02:
        hz = {0x01: 1000, 0x02: 500, 0x08: 125}.get(r[8], f"codice {r[8]:#04x}")
        print(f"Polling rate:  {hz} Hz")

    dev.close()
    print("\nSe vedi DPI e polling corretti, il software leggero è fattibile al 100%.")


if __name__ == "__main__":
    main()
