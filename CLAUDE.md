# Heatingrod Controller (Rust v3)

Dieses Repo enthält den **Rust-Controller v3** (ESPHome Native API Server + Regelung).
Repo-Split 2026-09-05: Das alte `heatingrod`-Repo läuft mit **Python v2** aus und
behält dort die v2-Quellen, PyScript-Scheduler-Quellen und Legacy-Dateien. Die
Hardware-/Anlagen-Doku (dieses Dokument, `docs/`, `plan/`) liegt hier im neuen Repo.

## Überblick
PV-Überschuss-gesteuerter Heizstab (max 7.5kW) in einem 800L Pufferspeicher (PSR/PSRR 800).
Brauch- und Nutzwasser, im Sommer kein Heizen. Analoge Temperatursperre ~67°C (graduell, kein Hartabschalter).
Prädiktiver Scheduler (HA PyScript) steuert Ölbrenner vorausschauend basierend auf Solarprognose und Wärmebedarf.

## Hardware-Anlage

### Kessel & Brenner
- **Kessel:** Götz Pro Condens PC1 15 (Öl-Vollbrennwert, 8-18kW, Baujahr 2016)
  - ProTwin-Gegenstrom-Wärmetauscher, Abgas <50°C, ganzjährige Kondensation
  - Rücklauftemperatur >60°C für Korrosionsschutz Stahl-WT
- **Steuerung:** Götz ProControl 3 (SW V1.53, Service-PW: 1503)
  - Proprietär, kein Standard-Bus (kein BSB/LPB/Modbus)
  - NTC-Sensoren: Abgas, Außen, Boiler, Rücklauf, Vorlauf HK1/HK2, Zirkulation, Kessel, Puffer
  - **Analog 0-10V Eingang** (Par 61): Sollwert HK1 extern (0-10V = 0-100°C) — potentiell nutzbar
  - **Kaskadeneingang** (230V): Kessel sperren/freigeben von extern
  - **SD-Karte**: Datenlogging (Excel, 2-60s Intervall) + Konfigurations-Backup
  - **FlashStick**: Firmware-Update
  - Herstellerebene: passwortgeschützt (Firmware-Reverse-Engineering TODO)
- **Brenner:** ELCO Vectron Blue VB 1.20 (11-18kW, einstufig, Blaubrenner)
  - Digitaler Verbrennungsregler, 207W Eigenverbrauch, Düse 0.40 gal/h

### Speicher
- **Pufferspeicher:** Austria Email PSR/PSRR 800 (800L)
  - Höhe 1700mm, Ø 790mm, Warmhalteverlust 108W (~2.6 kWh/Tag)
  - Max 4 bar Betriebsdruck, Sicherheitstemperaturbegrenzer 110°C
  - Einschraubheizkörper 1½" Muffe auf ~26cm Höhe (Zusatzheizung, nicht Dauerheizung lt. Hersteller)
  - SOREL-Anschluss auf ~50cm Höhe (Warmwasser-Entnahme)
  - Register: oben 1.8m²/11L, unten 2.4m²/15L (Ölbrenner-Kreislauf)
  - Nutzbare Energie: ~23 kWh bei 40→65°C

### Heizstab & Leistungsregler
- **Heizstab:** 7.5kW, 3-phasig, Einschraubheizkörper
  - Analoge Temperatursperre ~67°C (graduell, Leistung sinkt ab ~65°C, exakte Kennlinie noch nicht vermessen)
- **Thyristorsteller:** Chiemtronic T-Drive 3Ph compact 12A
  - Schwingungspaketsteuerung (Impulsgruppenbetrieb), Softstart
  - Ansteuerung: 0-10V (Klemmen 3/4) ← DAC GP8403 Channel 0
  - 3x400V, 3x12A (~8.3kVA max), Melderelais bei 100%
  - Reaktionszeit: ~50ms (Paketfenster, 50Hz Halbwellen)

