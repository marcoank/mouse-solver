# mouse-cfg

Imposta DPI, polling rate e luci del mouse senza Synapse, Armoury Crate o SteelSeries GG.
Un exe portable: lo apri, scrivi le impostazioni nella memoria del mouse, lo chiudi. Niente servizi in background,
niente account, nessuna connessione di rete.

| Exe | A cosa serve |
|---|---|
| `MouseCfg.exe` | interfaccia grafica, quella da usare |
| `mouse-cfg.exe` | riga di comando (test e debug) |
| `mouse-cfg-kit.exe` | raccoglie informazioni in sola lettura su un mouse non supportato e crea `report_*.json` da mandarci |

## Mouse supportati

Elenco completo in [SUPPORTED.md](SUPPORTED.md): **210 file modello, 254 coppie VID:PID**, più tutti i Logitech HID++ 2.0.

| Marca | Modelli | Da dove viene il protocollo | Cosa si può fare |
|---|---|---|---|
| Razer | 114 (Basilisk V3 **verificato**) | OpenRazer | DPI e stadi, polling (anche HyperPolling fino a 8000 Hz), luci per zona, luminosità, tutto riletto per verifica |
| ASUS ROG / TUF | 61 file, 69 modelli | G-Helper | DPI e stadi, polling, luci; le funzioni con pacchetti diversi dal solito vengono disattivate, non indovinate |
| SteelSeries | 33 file, 76 modelli | rivalcfg | DPI, polling, colori; **solo scrittura** (questi mouse non permettono di rileggere) |
| Logitech G | tutti quelli HID++ 2.0 | libratbag | **per ora solo lettura** di DPI e polling: il salvataggio nei profili del mouse arriverà quando avremo un Logitech su cui provarlo |
| BenQ Zowie | tutti i "senza driver" | FAQ Zowie | si configurano dai tasti: l'app spiega come e misura il polling |

Gli stati: `verified` = provato da noi su mouse reale; `community` = provato da qualcun altro; `experimental` = preso da
progetti open source, mai provato da noi. Ogni modello è un file in [devices/](devices/): per un mouse simile basta un nuovo TOML.

Per aggiornare il database quando i progetti open source aggiungono mouse:

```
python -I tools/update_devices.py
```

## Riga di comando

```
mouse-cfg list
mouse-cfg status
mouse-cfg dpi 800,1600,3200 --active 2
mouse-cfg polling 1000
mouse-cfg rgb logo static ff0000
mouse-cfg rgb all spectrum
mouse-cfg brightness all 50
mouse-cfg measure            # polling reale: muovi il mouse in cerchio
mouse-cfg backup
mouse-cfg restore            # ultimo backup
```

## Sicurezza

- Ogni driver ha una **lista chiusa** di comandi ammessi (lettura e scrittura di DPI, polling, luci). Firmware, bootloader,
  tasti e macro vengono rifiutati prima di arrivare al mouse.
- Prima di ogni scrittura si salva un backup in `%APPDATA%\mouse-cfg\backups\`.
- Dopo ogni scrittura si rilegge e si confronta (tranne sui mouse "solo scrittura", che lo dichiarano).
- Il software del produttore aperto viene segnalato: può sovrascrivere le impostazioni.

## Misura del polling

`Misura polling` conta i pacchetti che il mouse manda davvero mentre lo muovi. Funziona con ogni mouse.
Da Windows 11 24H2 i programmi in background ricevono il mouse a circa 125 Hz, quindi durante la misura si apre una piccola
finestra in primo piano.

## Reverse engineering di un mouse nuovo

1. `mouse-cfg-kit.exe` (sola lettura): VID/PID, interfacce HID, descrittori e, per Razer/ASUS sconosciuti, le risposte ai
   comandi di lettura noti.
2. Per protocolli sconosciuti: cattura del traffico del software ufficiale con USBPcap (Fase 6, Modalità B: da fare).
3. Dal report si scrive un `devices/*.toml` `experimental`, si prova, si promuove.

`tools/razer_probe.py` è il probe Python originale, di sola lettura. Con la variabile `MOUSECFG_DEBUG=1` la CLI stampa ogni
pacchetto inviato e ricevuto (Razer e SteelSeries).

## Compilare

```
cargo build --release
```

Riferimenti dei protocolli (letti, non copiati): [OpenRazer](https://github.com/openrazer/openrazer) (GPL-2.0),
[G-Helper](https://github.com/seerge/g-helper) (GPL-3.0), [rivalcfg](https://github.com/flozz/rivalcfg) (WTFPL),
[libratbag](https://github.com/libratbag/libratbag) (MIT). I generatori in `tools/` leggono quei sorgenti come testo, senza eseguirli.
La licenza del progetto è ancora da decidere.
