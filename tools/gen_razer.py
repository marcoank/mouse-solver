"""Genera devices/razer_*.toml per tutti i mouse supportati da OpenRazer.

Legge (solo come testo / AST, senza eseguire niente):
  - daemon/openrazer_daemon/hardware/mouse.py  -> nome, PID, metodi, DPI massimi, polling
  - driver/razermouse_driver.c                 -> transaction id e variante di protocollo per PID

Uso:  python -I tools/gen_razer.py <cartella sorgenti openrazer> <cartella devices>
I file scritti a mano (senza la riga "# generato") non vengono toccati.
"""
import ast
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).parent))
from openrazer_parse import parse  # noqa: E402

GENERATED = "# generato da tools/gen_razer.py da OpenRazer: non modificare a mano\n"
LEDS = {"logo": 0x04, "scroll": 0x01, "backlight": 0x05, "left": 0x11, "right": 0x10}
EFFECT_METHODS = {
    "static": "static",
    "spectrum": "spectrum",
    "wave": "wave",
    "breath_single": "breathing",
    "reactive": "reactive",
    "none": "off",
}


def daemon_devices(path):
    tree = ast.parse(Path(path).read_text(encoding="utf-8"))
    classes = {}
    for node in tree.body:
        if not isinstance(node, ast.ClassDef):
            continue
        attrs = {"_bases": [b.id for b in node.bases if isinstance(b, ast.Name)], "_doc": ast.get_docstring(node) or ""}
        for st in node.body:
            if isinstance(st, ast.Assign) and len(st.targets) == 1 and isinstance(st.targets[0], ast.Name):
                attrs[st.targets[0].id] = st.value
        classes[node.name] = attrs

    def value(cls, key):
        c = classes.get(cls)
        if c is None:
            return None
        if key in c:
            return evaluate(c[key])
        for b in c["_bases"]:
            v = value(b, key)
            if v is not None:
                return v
        return None

    def evaluate(n):
        if isinstance(n, ast.BinOp) and isinstance(n.op, ast.Add):
            return evaluate(n.left) + evaluate(n.right)
        if isinstance(n, ast.Attribute) and isinstance(n.value, ast.Name):
            return value(n.value.id, n.attr)
        try:
            return ast.literal_eval(n)
        except ValueError:
            return None

    out = []
    for name, c in classes.items():
        pid = value(name, "USB_PID")
        if pid is None or name.startswith("__"):
            continue
        doc = c["_doc"].strip().splitlines()[0] if c["_doc"] else name
        pretty = doc.replace("Class for the ", "").replace("Class for ", "").strip().rstrip(".")
        out.append({
            "class": name,
            "name": pretty,
            "pid": pid,
            "methods": value(name, "METHODS") or [],
            "dpi_max": value(name, "DPI_MAX"),
            "poll_rates": value(name, "POLL_RATES"),
        })
    return out


def builders(info, func):
    return [b[0] for b in info.get(func, {}).get("builders", [])]


