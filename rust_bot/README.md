# SebessegBot V5 (Institutional Scalping Engine)

Egy nagyteljesítményű, Rust/Tokio alapú algoritmikus kereskedő bot a Hyperliquid DEX-re. A V5-ös architektúra a "Single Source of Truth" elvre épül: a teljes stratégiai logika és kockázatkezelés egyetlen JSON fájlból vezérelhető anélkül, hogy a motor kódját újra kellene fordítani.

## 🚀 Fő Funkciók (V5 Architektúra)
* **Százalékos Skálázás (Dynamic Ladder):** A limitszintek nem fix dollárban, hanem az aktuális árfolyam százalékában vannak meghatározva, így a bot bármilyen árfolyamszinten konzisztens marad.
* **Smart Cleanup & Anti-Stale Ladder:** A Reconcile loop felismeri a beragadt (szellem) limiteket, és azonnal törli őket, amint a pozíció felveszi az irányt, elkerülve az akaratlan rávásárlásokat.
* **Dust Killer:** Beépített védelem a "por" pozíciók (pl. <$15 USD) ellen. A bot felismeri a kerekítési hibából beragadt mikropozíciókat, és kíméletlenül likvidálja őket (Market Close), hogy elkerülje a Taker fee vérzést.
* **Ghost Buster (State Lock):** Megakadályozza a tőzsdei API spamet (Rate Limit) azzal, hogy csak akkor küld új TP/SL védőhálót, ha a pozíció mérete ténylegesen megváltozott.

## 🧠 Szignál Motor (Konfluencia)
A `strategy_maker.json` fájlban több indikátor is bekapcsolható. Ha több indikátor aktív (pl. Z-Score ÉS RSI), a bot **csak akkor lép be, ha mindkét indikátor megerősíti ugyanazt az irányt (AND logika).**

* **Z-Score (Trigger):** A hirtelen volatilitás-tüskéket vadássza le az átlagtól való statisztikai eltérés alapján.
* **RSI (Context):** Tick-alapú, de okos mintavételezéssel (time-sampled) működő RSI, ami megakadályozza a mikroszekundumos zajból eredő fals jeleket.

## ⚙️ Vezérlőpult (`strategy_maker.json`)
A bot kizárólag ebből a fájlból dolgozik. Főbb paraméterek:
* `dust_limit_usd`: A pozíció minimális mérete. Ez alatt a bot bezárja a pozíciót, hogy ne fizessen felesleges jutalékot.
* `pos_clear_threshold`: A kerekítési hibák toleranciája (pl. 0.0001).
* `min_signal_interval_ms`: Két trade közötti kötelező szünet (Cooldown), hogy elkerüljük a wash tradinget.
* `ladder_levels.offset_from_mid_pct`: A limitek távolsága az aktuális középártól (százalékban).
