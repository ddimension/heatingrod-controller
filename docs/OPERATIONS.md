# Betrieb & Wartung (Operations)

Betriebsdokumentation für den heatingrod-controller v3 auf dem Pi.
Kontext und Sicherheitsregeln: [../CLAUDE.md](../CLAUDE.md).

## Build

```bash
# Dev-Maschine: Rust stable (rust-toolchain.toml), zig 0.14.1 in ~/.local/opt
cargo test --workspace                     # 175 Tests

# Release für den Pi (aarch64, statisch musl):
cargo zigbuild --release --target aarch64-unknown-linux-musl -p heatingrod

# Fallback: direkt auf dem Pi bauen (rustup dort installiert)
```

**Musl-Target auf dem Pi:** statisches Binary (~1,5 MB, keine
Libc-Abhängigkeit). Seit der Kernel-w1-Umstellung (2026-09-04) gibt es
kein dlopen mehr. Journald läuft über einen **eigenen nativen Layer**
(`crates/heatingrod/src/logging.rs`, Datagramm-Protokoll, kein
libsystemd-Link — der wäre mit statischem musl unmöglich) mit identischem
Feld-Set (PRIORITY, SYSLOG_IDENTIFIER, TARGET, CODE_FILE/LINE). Ohne
journald-Socket fällt er automatisch auf Stderr zurück.

## OpenWrt (musl) als Ziel

OpenWrt ist ein explizites Ziel des Projekts; dort ist **musl Standard**.
Drei Punkte beachten:

1. **1-Wire braucht Kernel-Module:** auf OpenWrt `kmod-w1` +
   `kmod-w1-master-ds2490` + `kmod-w1-slave-therm` als Package-Dependencies
   (die `+libow-capi`-Abhängigkeit ist mit dem Kernel-w1-Testlauf
   2026-09-04 entfallen).
2. **journald gibt es auf OpenWrt nicht.** Der native Journald-Layer
   prüft den Socket `/run/systemd/journal/socket` und fällt ohne ihn
   automatisch auf Stderr zurück (procd → logd). Kein Feature-Gate nötig —
   ein Binary für Debian und OpenWrt.
3. **Kein libudev:** `serialport` ist mit `default-features = false`
   eingebunden (libudev wird nur für Port-Enumeration gebraucht, wir nutzen
   Config-Pfade) — zugleich die Cross-Compile-Voraussetzung. `i2cdev`
   spricht Kernel-SMBus-ioctls, funktioniert auf OpenWrt, wenn das
   I2C-Device existiert.

Typische OpenWrt-Targets: `aarch64-unknown-linux-musl`, `armv7-unknown-
linux-musleabihf`, `mipsel-unknown-linux-musl` — alle via `cargo zigbuild`.
Package-Build: `ddimension-openwrt-repo/heatingrod/`.

## Deploy auf den Pi

```bash
cargo zigbuild --release --target aarch64-unknown-linux-musl -p heatingrod
rsync -a target/aarch64-unknown-linux-musl/release/heatingrod \
      root@heizung:/root/heatingrod-rust/heatingrod.new
ssh root@heizung "systemctl stop heatingrod-controller-v3 && \
  mv /root/heatingrod-rust/heatingrod.new /root/heatingrod-rust/heatingrod && \
  systemctl start heatingrod-controller-v3"
```

- Config: `/root/heatingrod-rust/production.yaml` (gitignored, enthält PSKs)
- `calibration.json`/`state.json` liegen neben der Config (gitignored)
- Rollback-Binary (GNU-Build, Stand 04.09.2026): `/root/heatingrod-rust/heatingrod.gnu`
- **Ausgelagerte Pakete** (aioesphomeserver, ds100-modbus, sorel-canbus)
  werden im Rust-Port nicht gebraucht — sie sind Python-v2-Abhängigkeiten.

### 1-Wire-Kernelmodule

Seit 2026-09-04 aktiv: Module `wire`, `ds2490`, `w1_therm`, geladen via
`ExecStartPre` des v3-Units. Sensoren liegen unter
`/sys/bus/w1/devices/28-*/w1_slave`; Kernel-Adressen sind LSB-first kodiert
und werden vom Poller aufs Config-Format (`28.FF…`) kanonisiert.

