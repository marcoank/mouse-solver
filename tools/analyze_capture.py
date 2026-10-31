"""Analizza una cattura del kit (capture.pcap + markers.json) e propone come il mouse codifica
DPI, polling, colori ed effetti.

Uso:  python -I tools/analyze_capture.py <cartella cattura> [report.md]

Cosa fa:
1. Estrae i comandi che il PC manda al mouse: SET_REPORT (feature/output) sui control transfer
   e i pacchetti interrupt OUT; e le risposte (GET_REPORT, interrupt IN subito dopo un comando).
2. Li divide per passo usando gli orari dei marcatori.
3. Scarta i comandi uguali in tutti i passi (richieste periodiche di stato) e confronta quelli che
   cambiano: per ogni posizione di byte che varia prova le codifiche note (DPI/50, DPI/100-1,
   u16 little/big endian, millisecondi del polling, terne RGB...).
4. Scrive un report Markdown con le ipotesi, dalla più probabile.
"""
import json
import struct
import sys
from collections import defaultdict
from pathlib import Path

# ---------------------------------------------------------------- pcap USBPcap


def read_pcap(path):
    data = Path(path).read_bytes()
    if len(data) < 24:
        return []
    endian = "<" if data[:4] == b"\xd4\xc3\xb2\xa1" else ">"
    pos, out = 24, []
    while pos + 16 <= len(data):
        sec, usec, incl, _ = struct.unpack(endian + "IIII", data[pos:pos + 16])
        rec = data[pos + 16:pos + 16 + incl]
        pos += 16 + incl
        if len(rec) < 27:
            continue
        hlen, irp, status, func, info, bus, dev, ep, transfer, dlen = struct.unpack("<HQIHBHHBBI", rec[:27])
        stage = rec[27] if transfer == 2 and hlen >= 28 else None
        out.append({"t": sec + usec / 1e6, "irp": irp, "info": info & 1, "dev": dev, "ep": ep,
                    "transfer": transfer, "stage": stage, "data": rec[hlen:]})
    return out


def extract(packets, address):
    """Comandi PC->mouse e risposte, in ordine di tempo."""
    cmds, pending = [], {}
    for p in packets:
        if p["dev"] != address:
            continue
        d = p["data"]
        if p["transfer"] == 2:  # control
            if p["info"] == 0 and p["stage"] == 0 and len(d) >= 8:
                req_type, req, w_value, w_index, w_len = struct.unpack("<BBHHH", d[:8])
                if req_type == 0x21 and req == 0x09:  # SET_REPORT
                    kind = {2: "output", 3: "feature"}.get(w_value >> 8, f"tipo{w_value >> 8}")
                    payload = d[8:]
                    entry = {"t": p["t"], "dir": "out", "kind": kind, "report_id": w_value & 0xFF, "data": payload}
                    cmds.append(entry)
                    if not payload:
                        pending[p["irp"]] = entry  # i dati arrivano nello stadio DATA
                elif req_type == 0xA1 and req == 0x01:  # GET_REPORT
                    pending[p["irp"]] = {"t": p["t"], "dir": "in", "kind": "get_feature", "report_id": w_value & 0xFF, "data": b""}
            elif p["irp"] in pending and d:
                e = pending.pop(p["irp"])
                e["data"] = d
                if e["dir"] == "in":
                    cmds.append(e)
        elif p["transfer"] == 1 and d:  # interrupt
            if not p["ep"] & 0x80 and p["info"] == 0:
                cmds.append({"t": p["t"], "dir": "out", "kind": "interrupt_out", "report_id": d[0], "data": d})
            elif p["ep"] & 0x80 and p["info"] == 1:
                # le risposte del protocollo arrivano subito dopo un comando; il movimento no
                last_out = next((c["t"] for c in reversed(cmds) if c["dir"] == "out"), None)
                if last_out is not None and p["t"] - last_out < 0.05:
                    cmds.append({"t": p["t"], "dir": "in", "kind": f"interrupt_in_ep{p['ep'] & 0x7F}", "report_id": d[0], "data": d})
    return cmds


# ---------------------------------------------------------------- ipotesi di codifica


