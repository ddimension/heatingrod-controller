# esphome-native-api — Vendoring & Patches

Basis: `esphome-native-api` **3.0.0** (MIT, UbiHome), vendored nach
`rust/vendor/esphome-native-api/`. Grund: Die Crate ist WIP mit
Breaking-Change-Historie, und zwei für uns kritische Felder fehlen im
Builder. Die High-Level-Abstraktion `EspHomeServer` wird NICHT genutzt
(nur BinarySensor, Encryption unverdrahtet, Zähler-Keys statt MD5).

## Patch 1 — `devices` + `esphome_version` Builder-Felder (src/esphomeapi.rs)

Upstream hartkodiert `devices: vec![]` und `esphome_version: proto::VERSION`
im DeviceInfo-Auto-Reply. Für Sub-Device-Identität (device_ids 0–4) und
Identitäts-Parität mit dem Python-Server brauchen wir beides konfigurierbar.

Geändert (markiert mit `// ---- heatingrod patch ----`):

1. `use`-Statement: `DeviceInfo` importiert.
2. Zwei neue Builder-Felder nach `project_version`:
   - `devices: Option<Vec<DeviceInfo>>` → Setter `.devices(vec![...])` (innerer Typ; `strip_option`-Semantik: Feldname = T, `_opt`-Name = Option<T>)
   - `esphome_version: Option<String>` → Setter `.esphome_version("2026.6.2")`
3. `device_info`-Konstruktion nutzt die Felder (Fallback: bisheriges Verhalten).

## Patch 2 — NoiseEncryptionSetKey 124/125 in parser.rs

Die Structs existieren im versionierten Proto (`version_2026_6_2.rs` Z. 672/677),
aber die handgepflegte Mapping-Liste in `parser.rs` endet bei 123 — upstream hat
124/125 nie verdrahtet. Ohne Antwort auf 124 wartet HA **~10s pro Connect**
(genau der Timeout, den die Python-Seite dokumentiert hat).

Geändert: Import der beiden Structs + zwei Mapping-Zeilen (markiert mit
`// heatingrod patch`). Unser Server antwortet auf 124 mit `success=false`
(Key ist config-verwaltet).

## Patch 3 — Initiator-Mode (Client-Handshake) in esphomeapi.rs

`EspHomeApi::start()` war responder-only (peekt das erste Byte, wartet auf
Frame 1). Als Client deadlockte das gegen echte ESPHome-Firmware (Phase-0-
Spike bestätigt). Geändert:

1. Builder-Feld `initiator: bool` (Default false = upstream-Verhalten).
2. `start()`: bei `initiator=true` wird das Framing aus dem konfigurierten
   Key abgeleitet statt gepeekt; der Noise-Handshake läuft als Initiator
   (`HandshakeState::new(..., true, ...)`, ClientHello + msg1 senden —
   **als Codec-Payloads**, der FramedWriter fügt `[0x01][len]` selbst hinzu —
   dann ServerHello/frame2 lesen, `get_ciphers()` mit vertauschter Reihenfolge:
   Initiator → erstes Element ist ENCRYPT).

Validiert (2026-09-04) gegen echte ESPs: kaskade (esp-heizung-ksk) und
powerlogger — Connect, List, Subscribe, State-Tracking laufen.

## Client-Learnings (kaskade/powerlogger gegen echte ESPHome-Firmware)

- **GetTimeRequest beantworten**: echte ESPHome-Server senden nach dem Hello
  ein GetTimeRequest und warten auf die Antwort, bevor sie weitere Requests
  verarbeiten — ohne Antwort kommt keine Entity-Liste zurück.
- **API ≥ 1.14: object_id ist optional auf dem Draht** (aioesphomeapi
  `MIN_VERSION_OBJECT_ID_OPTIONAL = (1,14)`). Der Client rekonstruiert es
  mit dem ESPHome-Algorithmus `sanitize(snake_case(name))` —
  `compute_object_id()` in clients.rs.

## Upgrade-Prozedur

Bei neuer Crate-Version: Diff der neuen Version gegen die vendored (ohne
unsere Patches), Patch-Teil übertragen, `CRATE-PATCHES.md` aktualisieren.
Unsere Patches sind im Quelltext markiert.

## Patch 4 — Answer-Channel 16 → 256 (esphomeapi.rs, `start()`)

