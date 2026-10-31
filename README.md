# mouse-cfg

Imposta DPI, polling rate e luci del mouse senza Synapse, Armoury Crate, G HUB, iCUE o SteelSeries GG.
Un exe portable: lo apri, scrivi le impostazioni nella memoria del mouse, lo chiudi. Niente servizi in background,
niente account, nessuna connessione di rete.

| Exe | A cosa serve |
|---|---|
| `MouseCfg.exe` | interfaccia grafica, quella da usare |
| `mouse-cfg.exe` | riga di comando (test e debug) |
| `mouse-cfg-kit.exe` | per i mouse non supportati: raccolta informazioni in sola lettura e cattura guidata del software ufficiale |

## Mouse supportati

Elenco completo in [SUPPORTED.md](SUPPORTED.md): **369 file modello, 421 coppie VID:PID**, più tutti i Logitech HID++ 2.0.
Lo stesso mouse può avere più PID (cavo, wireless, ricevitore, edizioni speciali).

| Marca | VID:PID | Fonte del protocollo | Cosa si può fare |
|---|---|---|---|
| Razer | 114 (Basilisk V3 **verificato**) | OpenRazer | DPI e stadi, polling (fino a 8000 Hz), luci per zona, luminosità; tutto riletto |
| SteelSeries | 76 | rivalcfg | DPI, polling, colori; **solo scrittura** (non si possono rileggere) |
| ASUS ROG / TUF | 61 | G-Helper | DPI e stadi, polling, luci; riletto |
| Corsair | 41 | ckb-next, OpenRGB | famiglia NXP (M65, Sabre, Scimitar, Harpoon, Glaive, Katar, Ironclaw...): DPI e stadi, polling, colore fisso per zona. Famiglia Bragi (M55, Katar Pro, Dark Core Pro, M75...): solo polling |
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
progetti open source, mai provato da noi. Oggi è verificato solo il Razer Basilisk V3: tutto il resto è `experimental`.
Ogni modello è un file in [devices/](devices/): per un mouse simile basta un nuovo TOML.

### Da sapere prima di usarli

- **Solo scrittura** (SteelSeries, HyperX Dart e Saga Pro, Glorious G2 e GP): il mouse non permette di rileggere,
  l'app mostra i valori scritti l'ultima volta da qui. Usa "Misura polling" per controllare.
- **Glorious Model O 2 / I 2 / D 2 / O 2 Mini**: il blocco di configurazione va riscritto intero, quindi lift-off,
  debounce e motion sync tornano ai valori di fabbrica e il mouse passa al profilo 1.
- **Glorious PRO e Model O/D Wireless, Pulsar famiglia S, Lamzu**: nessun comando di salvataggio è documentato;
  che le impostazioni restino dopo aver scollegato il mouse non è ancora dimostrato.
- **Corsair NXP**: dopo il cambio di polling il mouse si ricollega da solo (un paio di secondi). Il colore salvato si vede
  dopo averlo ricollegato. Sui modelli "a file" (Glaive, Scimitar Pro/Elite, M65 Elite, Ironclaw, Nightsword, Dark Core)
  il salvataggio dei DPI non è verificato.
- **Logitech**: si modifica solo il profilo onboard attivo, leggendo e riscrivendo i soli byte di DPI, polling e luci
  (nome e altre impostazioni di G HUB restano). Polling massimo salvabile: 1000 Hz.
- **SinoWealth**: i firmware non in tabella restano in sola lettura per sicurezza.

Per aggiornare dal codice di OpenRazer, rivalcfg e G-Helper (i generatori leggono i sorgenti come testo, senza eseguirli):

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

Con la variabile `MOUSECFG_DEBUG=1` la CLI stampa ogni pacchetto inviato e ricevuto.

## Sicurezza

- Ogni driver ha una **lista chiusa** di comandi ammessi (lettura e scrittura di DPI, polling, luci e il salvataggio
  documentato). Firmware, bootloader, reset di fabbrica, abbinamento dei ricevitori, tasti e macro vengono rifiutati
  prima di arrivare al mouse.
- Si mandano solo scritture documentate da una fonte; quelle solo "dedotte" sono lasciate fuori.
- Prima di ogni scrittura si salva un backup in `%APPDATA%\mouse-cfg\backups\`.
- Dopo ogni scrittura si rilegge e si confronta (tranne sui mouse "solo scrittura", che lo dichiarano).
- Il software del produttore aperto viene segnalato: può sovrascrivere le impostazioni.

## Misura del polling

`Misura polling` conta i pacchetti che il mouse manda davvero mentre lo muovi. Funziona con ogni mouse.
Da Windows 11 24H2 i programmi in background ricevono il mouse a circa 125 Hz, quindi durante la misura si apre una piccola
finestra in primo piano.

## Reverse engineering di un mouse nuovo

1. `mouse-cfg-kit.exe` → **1 Raccolta informazioni** (sola lettura, 1 minuto): VID/PID, interfacce HID, descrittori,
   feature report dei mouse sconosciuti e, per Razer/ASUS/Logitech, le risposte ai comandi di lettura noti.
   Produce `report_*.json`.
2. `mouse-cfg-kit.exe` → **2 Cattura guidata** (10 minuti, serve [USBPcap](https://desowin.org/usbpcap/) e il kit avviato
   come amministratore): registra solo il traffico del mouse scelto mentre cambi DPI, polling e luci nel software
   ufficiale, un passo alla volta. Alla fine controlla che nella cattura non ci sia nessun altro dispositivo.
   Produce una cartella `cattura_<vid>_<pid>_<ora>` con `capture.pcap` e `markers.json`.
3. `python -I tools/analyze_capture.py <cartella>`: separa i comandi per passo, scarta quelli ripetuti e propone quali byte
   codificano DPI, polling e colori (`analisi.md`).
4. Dal risultato si scrive un `devices/*.toml` `experimental`, si prova, si promuove.

## Compilare

```
cargo build --release
cargo test --workspace
python -I tools/test_analyze_capture.py
```

Riferimenti dei protocolli (letti e reimplementati, non copiati): [OpenRazer](https://github.com/openrazer/openrazer)
(GPL-2.0), [G-Helper](https://github.com/seerge/g-helper) (GPL-3.0), [rivalcfg](https://github.com/flozz/rivalcfg)
(WTFPL), [libratbag](https://github.com/libratbag/libratbag) (MIT), [ckb-next](https://github.com/ckb-next/ckb-next)
(GPL-2.0), [OpenRGB](https://gitlab.com/CalcProgrammer1/OpenRGB) (GPL-2.0) e diversi progetti indipendenti per Glorious,
Pulsar, Lamzu, Endgame Gear, Roccat e HyperX (citati nei commenti dei driver).
La licenza del progetto è ancora da decidere.
