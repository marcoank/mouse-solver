"""Genera devices/steelseries_*.toml da rivalcfg (licenza WTFPL), senza eseguirne il codice.

Le codifiche di rivalcfg (range, choice, multidpi...) vengono trasformate qui in tabelle
"DPI -> codice" e "Hz -> byte", così il driver Rust non deve conoscere i singoli handler.

Uso:  python -I tools/gen_steelseries.py <cartella rivalcfg> <cartella devices>
"""
import json
import subprocess
import sys
from pathlib import Path

GENERATED = "# generato da tools/gen_steelseries.py da rivalcfg: non modificare a mano\n"
HERE = Path(__file__).parent


def toml_list(v):
    return "[" + ", ".join(toml_value(x) for x in v) + "]"


def toml_value(v):
    if isinstance(v, bool):
        return str(v).lower()
    if isinstance(v, int):
        return f"0x{v:02X}" if 0 <= v < 256 else str(v)
    if isinstance(v, str):
        return json.dumps(v)
    if isinstance(v, list):
        return toml_list(v)
    if isinstance(v, dict):
        return "{ " + ", ".join(f"{json.dumps(str(k))} = {toml_value(x)}" for k, x in v.items() if x is not None) + " }"
    raise TypeError(v)


def as_bytes(v):
    return v if isinstance(v, list) else [v]


def cmd(s):
    out = {"report": s["report_type"], "command": s["command"]}
    if s.get("command_suffix"):
        out["suffix"] = s["command_suffix"]
    if s.get("packet_length"):
        out["packet_length"] = s["packet_length"]
    if s.get("readback_length"):
        out["readback"] = s["readback_length"]
    return out


def range_table(s):
    a, b, step = s["input_range"]
    ins = list(range(a, b + 1, step))
    if "output_choices" in s:
        return [[int(k), v] for k, v in sorted(((int(k), v) for k, v in s["output_choices"].items()))]
    o0, o1, ostep = s["output_range"]
    outs = list(range(o0, o1 + 1, ostep))
    return [[i, o] for i, o in zip(ins, outs)]


def dpi_section(settings):
    if "sensitivity" in settings:
        s = settings["sensitivity"]
        vt = s["value_type"]
        if vt not in ("multidpi_range", "multidpi_range_choice", "multidpi_range_choice_xy"):
            return None
        return {
            "mode": "multi",
            "cmd": cmd(s),
            "codes": range_table(s),
            "code_bytes": s.get("dpi_length_byte", 1),
            "count_mode": s.get("count_mode", "number"),
            "first_preset": s.get("first_preset", 0),
            "xy": s.get("xy_mapping", "none"),
            "max_presets": s["max_preset_count"],
        }
    presets = [settings[k] for k in ("sensitivity1", "sensitivity2", "sensitivity3") if k in settings]
    if not presets:
        return None
    s = presets[0]
    if s["value_type"] == "choice":
        codes = sorted([int(k), v] for k, v in s["choices"].items())
    elif s["value_type"] in ("range", "range_choice"):
        codes = range_table(s)
    else:
        return None
    return {
        "mode": "presets",
        "cmds": [cmd(p) for p in presets],
        "codes": codes,
        "code_bytes": s.get("range_length_byte", 1),
    }


def zones(settings):
    out = []
    for key, s in settings.items():
        if not key.endswith("color") or key == "reactive_color":
            continue
        vt = s.get("value_type")
        if vt not in ("rgbcolor", "rgbgradient", "rgbgradientv2") or not s.get("command"):
            continue
        name = {"color": "logo"}.get(key, key[: -len("_color")])
        z = {"zone": name, "kind": vt, "cmd": cmd(s)}
        if "led_id" in s:
            z["led_id"] = s["led_id"]
        if vt == "rgbgradient":
            z["header"] = s["rgbgradient_header"]
        if vt == "rgbgradientv2":
            z["header"] = s["rgbgradientv2_header"]
        out.append(z)
    return out


def effect_section(settings):
    for key, names in (("light_effect", {"static": "steady", "breathing": "breath", "spectrum": "rainbow-shift"}),
                       ("default_lighting", {"static": "off", "spectrum": "rainbow"})):
        s = settings.get(key)
        if not s or s.get("value_type") != "choice":
            continue
        values = {eff: as_bytes(s["choices"][c]) for eff, c in names.items() if c in s["choices"]}
        if values:
            return {"cmd": cmd(s), "values": values}
    return None


def brightness_section(settings):
    s = settings.get("led_brightness")
    if s and s.get("value_type") == "range" and s["input_range"][0] == 0:
        return {"cmd": cmd(s), "max": s["input_range"][1], "code_bytes": s.get("range_length_byte", 1)}
    return None