### Elektronik & Sensorik
- **Controller:** Raspberry Pi (`root@heizung`, Debian Trixie, aarch64)
- **DAC:** DFRobot GP8403 (I2C 0x5f), 0-10V Ausgang → Thyristorsteller, direkt via i2cdev
- **Stromzähler Netz:** EasyMeter Q3A (ISK) → Tasmota → MQTT `tele/tasmota_1E92B2/SENSOR` (historisch; v3 liest direkt via Powerlogger-ESP)
- **Stromzähler Heizstab:** B+G E-Tech DS100-00B, Modbus RTU Serial (FTDI USB, 115200 Baud)
- **Warmwasser-Steuerung:** SOREL MTDCv5, CAN-Bus via USBtin (slcan0)
- **Temperatursensoren:** 1-Wire DS18B20 am DS1490F-USB-Adapter, Kernel-w1 (ds2490/w1_therm)

### Infrastruktur
- **Home Assistant:** http://homeassistant.kalnet.hooya.de:8123
- **Grafana:** InfluxDB-basierte Dashboards (grafana/: heizung.json, sorel.json, ds100.json)

## Sicherheitsregeln (KRITISCH)
1. DAC MUSS bei Programmende/Fehler/Disconnect auf 0V gesetzt werden
2. Heizstab darf NUR bei PV-Überschuss laufen
3. Bei fehlenden/veralteten Messdaten (>30s): sofort abschalten
4. Sensor-Fault: DS100-Power null während Heizen → sofort abschalten. Tank-Temp null → nur loggen, weiter heizen (analoge Thermalsperre schützt)
5. Bei Fehlern: Notification über HA `notify.signal_haus` senden — **derzeit auf
   keiner Seite implementiert**: Python v2 und Rust v3 loggen nur noch
   (Operator-Entscheidung 04.09.2026: vorerst belassen, Nachrüstung via
   REST-Kanal oder HA-Automation auf ESPHome-Logs möglich)

## Verfügbare Sensoren in Home Assistant

### Netz / PV (Wechselrichter mit Batterie-Support)
| Entity | Beschreibung | Einheit |
|--------|-------------|---------|
| `sensor.powerlogger_aktueller_verbrauch` | Aktueller Netzbezug (EasyMeter Q3A) | W |
| `sensor.powerlogger_einspeisung` | Gesamt-Einspeisung (Zähler) | kWh |
| `sensor.powerlogger_verbrauch` | Gesamt-Verbrauch (Zähler) | kWh |
| `sensor.import_power` | Netzbezug (Wechselrichter-Sicht) | W |
| `sensor.export_power` | Einspeisung (Wechselrichter-Sicht) | W |
| `sensor.export_power_raw` | Einspeisung signed (negativ=Bezug) | W |
| `sensor.load_power` | Hausverbrauch gesamt | W |
| `sensor.total_dc_power` | PV DC-Leistung gesamt | W |
| `sensor.mppt1_power` / `sensor.mppt2_power` | PV MPPT-Tracker einzeln | W |
| `sensor.daily_pv_generation` | PV-Erzeugung heute | kWh |
| `sensor.total_pv_generation` | PV-Erzeugung gesamt | kWh |

### Heizstab (via DS100 ESPHome)
| Entity | Beschreibung | Einheit |
|--------|-------------|---------|
| `sensor.ds100_power` | Aktuelle Leistungsaufnahme (100ms Polling) | W |
| `sensor.ds100_energy_total` | Gesamtenergie | kWh |
| `sensor.ds100_voltage_l1/l2/l3` | Spannung pro Phase | V |
| `sensor.ds100_current_l1/l2/l3` | Strom pro Phase | A |
| `sensor.ds100_frequency` | Netzfrequenz | Hz |
| `sensor.ds100_power_factor` | Leistungsfaktor | - |
| `sensor.heizstab_temperatur` | Temperatur am Heizstab (1-Wire via HA) | °C |

### Speicher-Temperaturen
| Entity | Beschreibung | Aktuell |
|--------|-------------|---------|
| `sensor.speicher_oben_temperatur` | Speicher oben | ~73°C |
| `sensor.speicher_rucklauf_temperatur` | Speicher Rücklauf | ~59°C |
| `sensor.28_ff7296711605_temperatur` | Speicher Vorlauf | ~64°C |
| `sensor.speicher_heizung_vorlauf_temperatur` | Heizung Vorlauf | ~65°C |
| `sensor.speicher_heizung_rucklauf_temperatur` | Heizung Rücklauf | ~39°C |
| `sensor.keller_speicher_temperatur` | Keller/Speicherraum | ~20°C |
| `sensor.heizstab_temperatur` | Am Heizstab | ~45°C |

