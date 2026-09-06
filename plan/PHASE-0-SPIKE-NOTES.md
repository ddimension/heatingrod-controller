# Phase 0 — Spike-Ergebnisse (2026-09-04)

Crate: `esphome-native-api` 3.0.0 vendored + 2 Patches (siehe CRATE-PATCHES.md).
Verifikation: `crates/esphome-api-server/examples/crate_spike.rs` (Server, Port 6054)
gegen `tests-py/spike_check.py` (aioesphomeapi aus dem heatingrod-venv — dieselbe
Client-Bibliothek wie HA).

| # | Frage | Ergebnis |
|---|-------|----------|
| 1 | Builder: devices[]/project/esphome_version im DeviceInfo-Auto-Reply | **Patch nötig und angewendet.** aioesphomeapi liest: `mac=B8:27:EB:96:AE:01`, `esphome_version=2026.6.2`, `project=custom.heatingrod/2.0.0`, `devices=[(1,'DS100 Energiezähler'),(2,'Thyristorsteller')]`. `project_name/version` und `compilation_time` sind upstream-Builder-Felder (Plan-Agent lag hier falsch). |
| 2 | HelloResponse api_version 1/14 | Setzbar (Builder). Verbindung von aioesphomeapi ohne Probleme. |
| 3 | msg124 (NoiseEncryptionSetKey) | Structs existierten im Proto, waren aber **nicht in parser.rs verdrahtet** (Mapping endet bei 123) → Patch 2. Unser Server antwortet `success=false`. Diese aioesphomeapi-Version sendet 124 nicht; Echttest mit HA in Phase 1. |
| 4 | Client-Mode (Initiator) | **TIMEOUT bestätigt**: `start()` peekt das erste Byte → deadlockt als Client. Initiator-Patch für Phase 4 geplant (kaskade/powerlogger). Hostname kalnet von Dev-Maschine erreichbar. |
| 5 | Plaintext-Rejection | Crate sendet `[0x01][len]["Only key encryption is enabled"]` (Python sendete leeres Frame). aioesphomeapi wirft darauf **`RequiresEncryptionAPIError`** — das Key-Prompt-Signal für HA funktioniert. |
| 6 | Receiver-Semantik | `receiver()` = `resubscribe()` (kein Verlust nach Erstellung). Initial-Burst, 5s-Pushes und ClimateCommand (`has_mode=true mode=3 has_target=true target=72`) kamen korrekt an. |
| + | Falscher Key (gültige b64, 32 Byte) | Server sendet ServerHello + „Handshake MAC failure" → aioesphomeapi wirft `InvalidEncryptionKeyAPIError` mit `received_name=heatingrod-test, received_mac=...`. Entspricht dem Python-Verhalten. |
| + | strip_option-Setter | Feldname nimmt T, `_opt`-Name nimmt Option<T> (umgekehrt zur ursprünglichen Annahme). |

## Konsequenzen für den Plan

- Keine weiteren Crate-Patches vor Phase 4 (dann Initiator-Patch).
- Phase 1 kann direkt mit dem gepatched Builder arbeiten (devices, esphome_version).
- Der 10s-Timeout-Test (msg124) braucht echtes HA (Phase 1, manueller Add).
- `tests-py/spike_check.py` bleibt als Grundlage der Cross-Check-Suite (Phase 2).