Upstream nutzt `mpsc::channel::<ProtoMessage>(16)` für den Antwort-Kanal
einer Verbindung. Unser Server pusht den 71-Frame-Full-Push in einem engen
Loop (try_send ohne await); stockt der Write-Loop kurz, läuft der Kanal
voll und `try_send` liefert `Full` — was unser Broadcast als toten
Subscriber wertete und die HA-Verbindung **bei lebendigem TCP-Socket** aus
dem Hub warf (beobachtet 04.09.2026: `subscriber_count()=0` trotz ESTAB
und laufendem Push-Traffic). Fix hat zwei Teile: Kapazität 256 hier +
`TrySendError::Full`-tolerantes `broadcast()` in esphome-api-server.

## Paritäts-Audit 04.09.2026 (7 Agenten, Python v2 ↔ Rust v3)

Vollständiger Audit nach dem Cutover. Ergebnis + Fixliste (alle umgesetzt,
159 Tests grün, deployed):

**Konform verifiziert:** MD5-Keys/Golden-Values, Wire-Protokoll Feld-für-Feld,
MAC-Uppercase, Noise/msg124/Subscribe-Reihenfolge, Kalibrierung (byte-identischer
Sweep über die 94-Punkte-Produktionskurve), alle Regelkonstanten/Guards,
Lernpunkt-Reihenfolge, DAC-Register-Map/Shutdown, 1-Wire-Filter/Backoff.

**Gefixt:**
- I1: 1-Wire-Auto-Registrierung — Key wird nach object_id-Überschreibung neu
  berechnet (MD5(1w_28_…), sonst doppelte HA-Entities)
- S2: Kalibrier-Sleep interruptibel (Stop-Flag 1s-Slices + Command-Drain-Hook
  pro Sekunde — Scheduler-Kommandos wirken zwischen Steps statt nach dem Sweep)
- B7: slcan-USB-Disconnect → close + break + Reconnect (vorher stiller Dead-Loop)
- B6: Kaskade-Soll sofort (200ms-desired_seq-Poll statt 10s-Tick — Brennersperre)
- V1: climate.current_temperature_entity wird subscribt (Klima-Ist-Temperatur lebt)
- V2: Climate-Action-Echo meldet action=3 (heating), 10s change-gated
- B3/B4: co2_saved + overdraw im 30s-Push (energy_total_active-Delta × 0.268)
- H7: HC_STATE4 ohne &0xFF-Maske (Python rechnet über die volle u16)
- B9: DS100-Timeout schließt den Port nicht mehr (nur echte io-Fehler)
- W1: leere 1-Wire-Discovery → Fehler + Reconnect (kein totes initialisiert=true)
- W2: out-of-range zählt read_errors nicht mehr (Log-Gate-Parität)
- Logging: PRIORITY INFO→6 (Informational), -v nur First-Party-DEBUG
  (Targets-Filter), --log-module implementiert, LogForwardLayer filtert auf
  heatingrod:: + INFO+, Disconnect-WARN, Gap-Warnung (>10s), Entity-not-found-
  Errors, Grid-Source-Wechsel-Logs, kaskade-Zustandswechsel INFO, esphome_version
  2026.3.1 (Python-Default), GetTimeRequest-Antwort, Subscribe-Dedup,
  "(mock)"-Suffix entfernt, erster Watchdog-Tick bei 15s (Python-Parität),
  Controller-Thread-Tod → Exit (systemd-Restart + dac-safe)
- Watchdog-Test für die subscribers==0-Operator-Entscheidung ergänzt

**Bewusst offen (dokumentiert):** Notifications (auf beiden Seiten seit
2026-03-30 nicht implementiert — Operator: belassen), is_responsive als
Flag statt Bus-Probe (Kommentar korrigiert), SYSLOG_IDENTIFIER-Schema (v3:
ein Ident, Module im TARGET-Feld — CLAUDE.md angepasst).

## Upstream-Rückgabe (geplant, Stand 04.09.2026)

Die vier Patches sollen als PRs an UbiHome/esphome-native-api zurückfließen
(Upstream-Stand verifiziert: responder-only, kein 124/125-Mapping, README
behauptet Client-Support). Vorschlag:

1. **PR Bugfix:** parser.rs 124/125-Mappings (+ Channel 16→256)
2. **PR Feature:** `devices[]` + `esphome_version` Builder-Felder
3. **PR Feature (groß):** Initiator-Mode (Client-Handshake; Wire-Test aus
   tests/wire_client.rs als Regressionstest mitgeben)

Push läuft über den GitHub-Account des Betreibers (Fork von
UbiHome/esphome-native-api); lokale Branch-Vorbereitung aus dem Vendor-Tree
mit entfernten `// heatingrod patch`-Markern. Noch nicht ausgeführt —
Betreiber entscheidet über Umfang (alle drei oder nur 1+2).