def generate(name, p):
    st = p["settings"]
    dpi = dpi_section(st)
    poll = st.get("polling_rate")
    zs = zones(st)
    eff = effect_section(st)
    bri = brightness_section(st)
    if not (dpi or poll or zs):
        return None
    m0 = p["models"][0]
    lines = [GENERATED,
             f'name = {json.dumps(p["name"])}',
             f'vid = 0x{m0["vendor_id"]:04X}',
             f'pid = 0x{m0["product_id"]:04X}']
    if len(p["models"]) > 1:
        lines.append("aliases = [" + ", ".join(f"[0x{m['vendor_id']:04X}, 0x{m['product_id']:04X}]" for m in p["models"][1:]) + "]")
    lines += ['driver = "steelseries"', 'status = "experimental"',
              'notes = "Questo mouse non permette di rileggere le impostazioni: l\'app mostra le ultime scritte da qui. Usa Misura polling per controllare."',
              "", "[steelseries]", f"interface = {m0['endpoint']}"]
    if p.get("save_command"):
        lines.append(f"save = {toml_value(cmd(p['save_command']))}")
    fw = p.get("firmware_version")
    if fw:
        lines.append(f"firmware = {toml_value(cmd(fw))}")
    if poll and poll.get("value_type") == "choice":
        lines += ["", "[steelseries.polling]", f"cmd = {toml_value(cmd(poll))}",
                  "values = " + toml_value({str(k): as_bytes(v) for k, v in poll["choices"].items()})]
    if dpi:
        lines += ["", "[steelseries.dpi]"]
        for k, v in dpi.items():
            if k == "codes":
                lines.append("codes = [" + ", ".join(f"[{a}, {b}]" for a, b in v) + "]")
            else:
                lines.append(f"{k} = {toml_value(v)}")
    if eff:
        lines += ["", "[steelseries.effect]", f"cmd = {toml_value(eff['cmd'])}", f"values = {toml_value(eff['values'])}"]
    if bri:
        lines += ["", "[steelseries.brightness]"] + [f"{k} = {toml_value(v)}" for k, v in bri.items()]
    for z in zs:
        lines += ["", "[[steelseries.zones]]"] + [f"{k} = {toml_value(v)}" for k, v in z.items()]

    # Capacità per la GUI
    effects = ["static", "off"]
    if any(z["kind"] == "rgbgradient" for z in zs) or (eff and "spectrum" in eff["values"]):
        effects.insert(1, "spectrum")
    if eff and "breathing" in eff["values"]:
        effects.insert(1, "breathing")
    lines += ["", "[caps]"]
    if dpi:
        values = [a for a, _ in dpi["codes"]]
        steps = sorted({b - a for a, b in zip(values, values[1:])})
        lines += [f"dpi_min = {values[0]}", f"dpi_max = {values[-1]}", f"dpi_step = {steps[0] if steps else 100}"]
        if len(steps) > 1:
            lines.append(f"dpi_values = {values}")
        if dpi["mode"] == "presets":
            n = len(dpi["cmds"])
            lines += [f"min_stages = {n}", f"max_stages = {n}", "active_stage_settable = false"]
        else:
            lines += ["min_stages = 1", f"max_stages = {dpi['max_presets']}"]
    else:
        lines += ["dpi_min = 0", "dpi_max = 0", "dpi_step = 1", "max_stages = 0"]
    hz = sorted(int(k) for k in poll["choices"]) if poll and poll.get("value_type") == "choice" else []
    lines.append(f"polling = {hz}")
    lines.append("zones = " + json.dumps([z["zone"] for z in zs]))
    lines.append("effects = " + json.dumps(effects if zs else []))
    return "\n".join(lines) + "\n"


def main():
    src, devdir = Path(sys.argv[1]), Path(sys.argv[2])
    dump = subprocess.run([sys.executable, "-I", str(HERE / "rivalcfg_dump.py"), str(src)],
                          capture_output=True, text=True, check=True).stdout
    profiles = json.loads(dump)
    for f in devdir.glob("steelseries_*.toml"):
        if f.read_text(encoding="utf-8").startswith("# generato"):
            f.unlink()
    written = 0
    for name, p in profiles.items():
        text = generate(name, p)
        if text is None:
            print("saltato", name)
            continue
        (devdir / f"steelseries_{name}.toml").write_text(text, encoding="utf-8")
        written += 1
    print(f"Scritti {written} file SteelSeries ({sum(len(p['models']) for p in profiles.values())} modelli).")


if __name__ == "__main__":
    main()