**Rollback auf Python v2 erfordert vorher** `rmmod w1_therm ds2490 wire`
(sonst hält der Kernel-Treiber das USB-Device für libowcapi besetzt) und
das Entfernen der ExecStartPre-Module-Zeilen aus dem v3-Unit (relevant,
wenn v3 beim Reboot disabled wird).

## Verifikation nach Deploy

1. Journal: `journalctl -u heatingrod-controller-v3 -f`
2. STATUS-Line (alle 60s) prüfen — alle Felder aussagekräftig:

```
STATUS | heating | grid=-508W(pl) pv=3915W rod=2278W/47.0°C tank=57.6°C
dac=4.1V/41% delay=n/a | hapi=ok pl=ok,msgs=54,intv=1.4s
ds100=232V,49.9Hz,mbus=524,0e cbus=ok,pkts=0,errs=0
1wir=12/0e,cyc=3,8279ms idac=ok pc1=80°C ksk=off
```

   - `grid(pl)` = Powerlogger direkt (sonst `(ha)` = HA-Fallback)
   - `1wir=12/0e` = 12 Sensoren, 0 Lesefehler; `cyc/ms` = Zykluszähler/-dauer
   - `pc1=80°C` = CH1-Sollwert (8V); `ksk` = Kaskade-Istzustand
3. HA: 71 Entities available, 0 neue Entities (Post-Deploy-Check wie gehabt)

## Rollback auf Python v2

v2 bleibt installiert (disabled, startet nicht bei Boot):

```bash
ssh root@heizung "systemctl stop heatingrod-controller-v3 && \
  systemctl start heatingrod-controller-v2"
```

`state.json`/`calibration.json` sind schema-kompatibel geteilt (verifiziert
in der Rollback-Probe 2026-09-04). v2 erst nach 7 Tagen sauberem v3-Betrieb
deinstallieren. 1-Wire-Hinweis: vorher `rmmod w1_therm ds2490 wire`.

## CAN/USBtin: verklemmter Adapter (Vorfall 04.09.2026)

Symptom: `cbus=ok,pkts=0` dauerhaft, obwohl der Bus Verkehr hat. Der USBtin
klemmt intern: `C\r` wird mit NAK (BEL, 0x07) beantwortet, obwohl der Kanal
zu ist — auch der **Kernel-slcans-Treiber bricht dann den Init ab** (netdev
unregistered ~30ms nach Attach). `rmmod cdc_acm` allein hilft NICHT (resettet
nur den Host-Treiber, nicht den MCU).

Heilung — USB-Gerät de-authorisieren (echter Bus-Detach → MCU-Reset):

```bash
echo 0 > /sys/bus/usb/devices/1-1.3/authorized   # USBtin (lsusb-Tree prüfen!)
sleep 2
echo 1 > /sys/bus/usb/devices/1-1.3/authorized
```

Danach ackt `C` wieder sauber (`\r`). Diagnose ohne Dienst-Stop:
v3 stoppen, dann mit python3/pyserial `C\r`, `S5\r`, `O\r` senden und die
Acks ansehen — `\x07` = verklemmt. Der Controller toleriert das NAK im
laufenden Betrieb, bekommt aber keine Frames → bei `pkts=0` über längere
Zeit an den USB-Reset denken.

## Sicherheitsinvarianten (nie verletzen)

- CH0 → 0V bei jedem Fehler/Signal/Disconnect; ExecStopPost `tools/dac-safe`
  schreibt CH0=0V/CH1=6V direkt per I2C, auch bei Crash
- CH1 → 6V (safe) bei Shutdown — **nie 0V**; 0V nur via Climate-OFF
- CH1 < 0,05V → Kaskade ON (Brennersperre aktiv)
- Entity-Keys = MD5(object_id) erste 8 Hex als u32 (HA-Identität!)
- MAC UPPERCASE in device_info (Vorfall 18.08.2026)
