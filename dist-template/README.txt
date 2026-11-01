MOUSE SOLVER - TEST VERSION (BETA)
==================================

Sets your mouse DPI, polling rate and lighting without the vendor software. Settings are saved in
the mouse memory: open, apply, close.

IMPORTANT: this is a beta. Only a few models have been tested on a real mouse; the others use
protocols taken from open source projects. Before every change the app saves a copy of the current
settings and, when the mouse allows it, reads every write back to verify it.
Firmware, buttons and macros are never touched. Use at your own risk (see LICENSE).

HOW TO TRY IT
1. Extract the whole zip into a folder. Do not run it from inside the zip.
2. Close the vendor software (Synapse, Armoury Crate, G HUB, iCUE, SteelSeries GG...), also from
   the icon next to the clock.
3. Connect the mouse, preferably with the cable.
4. Start MouseSolver.exe. Windows may show "Windows protected your PC": the app is not signed.
   Click "More info", then "Run anyway".
5. Change ONE thing at a time and press "Apply". The message at the bottom says whether it worked
   (green) or not (red). Then unplug and replug the mouse and press "Reload": was it kept?

HELP US
At the bottom of the window open "How did it go? Help us with a report", answer and press
"Create report". A mouse-solver-report-*.json file appears on your Desktop: no serial numbers or
personal data. Attach it to a report with "Open the reports page".
Mouse not recognised? Run mouse-solver-kit.exe, choose 1 and attach the report_*.json files.

IF SOMETHING GOES WRONG
"Restore last backup" puts the previous settings back. In any case the mouse stays normal and you
can always set it up again with the vendor software.

Not affiliated with any mouse manufacturer: brand names are used only to identify compatible
models (see NOTICE).
