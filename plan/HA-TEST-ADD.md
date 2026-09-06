# HA manueller Eintrag des Rust-Testgeräts

Das Testgerät läuft auf `heizung` (192.168.203.199:6054), parallel zur
Python-Produktion (0.0.0.0:6053). Es wird **manuell** hinzugefügt — die
Crate unterstützt kein mDNS, und Auto-Discovery würde mit dem
Produktionsgerät kollidieren.

## Voraussetzungen

- Binär läuft auf dem Pi: `systemd-run --unit=heatingrod-rust-test /root/heatingrod-rust/heatingrod --config /root/heatingrod-rust/config/test-device.yaml`
- Log prüfen: `journalctl -u heatingrod-rust-test -f` — Zeile
  `MAC from interface eth0: XX:XX:XX:XX:XX:XX` merken.

## HA-Eintrag

1. Einstellungen → Geräte & Dienste → Integration hinzufügen → **ESPHome**
2. Host: `192.168.203.199:6054` (IP:Port — HA akzeptiert das im Host-Feld;
   falls nicht, nur IP und Port-Feld separat nutzen)
3. HA probt → erkennt verschlüsseltes Gerät → fragt nach dem
   **Encryption Key** → PSK aus `config/test-device.yaml` einfügen.
4. Gerät erscheint als `heatingrod-test` mit den Entities
   `sensor.rust_uptime`, `number.rust_power_limit`, `climate.rust_test_klima`
   (+ Sub-Devices DS100 Energiezähler/Thyristorsteller).

## Phase-1-Akzeptanzprüfungen

| Prüfung | Erwartung |
|---|---|
| Entities available | alle `available`, nicht `unavailable` |
| State-Push | `rust_uptime` steigt alle 5 s |
| Climate: nur Modus | HA setzt HEAT → Server loggt `has_target=false`, behält 60°C |
| Climate: Modus+Ziel | HA setzt 72°C → Log `CH1=7.20V (mock)` |
| Climate: OFF | Log `CH1=0V (mock)` |
| Number-Slider | Wert erreicht Server, wird zurückgespiegelt |
| Reconnect | HA-Client trennen/wiederverbinden → Entities bleiben available |
| Falscher Key | zweiter Eintrag mit falschem Key → HA meldet „Invalid encryption key", Server loggt Handshake-Fehler, kein Crash |
| Kein 10s-Timeout | Verbindung baut zügig auf (msg124→success=false); Log zeigt `Declining Noise key provisioning` falls HA 124 sendet |
| STATUS-Log | `journalctl` zeigt `STATUS | test-device | …` jede Minute |

## Aufräumen

`systemctl stop heatingrod-rust-test` + HA-Eintrag löschen (Geräte & Dienste
→ ESPHome heatingrod-test → Löschen). Entities verschwinden mit dem Eintrag.
