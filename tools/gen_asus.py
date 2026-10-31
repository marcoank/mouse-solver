"""Genera devices/asus_*.toml dai modelli di G-Helper (app/Peripherals/Mouse/Models/*.cs).

Il C# viene letto come testo: costruttori (PID, percorso HID, report ID) e metodi "override"
con valori semplici (DPI massimi, polling, zone luci...). Se un modello riscrive i pacchetti di
una funzione (protocollo diverso), quella funzione viene disattivata nel TOML invece di indovinare.

Uso:  python -I tools/gen_asus.py <cartella g-helper> <cartella devices>
"""
import json
import re
import sys
from pathlib import Path

GENERATED = "# generato da tools/gen_asus.py da G-Helper: non modificare a mano\n"
NOT_MICE = {"BalteusQi", "Balteus", "BulwarkDock"}
POLL = {"PR125Hz": 125, "PR250Hz": 250, "PR500Hz": 500, "PR1000Hz": 1000, "PR2000Hz": 2000,
        "PR4000Hz": 4000, "PR8000Hz": 8000}
ZONES = {"Logo": ("logo", 0x00), "Scrollwheel": ("scroll", 0x01), "Underglow": ("underglow", 0x02), "Dock": ("dock", 0x04)}
MODES = {"Static": "static", "Breathing": "breathing", "ColorCycle": "spectrum", "Rainbow": "wave", "React": "reactive", "Off": "off"}
ALL_MODES = ["Static", "Breathing", "ColorCycle", "Rainbow", "React", "Comet", "BatteryState", "Off"]

DPI_PACKETS = {"GetReadDPIPacket", "GetUpdateDPIPacket", "ParseDPI", "ParseDPIProfile", "ParseProfile",
               "GetChangeDPIProfilePacket", "GetChangeDPIProfilePacket2", "GetReadProfilePacket", "GetSetDPIProfileCountPacket"}
POLL_PACKETS = {"GetUpdatePollingRatePacket", "ParsePollingRate", "GetReadPollingRatePacket"}
LIGHT_PACKETS = {"GetReadLightingModePacket", "GetUpdateLightingModePacket", "ReadLightingSetting",
                 "ParseLightingSetting", "IndexForLightingMode", "LightingModeForIndex"}
FATAL = {"WriteForResponse", "SetProvider", "GetSaveProfilePacket", "CheckConnection"}

CLASS_RE = re.compile(r"public class (\w+)\s*:\s*(\w+)")
CTOR_RE = re.compile(r"(?:public|protected) (\w+)\(([^)]*)\)\s*:\s*base\(([^)]*)\)")
OVERRIDE_RE = re.compile(r"override [\w\[\]<>?,\s()]*?\s(\w+)\(([^)]*)\)\s*\{")


def body_from(text, start):
    depth, i = 0, start
    while i < len(text):
        if text[i] == "{":
            depth += 1
        elif text[i] == "}":
            depth -= 1
            if depth == 0:
                return text[start + 1:i]
        i += 1
    return ""


def parse(root):
    classes = {}
    for f in sorted((root / "app/Peripherals/Mouse/Models").glob("*.cs")):
        text = f.read_text(encoding="utf-8-sig")
        starts = [(m.start(), m.group(1), m.group(2)) for m in CLASS_RE.finditer(text)]
        for i, (pos, name, parent) in enumerate(starts):
            end = starts[i + 1][0] if i + 1 < len(starts) else len(text)
            chunk = text[pos:end]
            ctors = []
            for m in CTOR_RE.finditer(chunk):
                if m.group(1) != name:
                    continue
                params = [p.split()[-1] for p in m.group(2).split(",") if p.strip()]
                args = [a.strip() for a in m.group(3).split(",") if a.strip()]
                ctors.append((params, args))
            overrides = {}
            for m in OVERRIDE_RE.finditer(chunk):
                overrides[m.group(1)] = body_from(chunk, m.end() - 1)
            classes[name] = {"parent": parent, "ctors": ctors, "overrides": overrides}
    return classes


def literal(v):
    v = v.strip()
    if re.fullmatch(r"0x[0-9a-fA-F]+", v):
        return int(v, 16)
    if re.fullmatch(r"[\d_]+", v):
        return int(v.replace("_", ""))
    if v in ("true", "false"):
        return v == "true"
    if v.startswith('"'):
        return v.strip('"')
    return v