### Brenner / Heizung
| Entity | Beschreibung | Einheit |
|--------|-------------|---------|
| `sensor.schaltaktor_heizung_brenner_leistung` | Brenner Leistung | W |
| `sensor.schaltaktor_heizung_nutzwasser_leistung` | Nutzwasser-Pumpe | W |
| `sensor.brenner_verbrauch` | Brenner Ölverbrauch | L/d |
| `sensor.heizung_warmwasserverbrauch_tag` | Warmwasser Tagesverbrauch | kWh |

## Workspace-Struktur

```
crates/esphome-api-server/   Protokoll-Schicht (Spiegel von aioesphomeserver, Python)
crates/heatingrod/           App: Controller, Kalibrierung, Hardware, Clients
tools/dac-safe/              Failsafe-Binary für ExecStopPost (CH0=0V, CH1=6V)
tests-py/                    aioesphomeapi-Cross-Check (golden_diff.py)
vendor/esphome-native-api/   Gepatched (plan/CRATE-PATCHES.md, 4 Patches)
plan/                        CRATE-PATCHES, CUTOVER, HA-TEST-ADD, Phase-0-Notizen
config/                      test-device.yaml (committbar); production/shadow gitignored
systemd/heatingrod-controller-v3.service
docs/                        ProControl-3-Handbuch, Thyristor, Pufferspeicher-PDFs
grafana/                     heizung.json, sorel.json, ds100.json
```

### Sicherheitsmechanismen
1. **Signal-Handler**: SIGTERM/SIGINT/SIGHUP → DAC auf 0V + graceful shutdown
2. **HA-Disconnect**: Sofort DAC auf 0V
3. **Watchdog** (alle 10s): Prüft Sensor-Freshness, DAC auf 0V wenn Daten stale
4. **Tank-Temp-Limit**: Speicher oben > 80°C → Heizstab aus
5. **Tank-Temp-Sensor-Fault**: Sensor null → einmalige Warnung, Heizen läuft weiter (analoge Thermalsperre ~67°C schützt)
6. **DS100-Power-Sensor-Fault**: Leistungssensor null während Heizen → sofort aus
7. **Interne Temp-Sperre**: `power_smooth` (EWMA) <50% der erwarteten Leistung (aus Kalibrierung) bei >1V DAC für >60s → analoge Temperatursperre aktiv (~67°C), DAC auf 0V, Wiederaufnahme erst bei <60°C am Heizstab. Feedback-Korrektur und Kalibrierungs-Updates werden bei erkanntem Limiting pausiert, um Kennlinienvergiftung zu verhindern.
8. **Luft-Erkennung**: Warmwasser-Temperatureinbruch >10°C in 5s bei laufender Pumpe → Notification (CAN-Pfad)
9. **Manueller Schalter**: `input_boolean.heizstab_schalter` in HA → wenn off, kein Heizen
10. **ExecStopPost**: Systemd ruft `tools/dac-safe` (CH0=0V/CH1=6V direkt per I2C) falls Prozess crasht

### Echtzeit-Feedback & Regeldelay
- **100ms Power-Polling**: DS100 liefert Wirkleistung alle 100ms via `_on_ds100_power` Callback
- **Thyristor Burst-Pattern**: Chiemtronic T-Drive 3Ph feuert in ~5-7s ON/OFF-Paketen (Schwingungspaketsteuerung). DS100 sieht alternierend ~0W und Vollleistung, nicht den Mittelwert.
- **EWMA-Glättung**: α=0.033 (τ≈3s) glättet das Burst-Muster zu verlässlicher Durchschnittsleistung (`power_smooth`)
- **Timer-basiertes Settle**: Nach jedem DAC-Wechsel 7s warten (≥1 voller Burst-Zyklus) bevor Korrekturen erlaubt, verhindert Regeloszillation
- **Automatische Delay-Messung**: DAC-Change → DS100-Wertänderung (>50W Delta) = Feedback-Delay
- **STATUS-Log**: `delay=Xms` zeigt gemessenen Delay (erst bei aktiver Heizlast)

