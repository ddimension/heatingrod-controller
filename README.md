# heatingrod-controller

**PV-Überschuss-gesteuerter Heizstab-Controller in Rust** — regelt einen 7,5 kW
Heizstab (800 L Pufferspeicher) ausschließlich mit Solar-Überschuss und tritt
gegenüber Home Assistant als **ESPHome Native API Server** auf (Port 6053,
Noise-verschlüsselt). Statisches aarch64-musl-Binary, produktiv im Einsatz seit
2026-09-04.

```
PV (mppt1/2) ─┐                    ┌─ ESPHome Native API Server (71 Entities, HA)
EasyMeter Q3A ─┤ powerlogger (ESP) │
DS100 (Modbus)─┤                   │  Regelung: PV-Überschuss → 0–10 V (DAC GP8403)
SOREL (CAN)   ─┤ heatingrod v3     ├─→ Thyristorsteller T-Drive 3Ph → Heizstab
1-Wire (w1)   ─┤                   │
               ┘                   └─ Kaskade-Client → ProControl 3 Brennersperre
```

## Was der Controller kann

- **ESPHome Native API als Server**: HA integriert den Controller wie ein
  echtes ESPHome-Gerät — 71 Entities in 5 Sub-Devices (Controller, DS100,
  Thyristor, SOREL, 1-Wire), Climate-Entity für den Kessel-HK1-Sollwert
- **Regelung** mit EWMA-geglättetem Leistungs-Feedback (DS100, 100 ms),
  Settle-Timer gegen Burst-Artefakte, automatischer Feedback-Delay-Messung
- **Selbstlernende Kalibrierung** (Voltage→Watts) mit Totband-Schutz,
  Monotonie-Projektion (Pool-Adjacent-Violators) und Kennlinienvergiftungs-Schutz
- **Sicherheit**: Watchdog (10 s), Sensor-Freshness-Checks, Thermal-Limiter-
  Erkennung (~67 °C analog), DAC-auf-0V bei jedem Fehler, `dac-safe`
  Failsafe-Binary via ExecStopPost (CH0=0V/CH1=6V auch bei Crash)
- **Direktverbindungen**: Powerlogger-ESP (EasyMeter Q3A), DS100 Modbus RTU,
  SOREL CAN via USBtin (SCBI-Dekodierung), 1-Wire über Kernel-w1 (sysfs)
- **Kaskade**: ESP8266-Relais sperrt/gibt den Ölbrenner frei (ProControl 3)
- **Logging**: nativer Journald-Layer ohne libsystemd (musl-tauglich), 
  STATUS-Zeile alle 60 s, Log-Forwarding an HA

## Hardware (Anlage „Heizung")

| Komponente | Gerät |
|---|---|
| Kessel | Götz Pro Condens PC1 15 (Öl-Brennwert, 8–18 kW) |
| Kesselsteuerung | Götz ProControl 3 — angebunden via 0-10V-Eingang (Par 61) + Kaskadeneingang (230V-Relais) |
| Speicher | Austria Email PSR/PSRR 800 (800 L, ~23 kWh nutzbar) |
| Heizstab | 7,5 kW, 3-phasig, analoge Temperatursperre ~67 °C |
| Leistungsregler | Chiemtronic T-Drive 3Ph compact 12A (Schwingungspaketsteuerung) |
| DAC | DFRobot GP8403 (I2C 0x5f) → 0-10 V |
| Energiezähler | EasyMeter Q3A (Netz, via Powerlogger-ESP), B+G E-Tech DS100 (Heizstab, Modbus) |
| Warmwasser | SOREL MTDCv5 (CAN via USBtin) |
| Temperatur | 12× DS18B20 (1-Wire, DS1490F-Adapter, Kernel-w1) |
| Controller | Raspberry Pi (Debian, aarch64) |

Details, Parameter und Sicherheitsregeln: [CLAUDE.md](CLAUDE.md).

## Repo-Layout

```
crates/esphome-api-server/   ESPHome-Protokoll-Schicht (Rust-Port von aioesphomeserver)
crates/heatingrod/           App: Controller, Kalibrierung, Hardware, Clients
tools/dac-safe/              Failsafe-Binary für ExecStopPost (CH0=0V, CH1=6V)
tests-py/                    aioesphomeapi-Cross-Check (golden_diff.py)
vendor/esphome-native-api/   Vendored + gepatched (plan/CRATE-PATCHES.md)
plan/                        Design-Notizen: Cutover, Crate-Patches, Phasen
config/                      production.example.yaml (bereinigt), test-device.yaml
docs/                        Handbücher (ProControl 3, T-Drive, Pufferspeicher) + Ops
grafana/                     Dashboard-Definitionen (heizung, sorel, ds100)
systemd/                     heatingrod-controller-v3.service
```

## Quick Start

```bash
# Voraussetzungen: Rust stable (rust-toolchain.toml), cargo-zigbuild + zig für Cross-Builds
cargo test --workspace     # 175 Tests

# Release für den Pi (aarch64, statisch musl, ~1,5 MB):
cargo zigbuild --release --target aarch64-unknown-linux-musl -p heatingrod
```

Das musl-Binary ist vollständig statisch (kein dlopen, keine Libc-Abhängigkeit);
Journald läuft über einen eigenen nativen Datagramm-Layer. OpenWrt (musl) wird
unterstützt — Paket-Build siehe `ddimension-openwrt-repo`.

## Deployment & Betrieb

Deploy auf den Pi, Verifikation, Rollback auf Python v2, 1-Wire-Kernelmodule
und die USBtin-Wedge-Prozedur: [docs/OPERATIONS.md](docs/OPERATIONS.md).

## Status

- **Produktiv** auf dem Pi (`heatingrod-controller-v3.service`, Port 6053)
  seit 2026-09-04 — identische Geräte-Identität wie der Python-Vorgänger,
  kein Entity-Registry-Churn in Home Assistant
- Python v2 (Vorgänger) läuft im alten `heatingrod`-Repo aus; dieses Repo
  enthält die extrahierte Rust-Historie mit originalen Commit-Timestamps

## Lizenz

MIT — siehe [LICENSE](LICENSE).
