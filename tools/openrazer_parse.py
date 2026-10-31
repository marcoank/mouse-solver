"""Legge razermouse_driver.c di OpenRazer (solo come testo, senza eseguirlo) e ricava,
per ogni PID e ogni funzione sysfs, il costruttore di richiesta usato e il transaction id.

Uso come modulo da gen_razer.py, oppure:  python openrazer_parse.py <cartella openrazer>
"""
import re
import sys
from collections import defaultdict
from pathlib import Path

FUNC_RE = re.compile(r"^static (?:ssize_t|int|void|bool) (?:__must_check )?(\w+)\(")
CASE_RE = re.compile(r"case (USB_DEVICE_ID_RAZER_\w+)\s*:")
BUILDER_RE = re.compile(r"(?:request|report)\s*=\s*(razer_\w+)\(([^;]*)\);")
TID_RE = re.compile(r"transaction_id\.id\s*=\s*(0x[0-9a-fA-F]+)")


def pid_macros(root):
    out = {}
    for line in (root / "driver" / "razermouse_driver.h").read_text(encoding="utf-8").splitlines():
        m = re.match(r"#define (USB_DEVICE_ID_RAZER_\w+)\s+(0x[0-9a-fA-F]+)", line)
        if m:
            out[m.group(1)] = int(m.group(2), 16)
    return out


def parse(root):
    """{pid: {funzione: {"builders": [(nome, argomenti)], "tid": int|None}}}
    Se una funzione ha più switch (es. polling inviato due volte), i costruttori si accumulano."""
    root = Path(root)
    macros = pid_macros(root)
    lines = (root / "driver" / "razermouse_driver.c").read_text(encoding="utf-8").splitlines()
    result = defaultdict(lambda: defaultdict(lambda: {"builders": [], "tid": None}))

    func = None
    pre_builders, pre_tid = [], None   # prima dello switch (valgono per tutti i PID)
    group, group_builders, group_tid = [], [], None
    in_switch = False

    def flush():
        nonlocal group, group_builders, group_tid
        for name in group:
            pid = macros.get(name)
            if pid is None:
                continue
            e = result[pid][func]
            if not e["builders"]:
                e["builders"] = list(pre_builders)
            e["builders"] += group_builders
            if e["tid"] is None:
                e["tid"] = group_tid if group_tid is not None else pre_tid
        group, group_builders, group_tid = [], [], None

    for line in lines:
        m = FUNC_RE.match(line)
        if m:
            flush()
            name = m.group(1)
            func = name if name.startswith("razer_attr_") else None
            pre_builders, pre_tid, in_switch = [], None, False
            continue
        if func is None:
            continue
        if "switch" in line and "usb_pid" in line:
            in_switch = True
            continue
        cases = CASE_RE.findall(line)
        if cases:
            if group_builders or group_tid is not None:
                flush()
            group.extend(cases)
            continue
        b = BUILDER_RE.search(line)
        t = TID_RE.search(line)
        if not in_switch:
            if b:
                pre_builders.append((b.group(1), b.group(2).strip()))
            if t:
                pre_tid = int(t.group(1), 16)
            continue
        if b:
            group_builders.append((b.group(1), b.group(2).strip()))
        if t:
            group_tid = int(t.group(1), 16)
        if "break;" in line or "return" in line:
            flush()
        if line.startswith("}"):
            flush()
            func = None
    return result


if __name__ == "__main__":
    res = parse(sys.argv[1])
    funcs = defaultdict(lambda: defaultdict(int))
    for pid, fs in res.items():
        for f, e in fs.items():
            key = ",".join(sorted({b[0] for b in e["builders"]})) or "-"
            funcs[f][key] += 1
    for f in sorted(funcs):
        print(f)
        for k, n in sorted(funcs[f].items(), key=lambda x: -x[1]):
            print(f"    {n:3d}  {k}")
