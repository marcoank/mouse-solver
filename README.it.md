# mouse solver

Italiano · **[English](README.md)**

Imposta DPI, polling rate e luci del mouse da gaming senza Synapse, Armoury Crate, G HUB, iCUE o SteelSeries GG.
Un exe portable: lo apri, scrivi le impostazioni nella memoria del mouse, lo chiudi. Niente servizi in background,
niente account, nessuna connessione di rete.

> **Beta.** 369 file modello coprono 421 identificativi USB di 13 marchi più tutti i Logitech HID++ 2.0, ma solo pochi
> sono stati provati su un mouse vero. Gli altri usano protocolli presi da progetti open source.
> [Raccontaci com'è andata](../../issues/new?template=test-report.yml): è così che un modello passa da *sperimentale* a *provato*.

| Exe | A cosa serve |
|---|---|
| `MouseSolver.exe` | il programma (in italiano o inglese, come Windows) |
| `mouse-solver.exe` | riga di comando (test e debug) |
| `mouse-solver-kit.exe` | per i mouse non supportati: raccolta informazioni in sola lettura e cattura guidata del software ufficiale |

Download: [Releases](../../releases).

## Mouse supportati

Elenco completo in [SUPPORTED.md](SUPPORTED.md). Lo stesso mouse può avere più identificativi (cavo, wireless, ricevitore, edizioni speciali).

| Marca | ID USB | Fonte del protocollo | Cosa si può fare |
|---|---|---|---|
| Razer | 114 (Basilisk V3 **verificato**) | OpenRazer | stadi DPI, polling (fino a 8000 Hz), luci per zona, luminosità; tutto riletto |
| SteelSeries | 76 | rivalcfg | DPI, polling, colori; **solo scrittura** (non si possono rileggere) |
| ASUS ROG / TUF | 61 (provato da un utente su un mouse ROG) | G-Helper | stadi DPI, polling, luci; riletto |
| Corsair | 41 | ckb-next, OpenRGB | famiglia NXP (M65, Sabre, Scimitar, Harpoon, Glaive, Katar, Ironclaw...): DPI, polling, colore fisso per zona. Famiglia Bragi (M55, Katar Pro, Dark Core Pro, M75...): solo polling |
| Glorious | 33 + 3 SinoWealth | progetti indipendenti, libratbag | Model O 2 / I 2 / D 2 / O 2 Mini, D3/O3, O/D 2 PRO, Model O/D Wireless, Model O/D/O-/Eternal |
| Pulsar | 13 | progetti indipendenti | X2, X2H, X2A, Xlite, X3, ricevitori 1K/8K: DPI, stadio attivo, polling, LED |
| Lamzu | 36 | progetti indipendenti | Atlantis/Thorn: DPI, polling. Serie Aurora: solo polling e stadio attivo |
| Endgame Gear | 9 | progetti indipendenti | XM2w/OP1w 4k, OP1 8k v2, serie WE: DPI, polling |
| Roccat | 15 | libratbag, progetti indipendenti | Kone XTD/Pure/EMP, Kova, Kone AIMO, Kone Pro, Kone XP, Kone XP Air |
| HyperX | 8 | progetti indipendenti | Pulsefire Dart, Haste Wireless, Saga Pro |
| Cooler Master | 5 | OpenRGB | MM530/711/712/720/730: solo luci |
| Logitech G | tutti gli HID++ 2.0 | libratbag | DPI, polling, luci nel profilo onboard |
| SinoWealth | 7 | libratbag | Machenike, Marvo, G-Wolves, Genesis, T-Dagger... (riconosciuti dal firmware) |
| BenQ Zowie | tutti i "senza driver" | FAQ Zowie | si configurano dai tasti: l'app spiega come e misura il polling |

Gli stati: `verified` = provato da noi su mouse reale; `community` = provato da qualcun altro; `experimental` = preso da
progetti open source, non ancora provato.

### Da sapere

- **Solo scrittura** (SteelSeries, HyperX Dart e Saga Pro, Glorious G2 e GP): il mouse non permette di rileggere,
  l'app mostra i valori scritti l'ultima volta da qui. Usa "Misura" nel riquadro Polling rate per controllare.
- **Glorious Model O 2 / I 2 / D 2 / O 2 Mini**: il blocco di configurazione va riscritto intero, quindi lift-off,
  debounce e motion sync tornano ai valori di fabbrica e il mouse passa al profilo 1.
- **Glorious PRO e Model O/D Wireless, Pulsar famiglia S, Lamzu**: nessun comando di salvataggio è documentato;
  che le impostazioni restino dopo aver scollegato il mouse non è ancora dimostrato.
- **Corsair NXP**: dopo il cambio di polling il mouse si ricollega da solo (un paio di secondi). Il colore salvato si vede
  dopo averlo ricollegato. Sui modelli "a file" (Glaive, Scimitar Pro/Elite, M65 Elite, Ironclaw, Nightsword, Dark Core)
  il salvataggio dei DPI non è verificato.
- **Logitech**: si modifica solo il profilo onboard attivo, leggendolo e riscrivendo solo i byte di DPI, polling e luci
  (nome e altre impostazioni di G HUB restano). Polling massimo salvabile: 1000 Hz.
- **SinoWealth**: i firmware non in tabella restano in sola lettura per sicurezza.

## Sicurezza

- Ogni driver ha una **lista chiusa** di comandi ammessi (lettura e scrittura di DPI, polling, luci e il salvataggio
  documentato). Firmware, bootloader, reset di fabbrica, abbinamento dei ricevitori, tasti e macro vengono rifiutati
  prima di arrivare al mouse.
- Si mandano solo scritture documentate da una fonte; quelle solo "dedotte" sono lasciate fuori.
- Prima di ogni scrittura si salva un backup in `%APPDATA%\mouse-solver\backups\`.
- Dopo ogni scrittura si rilegge e si confronta (tranne sui mouse "solo scrittura", che lo dichiarano).
- Il software del produttore aperto viene segnalato: può sovrascrivere le impostazioni.

## Riga di comando

```
mouse-solver list
mouse-solver status
mouse-solver dpi 800,1600,3200 --active 2
mouse-solver polling 1000
mouse-solver rgb logo static ff0000
mouse-solver rgb all spectrum
mouse-solver brightness all 50
mouse-solver measure            # polling reale: muovi il mouse in cerchio
mouse-solver backup
mouse-solver restore            # ultimo backup
```

`MOUSE_SOLVER_LANG=it` o `=en` forza la lingua; `MOUSE_SOLVER_DEBUG=1` stampa ogni pacchetto inviato e ricevuto.

## Aiutaci con un mouse non supportato

1. `mouse-solver-kit.exe` → **1 Raccolta informazioni** (sola lettura, 1 minuto): identificativi, interfacce HID,
   descrittori, feature report e, per i marchi noti, le risposte ai comandi di lettura. Produce `report_*.json`.
2. `mouse-solver-kit.exe` → **2 Cattura guidata** (10 minuti, serve [USBPcap](https://desowin.org/usbpcap/) e il kit
   avviato come amministratore): registra solo il traffico del mouse scelto mentre cambi DPI, polling e luci nel
   software ufficiale, un passo alla volta, e alla fine controlla che non ci sia nessun altro dispositivo.
3. Apri una [segnalazione "mouse nuovo"](../../issues/new?template=new-mouse.yml) e allega i file. Le catture le
   analizziamo con `tools/analyze_capture.py`, che propone quali byte codificano DPI, polling e colori.

## Compilare

```
cargo build --release
cargo test --workspace
python -I tools/test_analyze_capture.py
```

Un tag `v*` fa compilare a GitHub Actions lo zip della release (`.github/workflows/release.yml`).

## Licenza

[Apache License 2.0](LICENSE). Non affiliato con nessun produttore di mouse: i nomi dei marchi servono solo a indicare
l'hardware compatibile (vedi [NOTICE](NOTICE)).

I protocolli sono stati letti e reimplementati, non copiati, da [OpenRazer](https://github.com/openrazer/openrazer),
[G-Helper](https://github.com/seerge/g-helper), [rivalcfg](https://github.com/flozz/rivalcfg),
[libratbag](https://github.com/libratbag/libratbag), [ckb-next](https://github.com/ckb-next/ckb-next),
[OpenRGB](https://gitlab.com/CalcProgrammer1/OpenRGB) e dai progetti indipendenti citati in ogni driver.