## 1-Wire (Kernel-w1)

Seit 2026-09-04 (Testlauf erfolgreich, libowcapi-Support danach entfernt —
Python v2 nutzt weiterhin libowcapi):
- **Module**: `wire`, `ds2490`, `w1_therm` — geladen via ExecStartPre des v3-Units
- **Sysfs**: `/sys/bus/w1/devices/28-*/w1_slave`; jeder `open()` = frische Konvertierung (~750ms), blockierend im dedizierten 1-Wire-Thread
- **CRC-Retries**: bis zu 3 sofortige Re-Reads bei CRC NO (lange Bus-Leitungen)
- **Adress-Kanonisierung**: Kernel-Dirnamen sind **LSB-first** (`28-0316720f11ff`) und werden aufs Config-Format (`28.FF110F721603`, MSB) gespiegelt → HA-Entity-Identität unverändert
- **Semantik**: Python-Parität (Backoff nach 5 Fehlern, 85°C-Power-On-Filter, Range-Check −55…125°C, Push-on-Change ≥0,1°C, per-Zyklus-Freshness fürs Controller-Snapshot)
- **Auto-Discovery**: unbekannte Sensoren werden als `1w_28_xxxx` in ESPHome auto-registriert
- **Rollback auf Python v2**: vorher `rmmod w1_therm ds2490 wire` (sonst hält der Kernel-Treiber das USB-Device für libowcapi besetzt); `libowcapi.so` bleibt auf dem Pi installiert

## ESPHome Native API (Server, Port 6053)

- **71 Entities, 5 Sub-Devices**: Controller(0), DS100(1), Thyristor(2), SOREL(3), 1-Wire(4)
- **Entity-Keys**: MD5(object_id) erste 8 Hex als u32 (HA-Identität!), nicht FNV-1
- **MAC UPPERCASE** (`B8:27:EB:96:AE:00`): sonst bricht die HA-Legacy-Migration
  (Vorfall 18.08.2026, damals Python; v3 übernimmt die MAC exakt → kein
  Entity-Registry-Churn)
- **Noise-Verschlüsselung** konfiguriert (`api.encryption.key` in der Production-Config,
  gitignored); der Server lehnt Plaintext ab, sobald ein PSK gesetzt ist
- **Entity-Identität hängt am Namen** (HA ≥ 2026.8): Umbenennen von `name:` legt
  eine neue Entity an — `object_id` nicht mehr ändern
- **Server antwortet auf PingRequest** (HA-Parität zu echten ESPs)

## Kalibrierung (Hybrid)

- **Erststart**: Schnellkalibrierung (6 Stufen à 30s) bei >2kW Überschuss
- **Messwert**: `power_smooth` (EWMA) statt DS100-Momentanwert (Burst-Artefakte)
- **Totband-Schutz**: Sweep-Abbruch wenn `max_voltage < calibration.min_sweep_voltage`
  (4,0V ≈ 3kW); 0W oberhalb 2,0V wird NICHT gelernt (Limiter/Fehler, keine Kennlinieneigenschaft)
- **Cutoff-Erkennung**: nur ab ≥3V (vorher 1V → false positives)
- **Lernpaar**: `update(dac.current_voltage, power_smooth)` **vor** `set_voltage()` —
  der Korrekturpfad wird erst nach dem Settle-Timer erreicht, `power_smooth` gehört
  zur bisherigen Spannung (Vorfall: verkehrte Reihenfolge bog die Kennlinie nach unten)
- **Monotonie-Projektion**: Pool-Adjacent-Violators (gewichtet mit `count`) projiziert
  auf nicht fallende Kurve; beide Lookups arbeiten nur darauf; Extrapolation über
  Basislinie `EXTRAPOLATION_BASELINE_V` (0,5V) unter dem obersten Punkt
- **Laufend**: jeder `power_smooth`-Wert nach Settle-Phase verfeinert (EWMA, α=0.3)
- **Persistent**: `calibration.json` (gitignored, neben der Production-Config auf dem Pi)
- **Auto-Neukalibrierung**: >7 Tage alt oder >30% Abweichung (`calibration.deviation_threshold`)
- **Kalibrierungs-Abort**: `grid > power_limit_stop` (echter Grid-Import, kein Doppelzählen von rod_power)
- **Kennlinienvergiftung**: bei Thermal-Limiter-Erkennung pausieren Kalibrierungs-Updates