def resolve_ctor(classes, name, args):
    """Segue la catena dei costruttori fino ad AsusMouse(vid, pid, path, wireless[, reportId])."""
    while name != "AsusMouse":
        c = classes.get(name)
        if c is None:
            return None
        match = [ct for ct in c["ctors"] if len(ct[0]) == len(args)]
        if not match:
            return None
        params, base_args = match[0]
        env = dict(zip(params, args))
        args = [env.get(a, literal(a)) for a in base_args]
        name = c["parent"]
    return args


def chain(classes, name):
    out = []
    while name in classes:
        out.append(name)
        name = classes[name]["parent"]
    return out


def lookup(classes, name, method):
    for c in chain(classes, name):
        if method in classes[c]["overrides"]:
            return classes[c]["overrides"][method], c
    return None, None


def ret_literal(body):
    m = re.search(r"return\s+([^;]+);", body or "")
    return literal(m.group(1)) if m else None


def lighting_modes(classes, name):
    body, owner = lookup(classes, name, "IsLightingModeSupported")
    if body is None:
        return list(ALL_MODES)
    if "base.IsLightingModeSupported" in body:
        parent_modes = lighting_modes(classes, classes[owner]["parent"])
        excluded = re.findall(r"lightingMode != LightingMode\.(\w+)", body)
        return [m for m in parent_modes if m not in excluded]
    return re.findall(r"lightingMode == LightingMode\.(\w+)", body)


def model(classes, name):
    c = classes[name]
    public = [ct for ct in c["ctors"] if not ct[0]]
    if not public or name in NOT_MICE:
        return None
    args = resolve_ctor(classes, name, [])
    if not args or len(args) < 4 or not isinstance(args[1], int):
        return None
    overridden = set()
    for cl in chain(classes, name):
        overridden |= set(classes[cl]["overrides"])
    if overridden & FATAL:
        return None

    def get(method, default):
        body, _ = lookup(classes, name, method)
        v = ret_literal(body)
        if v is None or (isinstance(v, str) and not isinstance(default, str)):
            return default
        return v

    path = args[2].lower()
    m_if = re.search(r"mi_(\d+)", path)
    m_col = re.search(r"(col\d+)", path)
    display = get("GetDisplayName", name)
    profiles = get("DPIProfileCount", 1)
    poll_body, _ = lookup(classes, name, "SupportedPollingrates")
    rates = sorted(POLL[p] for p in re.findall(r"PollingRate\.(PR\d+Hz)", (poll_body or "").split("BoosterPollingrates();")[-1]))
    zones_body, _ = lookup(classes, name, "SupportedLightingZones")
    zones = [ZONES[z] for z in re.findall(r"LightingZone\.(\w+)", zones_body or "LightingZone.Logo") if z in ZONES]
    has_rgb = get("HasRGB", False)
    count_settable = get("CanChangeDPICount", False)

    return {
        "class": name,
        "name": display if display.startswith(("ROG", "TUF", "ASUS")) else f"ROG {display}",
        "pid": args[1],
        "interface": int(m_if.group(1)) if m_if else None,
        "path_contains": m_col.group(1) if m_col else None,
        "report_id": args[4] if len(args) > 4 and isinstance(args[4], int) else 0,
        "packet_size": get("USBPacketSize", 65),
        "dpi": not (overridden & DPI_PACKETS),
        "polling": bool(rates) and get("CanSetPollingRate", True) is not False and not (overridden & POLL_PACKETS),
        "lighting": bool(has_rgb) and not (overridden & LIGHT_PACKETS),
        "dpi_xy": get("HasXYDPI", False),
        "dpi_colors": get("HasDPIColors", False),
        "dpi_increment": get("DPIIncrements", 50),
        "dpi_min": get("MinDPI", 100),
        "dpi_max": get("MaxDPI", 2000),
        "stages": profiles,
        "count_settable": bool(count_settable),
        "active_settable": profiles > 1 and get("CanChangeDPIProfile", True) is not False,
        "brightness_max": get("MaxBrightness", 100),
        "rates": rates,
        "zones": zones,
        "modes": [MODES[m] for m in lighting_modes(classes, name) if m in MODES],
    }


