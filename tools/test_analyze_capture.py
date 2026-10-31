"""Test dell'analisi su una cattura sintetica: un mouse finto che codifica
DPI come u16 LE di DPI/50, polling in millisecondi e colori R G B.

Uso:  python -I tools/test_analyze_capture.py
"""
import json
import struct
import sys
import tempfile
from pathlib import Path

sys.path.insert(0, str(Path(__file__).parent))
import analyze_capture  # noqa: E402

ADDR = 7


def record(t, irp, info, ep, transfer, stage, data):
    hlen = 28 if stage is not None else 27
    rec = struct.pack("<HQIHBHHBBI", hlen, irp, 0, 0, info, 1, ADDR, ep, transfer, len(data))
    if stage is not None:
        rec += bytes([stage])
    rec += data
    sec, usec = int(t), int((t - int(t)) * 1e6)
    return struct.pack("<IIII", sec, usec, len(rec), len(rec)) + rec


def set_feature(t, irp, payload):
    setup = struct.pack("<BBHHH", 0x21, 0x09, (3 << 8) | 0x05, 1, len(payload))
    return record(t, irp, 0, 0x00, 2, 0, setup + payload)


def main():
    pcap = struct.pack("<IHHiIII", 0xA1B2C3D4, 2, 4, 0, 0, 65535, 249)
    markers, t, irp = [], 1000.0, 1
    steps = [("dpi", v) for v in ("400", "800", "1600")] + [("polling", v) for v in ("125", "500", "1000")] \
        + [("color", v) for v in ("ff0000", "00ff00", "0000ff")]
    for kind, value in steps:
        start = t
        # rumore: stessa richiesta di stato in ogni passo
        pcap += set_feature(t + 0.1, irp, bytes([0x05, 0xAA, 0, 0, 0, 0, 0, 0])); irp += 1
        if kind == "dpi":
            payload = bytes([0x05, 0x11, 0x01, 0x00]) + struct.pack("<H", int(value) // 50) + bytes(2)
        elif kind == "polling":
            payload = bytes([0x05, 0x12, 0x00, 1000 // int(value), 0, 0, 0, 0])
        else:
            payload = bytes([0x05, 0x13, 0x01, 0x00, 0x00]) + bytes.fromhex(value)
        pcap += set_feature(t + 0.2, irp, payload); irp += 1
        t += 1.0
        markers.append({"kind": kind, "value": value, "label": "", "start": start, "end": t - 0.5})
    with tempfile.TemporaryDirectory() as d:
        Path(d, "capture.pcap").write_bytes(pcap)
        Path(d, "markers.json").write_text(json.dumps({"name": "Finto", "vid": "0x1234", "pid": "0x5678",
                                                       "usb_address": ADDR, "markers": markers}))
        report = analyze_capture.analyze(d)
    expected = ["byte 4: DPI/50 come u16 LE", "byte 3: millisecondi come u8", "byte 5..7: colore R G B"]
    missing = [e for e in expected if e not in report]
    if missing:
        print(report)
        raise SystemExit(f"FALLITO, mancano: {missing}")
    if "05 aa" in report:
        raise SystemExit("FALLITO: il rumore non è stato scartato")
    print("ok: tutte le codifiche riconosciute, rumore scartato")


if __name__ == "__main__":
    main()
