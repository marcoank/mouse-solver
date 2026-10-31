"""Aggiorna il database dei mouse dai progetti open source e rigenera SUPPORTED.md.

  python -I tools/update_devices.py            scarica OpenRazer, rivalcfg e G-Helper e rigenera tutto
  python -I tools/update_devices.py --offline  rigenera solo SUPPORTED.md dai file già presenti

I sorgenti scaricati vengono solo letti come testo dai generatori, mai eseguiti.
"""
import io
import subprocess
import sys
import tarfile
import tempfile
import tomllib
import urllib.request
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
DEVICES = ROOT / "devices"
SOURCES = {
    "openrazer": ("openrazer/openrazer", "gen_razer.py"),
    "rivalcfg": ("flozz/rivalcfg", "gen_steelseries.py"),
    "g-helper": ("seerge/g-helper", "gen_asus.py"),
}


def download(repo, dest):
    url = f"https://codeload.github.com/{repo}/tar.gz/HEAD"
    data = urllib.request.urlopen(url, timeout=60).read()
    with tarfile.open(fileobj=io.BytesIO(data)) as tar:
        top = tar.getmembers()[0].name.split("/")[0]
        tar.extractall(dest, filter="data")
    return Path(dest) / top


def supported_md():
    rows = []
    for f in sorted(DEVICES.glob("*.toml")):
        d = tomllib.loads(f.read_text(encoding="utf-8"))
        caps = d.get("caps", {})
        ids = [(d["vid"], d["pid"])] + [tuple(a) for a in d.get("aliases", [])]
        idtxt = "tutti" if d.get("any_pid") else ", ".join(f"{v:04x}:{p:04x}" for v, p in ids)
        stages = caps.get("max_stages", 0)
        dpi = f"{caps.get('dpi_min')}-{caps.get('dpi_max')}" if caps.get("dpi_max") else "-"
        if stages:
            dpi += f" ({stages} stadi)" if stages > 1 else ""
        poll = "/".join(str(x) for x in caps.get("polling", [])) or "-"
        zones = ", ".join(caps.get("zones", [])) or "-"
        rows.append((d["driver"], d["name"], d["status"], idtxt, dpi, poll, zones))
    order = {"verified": 0, "community": 1, "experimental": 2}
    rows.sort(key=lambda r: (order[r[2]], r[0], r[1].lower()))
    counts = {s: sum(1 for r in rows if r[2] == s) for s in order}
    lines = [
        "# Mouse supportati",
        "",
        "Generato da `tools/update_devices.py` dai file in `devices/`. Non modificare a mano.",
        "",
        f"**{len(rows)} file modello**: {counts['verified']} verificati su mouse reale, "
        f"{counts['community']} provati da altri, {counts['experimental']} sperimentali "
        "(protocollo preso da progetti open source, mai provato da noi).",
        "",
        "| Mouse | Stato | Driver | VID:PID | DPI | Polling (Hz) | Zone luci |",
        "|---|---|---|---|---|---|---|",
    ]
    lines += [f"| {n} | {s} | {drv} | {i} | {dpi} | {p} | {z} |" for drv, n, s, i, dpi, p, z in rows]
    (ROOT / "SUPPORTED.md").write_text("\n".join(lines) + "\n", encoding="utf-8")
    print(f"SUPPORTED.md: {len(rows)} modelli")


def main():
    if "--offline" not in sys.argv:
        with tempfile.TemporaryDirectory() as tmp:
            for name, (repo, gen) in SOURCES.items():
                print(f"Scarico {repo}...")
                src = download(repo, tmp)
                subprocess.run([sys.executable, "-I", str(ROOT / "tools" / gen), str(src), str(DEVICES)], check=True)
    supported_md()


if __name__ == "__main__":
    main()