## ProControl CH1 & Kaskadeneingang

- **CH1-Persistenz**: `state.json` (neben `calibration.json`) speichert DAC CH1
  (0-10V = 0-100°C HK1-Sollwert); Restore beim Start, Push an HA nach Bridge-Start
- **Kaskade (Brennersperre)**: ESP8266-Relais steuert KN/KL (230V) —
  `esp-heizung-ksk.kalnet.hooya.de:6053`, Noise-Key in Config.
  CH1 < 0,05V → Switch ON (gesperrt); CH1 > 0 → OFF (frei).
  Consistency-Check alle 10s im Watchdog. CH1=6V ist der Shutdown-Safe-Wert — **nie 0V**
  (0V nur via Climate-OFF), weil 0V den Brenner sperren würde.

## Logging

- **journald nativ**: eigener Layer in `crates/heatingrod/src/logging.rs` (Datagramm-
  Protokoll, kein libsystemd-Link — Voraussetzung für statisches musl-Binary).
  Felder: `MESSAGE`, `PRIORITY` (DEBUG=7/INFO=6/WARNING=4/ERROR=3), `SYSLOG_IDENTIFIER=heatingrod`,
  `TARGET` (Modulpfad), `CODE_FILE/LINE`. Ohne journald-Socket (OpenWrt): Stderr-Fallback.
- **STATUS-Log** alle 60s bedingungslos (Operator-Entscheidung):
  `STATUS | state | grid=XW(pl|ha) pv=XW rod=XW/X°C tank=X°C dac=XV/X% delay=Xms | hapi= pl=ok,msgs=N,intv=Xs ds100=V,Hz,mbus=N,Ne cbus=ok,pkts=N,errs=N 1wir=N/Ne,cyc=N,Nms idac= pc1=X°C ksk=`
- **Watchdog** arbeitet bei `subscribers==0` weiter (Direktverbindungen liefern die
  Daten) — nur Warnung, kein Emergency (anders als Python v2, dessen Sensoren durch HA kamen)
- **Clients** (kaskade/powerlogger): alle 15s PingRequest, Pong-Deadline 10s →
  Forced Reconnect; RTT-Log
- **LogForwardLayer**: Events des `heatingrod`-Modulbaums an ESPHome-Log-Subscriber
  (Python `_ESPHomeLogHandler`-Parität); Third-Party-Crates werden NICHT weitergeleitet

```bash
journalctl -u heatingrod-controller-v3            # alles von v3
journalctl -u heatingrod-controller-v3 -p warning # nur Warnungen
```

## Build & Deploy

```bash
# Dev-Maschine: Rust stable (rust-toolchain.toml), zig 0.14.1 in ~/.local/opt
cargo test --workspace                     # 175 Tests

# Release für den Pi (aarch64, statisch musl):
cargo zigbuild --release --target aarch64-unknown-linux-musl -p heatingrod
rsync -a target/aarch64-unknown-linux-musl/release/heatingrod \
      root@heizung:/root/heatingrod-rust/heatingrod.new
ssh root@heizung "systemctl stop heatingrod-controller-v3 && \
  mv /root/heatingrod-rust/heatingrod.new /root/heatingrod-rust/heatingrod && \
  systemctl start heatingrod-controller-v3"
```

- Config: `/root/heatingrod-rust/production.yaml` (gitignored, enthält PSKs)
- `calibration.json`/`state.json` liegen neben der Config (gitignored)
- `ExecStartPre` lädt die w1-Kernel-Module; `ExecStopPost` ruft `tools/dac-safe`
- Rollback-Binary: `/root/heatingrod-rust/heatingrod.gnu` (GNU-Build, Stand 04.09.2026)

## Prädiktiver Speicher-Scheduler (HA PyScript)

Quellen liegen im alten `heatingrod`-Repo (`pyscript/`), läuft auf HA.
Steuert Ölbrenner vorausschauend über `climate.procontrol_3_heizkessel_hk1`.

