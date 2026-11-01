# mouse solver

**[Italiano](README.it.md)** · English

Set your gaming mouse DPI, polling rate and lighting without Synapse, Armoury Crate, G HUB, iCUE or SteelSeries GG.
One portable exe: open it, write the settings into the mouse memory, close it. No background services, no account,
no network connection.

> **Beta.** 369 model files cover 421 USB IDs from 13 brands plus all HID++ 2.0 Logitech mice, but only a few have
> been tested on real hardware. Everything else uses protocols taken from open source projects. Please
> [report how it went](../../issues/new?template=test-report.yml): it is how models move from *experimental* to *tested*.

| Exe | What it is for |
|---|---|
| `MouseSolver.exe` | the app (Italian or English, following Windows) |
| `mouse-solver.exe` | command line (tests and debugging) |
| `mouse-solver-kit.exe` | for unsupported mice: read-only data collection and guided capture of the vendor software |

Download: [Releases](../../releases).

## Supported mice

Full list in [SUPPORTED.md](SUPPORTED.md). The same mouse can have several IDs (cable, wireless, receiver, special editions).

| Brand | USB IDs | Protocol source | What you can set |
|---|---|---|---|
| Razer | 114 (Basilisk V3 **verified**) | OpenRazer | DPI stages, polling (up to 8000 Hz), lighting per zone, brightness; all read back |
| SteelSeries | 76 | rivalcfg | DPI, polling, colours; **write-only** (cannot be read back) |
| ASUS ROG / TUF | 61 (tested by a user on a ROG mouse) | G-Helper | DPI stages, polling, lighting; read back |
| Corsair | 41 | ckb-next, OpenRGB | NXP family (M65, Sabre, Scimitar, Harpoon, Glaive, Katar, Ironclaw...): DPI, polling, static colour per zone. Bragi family (M55, Katar Pro, Dark Core Pro, M75...): polling only |
| Glorious | 33 + 3 SinoWealth | independent projects, libratbag | Model O 2 / I 2 / D 2 / O 2 Mini, D3/O3, O/D 2 PRO, Model O/D Wireless, Model O/D/O-/Eternal |
| Pulsar | 13 | independent projects | X2, X2H, X2A, Xlite, X3, 1K/8K dongles: DPI, active stage, polling, LED |
| Lamzu | 36 | independent projects | Atlantis/Thorn: DPI, polling. Aurora series: polling and active stage only |
| Endgame Gear | 9 | independent projects | XM2w/OP1w 4k, OP1 8k v2, WE series: DPI, polling |
| Roccat | 15 | libratbag, independent projects | Kone XTD/Pure/EMP, Kova, Kone AIMO, Kone Pro, Kone XP, Kone XP Air |
| HyperX | 8 | independent projects | Pulsefire Dart, Haste Wireless, Saga Pro |
| Cooler Master | 5 | OpenRGB | MM530/711/712/720/730: lighting only |
| Logitech G | all HID++ 2.0 | libratbag | DPI, polling, lighting in the onboard profile |
| SinoWealth | 7 | libratbag | Machenike, Marvo, G-Wolves, Genesis, T-Dagger... (identified by firmware) |
| BenQ Zowie | all "driverless" models | Zowie FAQ | set with the mouse buttons: the app explains how and measures the polling rate |

Status labels: `verified` = tested by us on a real mouse; `community` = tested by someone else; `experimental` =
from open source projects, not tested yet.

### Good to know

- **Write-only** (SteelSeries, HyperX Dart and Saga Pro, Glorious G2 and GP): the mouse cannot be read back, the app
  shows the values last written from here. Use "Measure polling" to check.
- **Glorious Model O 2 / I 2 / D 2 / O 2 Mini**: the whole configuration block is rewritten, so lift-off, debounce and
  motion sync go back to factory values and the mouse switches to profile 1.
- **Glorious PRO and Model O/D Wireless, Pulsar family S, Lamzu**: no save command is documented; whether the settings
  survive unplugging is not proven yet.
- **Corsair NXP**: after changing the polling rate the mouse reconnects by itself (a couple of seconds). The saved colour
  shows after replugging. On "file-based" models (Glaive, Scimitar Pro/Elite, M65 Elite, Ironclaw, Nightsword,
  Dark Core) saving DPI is not verified.
- **Logitech**: only the active onboard profile is changed, by reading it and rewriting just the DPI, polling and
  lighting bytes (the name and other G HUB settings stay). Highest polling that can be saved: 1000 Hz.
- **SinoWealth**: firmware versions not in the table stay read-only for safety.

## Safety

- Every driver has a **closed list** of allowed commands (read/write DPI, polling, lighting and the documented save step).
  Firmware, bootloader, factory reset, receiver pairing, buttons and macros are refused before reaching the mouse.
- Only writes documented by a source are sent; writes that are only "inferred" are left out.
- A backup is saved to `%APPDATA%\mouse-solver\backups\` before every write.
- Every write is read back and compared (except on write-only mice, which say so).
- Running vendor software is detected and reported: it may overwrite the settings.

## Command line

```
mouse-solver list
mouse-solver status
mouse-solver dpi 800,1600,3200 --active 2
mouse-solver polling 1000
mouse-solver rgb logo static ff0000
mouse-solver rgb all spectrum
mouse-solver brightness all 50
mouse-solver measure            # real polling rate: move the mouse in circles
mouse-solver backup
mouse-solver restore            # last backup
```

`MOUSE_SOLVER_LANG=it` or `=en` forces the language; `MOUSE_SOLVER_DEBUG=1` prints every packet sent and received.

## Help with a mouse we don't support

1. `mouse-solver-kit.exe` → **1 Collect information** (read-only, 1 minute): IDs, HID interfaces, descriptors, feature
   reports and, for known brands, the answers to read commands. Creates `report_*.json`.
2. `mouse-solver-kit.exe` → **2 Guided capture** (10 minutes, needs [USBPcap](https://desowin.org/usbpcap/) and the kit
   run as administrator): records only the traffic of the chosen mouse while you change DPI, polling and lighting in the
   vendor software, one step at a time, and checks at the end that no other device was recorded.
3. Open a [new mouse issue](../../issues/new?template=new-mouse.yml) and attach the files. We analyse captures with
   `tools/analyze_capture.py`, which proposes which bytes encode DPI, polling and colours.

## Building

```
cargo build --release
cargo test --workspace
python -I tools/test_analyze_capture.py
```

Pushing a `v*` tag builds the release zip with GitHub Actions (`.github/workflows/release.yml`).

## License

[Apache License 2.0](LICENSE). Not affiliated with any mouse manufacturer; brand names are used only to identify
compatible hardware (see [NOTICE](NOTICE)).

Protocols were read and reimplemented, not copied, from [OpenRazer](https://github.com/openrazer/openrazer),
[G-Helper](https://github.com/seerge/g-helper), [rivalcfg](https://github.com/flozz/rivalcfg),
[libratbag](https://github.com/libratbag/libratbag), [ckb-next](https://github.com/ckb-next/ckb-next),
[OpenRGB](https://gitlab.com/CalcProgrammer1/OpenRGB) and the independent projects cited in each driver.