def generate(dev, info):
    m = set(dev["methods"])
    tid = info.get("razer_attr_read_firmware_version", {}).get("tid")
    if tid is None:
        return None, "transaction id sconosciuto"

    # Polling
    poll_b = builders(info, "razer_attr_write_poll_rate")
    if "set_poll_rate" not in m:
        polling = "none"
    elif "razer_chroma_misc_set_polling_rate2" in poll_b:
        polling = "v2_dual" if poll_b.count("razer_chroma_misc_set_polling_rate2") > 1 else "v2"
    elif "razer_chroma_misc_set_polling_rate" in poll_b:
        polling = "standard"
    else:
        polling = "none"
    rates = dev["poll_rates"] or [125, 500, 1000]
    if polling == "standard":
        rates = [r for r in rates if r in (125, 500, 1000)]

    # DPI (solo la versione a 2 byte; i vecchi mouse "dpi_xy_byte" sono esclusi)
    dpi = "set_dpi_xy" in m
    stages = "set_dpi_stages" in m and "razer_chroma_misc_set_dpi_stages" in builders(info, "razer_attr_write_dpi_stages")

    # Luci
    static_b = builders(info, "razer_attr_write_matrix_effect_static_common")
    if "razer_chroma_extended_matrix_effect_static" in static_b:
        lighting = "extended"
    elif "razer_chroma_standard_set_led_state" in static_b:
        lighting = "classic"
    else:
        lighting = "none"
    zones, effects = [], set()
    if lighting != "none":
        for z in LEDS:
            zone_effects = {e for k, e in EFFECT_METHODS.items() if f"set_{z}_{k}" in m}
            if zone_effects:
                zones.append(z)
                effects |= zone_effects
        if lighting == "classic":
            effects &= {"static", "breathing", "spectrum", "off"}
    brightness = any(f"set_{z}_brightness" in m for z in zones)

    if not (dpi or polling != "none" or zones):
        return None, "niente di configurabile con i protocolli supportati"

    tids = {}
    for key, func in [("dpi", "razer_attr_write_dpi"), ("dpi_stages", "razer_attr_write_dpi_stages"),
                      ("polling", "razer_attr_write_poll_rate"),
                      ("lighting", "razer_attr_write_matrix_effect_static_common")]:
        t = info.get(func, {}).get("tid")
        if t is not None and t != tid:
            tids[key] = t

    order = ["static", "breathing", "spectrum", "wave", "reactive", "off"]
    lines = [GENERATED,
             f'name = "{dev["name"]}"',
             "vid = 0x1532",
             f'pid = 0x{dev["pid"]:04X}',
             'driver = "razer"',
             'status = "experimental"',
             "",
             "[razer]",
             f"transaction_id = 0x{tid:02X}",
             f'polling = "{polling}"',
             f'lighting = "{lighting}"',
             f"dpi = {str(dpi).lower()}",
             f"dpi_stages = {str(stages).lower()}",
             f"brightness = {str(brightness).lower()}"]
    if tids:
        lines.append("transaction_ids = { " + ", ".join(f"{k} = 0x{v:02X}" for k, v in tids.items()) + " }")
    if zones:
        lines.append("leds = { " + ", ".join(f"{z} = 0x{LEDS[z]:02X}" for z in zones) + " }")
    lines += ["",
              "[caps]",
              "dpi_min = 100",
              f"dpi_max = {dev['dpi_max'] or 16000}",
              "dpi_step = 50",
              f"max_stages = {5 if stages else 1}",
              f"polling = {rates if polling != 'none' else []}",
              f"zones = {zones}".replace("'", '"'),
              f"effects = {[e for e in order if e in effects]}".replace("'", '"'),
              ""]
    return "\n".join(lines), None


def main():
    src, devdir = Path(sys.argv[1]), Path(sys.argv[2])
    per_pid = parse(src)
    manual_pids = set()
    for f in devdir.glob("*.toml"):
        text = f.read_text(encoding="utf-8")
        if not text.startswith("# generato") and "0x1532" in text:
            for line in text.splitlines():
                if line.startswith("pid"):
                    manual_pids.add(int(line.split("=")[1].split("#")[0].strip(), 16))
    written, skipped = 0, []
    for f in devdir.glob("razer_*.toml"):
        if f.read_text(encoding="utf-8").startswith("# generato"):
            f.unlink()
    for dev in daemon_devices(src / "daemon/openrazer_daemon/hardware/mouse.py"):
        if dev["pid"] in manual_pids:
            continue
        text, why = generate(dev, per_pid.get(dev["pid"], {}))
        if text is None:
            skipped.append(f'{dev["name"]}: {why}')
            continue
        slug = "".join(ch if ch.isalnum() else "_" for ch in dev["name"].lower()).strip("_")
        while "__" in slug:
            slug = slug.replace("__", "_")
        slug = slug.removeprefix("razer_")
        path = devdir / f"razer_{slug}.toml"
        if path.exists():  # stesso nome, PID diverso (es. revisioni)
            path = devdir / f"razer_{slug}_{dev['pid']:04x}.toml"
        path.write_text(text, encoding="utf-8")
        written += 1
    print(f"Scritti {written} file Razer. Saltati {len(skipped)}:")
    for s in skipped:
        print("  -", s)


if __name__ == "__main__":
    main()