### Steuerungspfad
```
PyScript: climate.set_hvac_mode(..., "off")
  → Daemon: _on_climate_command() → DAC CH1=0V → Kaskade ON → Brenner gesperrt
PyScript: climate.set_temperature(..., temperature=60)
  → Daemon: DAC CH1=6V → Kaskade OFF → Brenner frei
```

### State Machine
- **DISABLED / NORMAL / SOLAR_PREPARE / SOLAR_HEATING / LEGIONELLA / EMERGENCY**
- **NORMAL → SOLAR_PREPARE**: 18–22h, solar_tomorrow ≥ 8 kWh, morning_temp ≥ 45°C
- **NORMAL → SOLAR_HEATING**: 8–16h, PV > 1500W (`SOLAR_BOOST_MIN_W`), tank_rod < 60°C
- **SOLAR_PREPARE → SOLAR_HEATING**: 6–16h, PV > 500W, tank_rod < 60°C
- **SOLAR_HEATING → NORMAL**: PV < 800W intraday, oder PV < 100W nach 16h, oder tank_rod > 63°C
- **→ EMERGENCY**: tank_top < 42°C; EMERGENCY → NORMAL bei 50°C
- **Reload-Resilienz**: Zustand aus `input_select.heizung_scheduler_mode` wiederhergestellt

### Datenquellen
- **HA State**: Speichertemperaturen, PV-Leistung, Brenner-Zustand
- **CCU JSON-RPC**: 13 TRV-Ventilstellungen via `Interface.getParamset`
  (OpenCCU `ccu2.kalnet.hooya.de`, User: heatingrod)
- **forecast.solar**: PV-Prognose (max 2 Calls/Tag), Cache in HA-Helper-Entities

## ProControl 3 — Hardware & Integration (Forschung)

- **MCU:** Atmel ATmega128A-AU (8-bit AVR, TQFP-64, 128KB Flash)
  - ISP über 2x3-Pin-Header (Pinout empirisch verifiziert, siehe altes Repo/Notizen)
- **Buchsenleisten:** NTC-Fühler (AGF, AF, BF, KRF, HK1, ZF, KF, POF, FB2, PUF, HK2, FB1),
  0-10V-Eingang (Par 61), Digitaleingang 0-5VDC (Par 67), 230V-Leiste (Kaskade, Pumpen, STB, Brenner)
- **NTC-Kennlinie:** 223Ω @ 115°C bis 48563Ω @ -20°C (Service Manual S.15, `docs/Pro Condens 3/`)
- **Service-Ebene Par 1-74** dokumentiert (Service Manual S.5-12); Par 69-72: Sichern/Wiederherstellen
- **Integrationswege:** 0-10V HK1-Sollwert (DAC CH1), Kaskadeneingang (Relais), SD-Karten-Logging
- **Herstellerebene:** separates Passwort (nicht 1503), Reverse-Engineering TODO

## Verhältnis zum alten Repo

- **Python v2** (Produktions-Historie, 1:1-Vorlage dieses Ports) + `pyscript/` + Legacy-Dateien:
  altes lokales `heatingrod`-Repo — läuft aus, bleibt als Referenz für Rollback & History
- **Ausgelagerte Pakete** (aioesphomeserver, ds100-modbus, sorel-canbus): eigenständige
  Projekte, werden vom Rust-Port nicht gebraucht (nur Python-v2-Abhängigkeiten)
- **OpenWrt-Package**: `ddimension-openwrt-repo/heatingrod/` — Source-Tarball aus diesem Repo
  (`files/heatingrod-<version>.tar.xz`, `PKG_HASH` im Makefile). Neu erzeugen und
  eintragen: Anleitung in `heatingrod/README.md` des Feeds; Regeln für jede
  Feed-Änderung in dessen `CLAUDE.md` — committet wird auf `main`, `stable` bekommt
  es nur per `scripts/release-stable.sh` und nur auf Ansage. Das Paket wird von der
  Feed-CI **nicht** gebaut (rust/host aus Quellen, ~35–45 GB) — lokal testen:
  `RELEASES=snapshot ARCHS=x86_64 PACKAGES=heatingrod scripts/local-build.sh`.