def dpi_codes(v):
    v = int(v)
    c = {"DPI": v, "DPI/50": v // 50, "DPI/50-1": v // 50 - 1, "DPI/100": v // 100, "DPI/100-1": v // 100 - 1}
    return {k: x for k, x in c.items() if x >= 0}


def poll_codes(v):
    v = int(v)
    ms = 1000 // v
    return {"Hz": v, "millisecondi": ms, "ms-1": ms - 1, "indice 125/250/500/1000": {125: 0, 250: 1, 500: 2, 1000: 3}.get(v, -1),
            "indice 1000/500/250/125": {1000: 0, 500: 1, 250: 2, 125: 3}.get(v, -1), "bit (1<<log2 ms)": 1 << (ms.bit_length() - 1)}


def byte_views(data, off):
    """Interpretazioni del valore che inizia a `off`."""
    out = {"u8": data[off]}
    if off + 1 < len(data):
        out["u16 LE"] = data[off] | data[off + 1] << 8
        out["u16 BE"] = data[off] << 8 | data[off + 1]
    return out


def guess(kind, values, packets):
    """Per un gruppo di pacchetti (uno per passo) cerca offset+codifica che spiegano i valori."""
    hits = []
    n = min(len(p) for p in packets)
    for off in range(n):
        if len({p[off] for p in packets}) == 1:
            continue
        views = [byte_views(p, off) for p in packets]
        if kind == "dpi":
            codes = [dpi_codes(v) for v in values]
        elif kind == "polling":
            codes = [poll_codes(v) for v in values]
        elif kind == "dpi_active":
            codes = [{"indice da 1": int(v), "indice da 0": int(v) - 1} for v in values]
        elif kind == "brightness":
            codes = [{"percento": int(v), "0-255": round(int(v) * 255 / 100), "0-10": round(int(v) / 10)} for v in values]
        else:
            codes = None
        if codes:
            for view in views[0]:
                for enc in codes[0]:
                    if all(views[i].get(view) == codes[i][enc] for i in range(len(packets))):
                        hits.append(f"byte {off}: {enc} come {view}")
        if kind == "color" and off + 2 < n:
            if all(tuple(p[off:off + 3]) == tuple(bytes.fromhex(v)) for p, v in zip(packets, values)):
                hits.append(f"byte {off}..{off + 2}: colore R G B")
            elif all(tuple(p[off:off + 3]) == tuple(bytes.fromhex(v)[::-1]) for p, v in zip(packets, values)):
                hits.append(f"byte {off}..{off + 2}: colore B G R")
    return hits


def hexs(b):
    return " ".join(f"{x:02x}" for x in b)


def analyze(folder):
    folder = Path(folder)
    meta = json.loads((folder / "markers.json").read_text(encoding="utf-8"))
    cmds = extract(read_pcap(folder / "capture.pcap"), meta["usb_address"])
    markers = meta["markers"]
    for m in markers:
        m["cmds"] = [c for c in cmds if c["dir"] == "out" and m["start"] <= c["t"] <= m["end"] + 0.3]

    # comandi presenti identici in tutti i passi = rumore (richieste periodiche)
    def key(c):
        return (c["kind"], c["report_id"], bytes(c["data"]))
    common = set.intersection(*[set(map(key, m["cmds"])) for m in markers]) if markers else set()

    lines = [f"# Analisi cattura {meta['name']} ({meta['vid']}:{meta['pid']})", "",
             f"{len(cmds)} comandi/risposte del protocollo, {len(markers)} passi. "
             f"{len(common)} comandi uguali in tutti i passi scartati come rumore.", ""]

    by_kind = defaultdict(list)
    for m in markers:
        by_kind[m["kind"]].append(m)

    for kind, steps in by_kind.items():
        lines += [f"## {kind}", ""]
        # raggruppo i comandi "nuovi" di ogni passo per (tipo, report id, lunghezza, primi 2 byte)
        groups = defaultdict(dict)
        for m in steps:
            for c in m["cmds"]:
                if key(c) in common:
                    continue
                g = (c["kind"], c["report_id"], len(c["data"]), bytes(c["data"][:2]))
                groups[g].setdefault(m["value"], c["data"])
        if not groups:
            lines += ["Nessun comando nuovo in questi passi (il software forse non ha inviato nulla).", ""]
            continue
        values = [m["value"] for m in steps]
        ranked = []
        for g, per_value in groups.items():
            complete = all(v in per_value for v in values)
            pkts = [per_value[v] for v in values] if complete else []
            hits = guess(kind, values, pkts) if complete and len(values) > 1 else []
            ranked.append((len(hits), complete, g, per_value, hits))
        ranked.sort(key=lambda r: (-r[0], not r[1]))
        for n_hits, complete, (ckind, rid, length, prefix), per_value, hits in ranked[:6]:
            lines.append(f"### {ckind}, report 0x{rid:02x}, {length} byte, inizio {hexs(prefix)}")
            lines.append("")
            lines.append("| passo | dati |")
            lines.append("|---|---|")
            for v in values:
                d = per_value.get(v)
                lines.append(f"| {v} | `{hexs(d[:48])}{' ...' if d is not None and len(d) > 48 else ''}` |" if d else f"| {v} | (assente) |")
            lines.append("")
            if hits:
                lines += ["**Ipotesi:** " + "; ".join(hits), ""]
            elif complete:
                diff = [i for i in range(min(len(per_value[v]) for v in values)) if len({per_value[v][i] for v in values}) > 1]
                lines += [f"Byte che cambiano: {diff[:20]} (nessuna codifica nota riconosciuta)", ""]
            else:
                lines += ["Presente solo in alcuni passi.", ""]
    return "\n".join(lines) + "\n"


def main():
    folder = Path(sys.argv[1])
    report = analyze(folder)
    out = Path(sys.argv[2]) if len(sys.argv) > 2 else folder / "analisi.md"
    out.write_text(report, encoding="utf-8")
    print(report)
    print(f"Report salvato in {out}")


if __name__ == "__main__":
    main()