def toml(m, names):
    order = ["static", "breathing", "spectrum", "wave", "reactive", "off"]
    lines = [GENERATED,
             f"name = {json.dumps(m['name'])}",
             "vid = 0x0B05",
             f"pid = 0x{m['pid']:04X}",
             'driver = "asus"',
             'status = "experimental"']
    if len(names) > 1:
        lines.append(f"notes = {json.dumps('Ricevitore condiviso da: ' + ', '.join(names) + '. Il mouse deve essere acceso.')}")
    lines += ["", "[asus]"]
    if m["interface"] is not None:
        lines.append(f"interface = {m['interface']}")
    if m["path_contains"]:
        lines.append(f'path_contains = "{m["path_contains"]}"')
    lines += [f"report_id = 0x{m['report_id']:02X}",
              f"packet_size = {m['packet_size']}",
              f"dpi = {str(m['dpi']).lower()}",
              f"polling = {str(m['polling']).lower()}",
              f"lighting = {str(m['lighting']).lower()}",
              f"dpi_xy = {str(m['dpi_xy']).lower()}",
              f"dpi_colors = {str(m['dpi_colors']).lower()}",
              f"dpi_increment = {m['dpi_increment']}",
              f"dpi_count_settable = {str(m['count_settable']).lower()}",
              f"dpi_active_settable = {str(m['active_settable']).lower()}",
              f"brightness_max = {m['brightness_max']}"]
    zones = m["zones"] if m["lighting"] else []
    lines.append("zones = { " + ", ".join(f"{n} = 0x{c:02X}" for n, c in zones) + " }")
    stages = m["stages"] if m["dpi"] else 0
    lines += ["", "[caps]",
              f"dpi_min = {m['dpi_min']}",
              f"dpi_max = {m['dpi_max']}",
              f"dpi_step = {m['dpi_increment']}",
              f"min_stages = {min(2, stages) if m['count_settable'] else stages}",
              f"max_stages = {stages}",
              f"active_stage_settable = {str(m['active_settable']).lower()}",
              f"polling = {m['rates'] if m['polling'] else []}",
              "zones = " + json.dumps([n for n, _ in zones]),
              "effects = " + json.dumps([e for e in order if e in m["modes"]] if zones else []),
              ""]
    return "\n".join(lines)


def merge(group):
    """Più modelli con lo stesso PID (ricevitore OMNI): capacità comuni a tutti."""
    m = dict(group[0])
    for o in group[1:]:
        # Se cambia il formato dei DPI non posso sapere quale mouse c'è dietro il ricevitore.
        if any(o[k] != m[k] for k in ("dpi_xy", "dpi_colors", "dpi_increment")):
            m["dpi"] = False
        m["dpi"] = m["dpi"] and o["dpi"]
        m["polling"] = m["polling"] and o["polling"]
        m["lighting"] = m["lighting"] and o["lighting"]
        m["dpi_max"] = min(m["dpi_max"], o["dpi_max"])
        m["dpi_min"] = max(m["dpi_min"], o["dpi_min"])
        m["stages"] = min(m["stages"], o["stages"])
        m["count_settable"] = m["count_settable"] and o["count_settable"]
        m["rates"] = [r for r in m["rates"] if r in o["rates"]]
        m["zones"] = [z for z in m["zones"] if z in o["zones"]]
        m["modes"] = [x for x in m["modes"] if x in o["modes"]]
        m["brightness_max"] = min(m["brightness_max"], o["brightness_max"])
    if len(group) > 1:
        m["name"] = "ROG ricevitore OMNI" if m["pid"] == 0x1ACE else m["name"]
    return m


def main():
    root, devdir = Path(sys.argv[1]), Path(sys.argv[2])
    classes = parse(root)
    by_pid = {}
    for name in classes:
        m = model(classes, name)
        if m:
            by_pid.setdefault(m["pid"], []).append(m)
    for f in devdir.glob("asus_*.toml"):
        if f.read_text(encoding="utf-8").startswith("# generato"):
            f.unlink()
    written = 0
    for pid, group in sorted(by_pid.items()):
        m = merge(group)
        slug = re.sub(r"[^a-z0-9]+", "_", m["name"].lower()).strip("_").removeprefix("rog_")
        (devdir / f"asus_{slug}.toml").write_text(toml(m, [g["name"] for g in group]), encoding="utf-8")
        written += 1
    print(f"Scritti {written} file ASUS da {sum(len(g) for g in by_pid.values())} modelli.")


if __name__ == "__main__":
    main()
