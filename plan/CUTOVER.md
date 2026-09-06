# Cutover-Runbook: Python v2 → Rust v3 (SELBE Identität, Port 6053)

## Gates (ALLE vorher erfüllt — nicht überspringen)

- [x] Phase-5-Parität: Shadow-Lauf ohne unerklärte Abweichungen (Cutover auf
      Betreiber-Entscheidung vorgezogen — "so lange wir noch Sonne haben")
- [x] Kaskade-Client gegen echte ESP verifiziert (`real_kaskade_probe` + Session-Test)
- [x] Climate-Pfad gegen Mock-DAC getestet (HEAT/OFF/mode-only/target-only)
- [~] `tools/dac-safe`: lief mehrfach in Produktion (ExecStopPost bei jedem
      Restart, Journal `DAC safe: CH0=0V CH1=6V(60C)`); Multimeter-Verifikation
      der Spannungen steht noch aus (Betreiber-Aktion am Pi)
- [x] `production.yaml` auf dem Pi = Kopie von `/root/heatingrod/config.yaml`
- [x] Letzter Golden-Diff: leer (Entity-Baum byte-identisch)

## Status 04.09.2026 (Cutover durchgeführt, Soak läuft)

- Cutover ~11:00, v3 produktiv auf 6053 mit identischer Identität (PID-seitig
  verifiziert: keine neuen HA-Entities, 0 unavailable)
- Rollback-Probe durchgeführt (v3→v2→v3, v2 übernahm nahtlos aus geteilten
  state.json/calibration.json und heizte sofort)
- Boot-Ownership: v3 `enabled`, v2 `disabled` (manueller Rollback bleibt via
  `systemctl start heatingrod-controller-v2`)
- 7-Tage-Soak läuft (bis ~11.09.) — danach v2 deinstallieren
- Paritäts-Audit 04.09.2026 abgeschlossen, alle Divergenzen gefixt
  (siehe CRATE-PATCHES.md, Abschnitt "Paritäts-Audit")

## Vorbereitung auf dem Pi

```bash
# production.yaml ist die ECHTE Produktionsconfig (identische Identität):
#   MAC B8:27:EB:96:AE:00, Name heatingrod, Port 6053, echter PSK
ssh root@heizung "cp /root/heatingrod/config.yaml /root/heatingrod-rust/production.yaml"

# Unit installieren (v2 bleibt installiert → Sofort-Rollback)
scp rust/systemd/heatingrod-controller-v3.service root@heizung:/etc/systemd/system/
ssh root@heizung "systemctl daemon-reload"
```

## Ablauf

1. **Shadow stoppen:** `systemctl stop heatingrod-rust-test`
2. **Python stoppen:** `systemctl stop heatingrod-controller-v2`
   → Journal prüfen: v2-ExecStopPost-Log „DAC safe: CH0=0V CH1=6V(60C)".
   → CH1=6V ist der korrekte Fail-Safe-Zustand (Kessel läuft frei).
   → Hinweis: Der PyScript-Scheduler kann kurz darauf den Sollwert wieder
     ändern — HA sieht das Climate als offline; der Brenner läuft mit dem
     zuletzt geschriebenen Analogwert.
3. **Port prüfen:** `ss -tlnp | grep 6053` muss leer sein.
4. **v3 starten:** `systemctl start heatingrod-controller-v3`
   Journal-Check (`journalctl -u heatingrod-controller-v3 -f`):
   - state.json wiederhergestellt (CH1)
   - Kalibrierung geladen (94 Punkte)
   - DAC initialisiert (echt!)
   - ESPHome-API auf 0.0.0.0:6053
   - HA verbindet (Hello + Subscribed to 8 HA entities)
   - powerlogger/kaskade/ds100/1wire/canbus verbinden
5. **HA-Verifikation** (~30s warten, dann über die HA-API):
   - Entity-Count + Availability: identische entity_ids wie vorher,
     **NULL neue Entities** (keine `_2`-Suffixe, keine Präfix-Namen),
     0 unavailable
   - `core.entity_registry` unverändert für das Gerät
6. **Smoke-Tests:**
   - Climate-Roundtrip: HA setzt 72°C → Journal `CH1=7.20V` (echt!),
     state.json aktualisiert, kaskade-Switch flippt (Brennersperre aus)
   - Climate OFF → CH1=0V, kaskade ON (Brenner gesperrt)
   - Manual Switch `input_boolean.heizstab_schalter` off → CH0=0V
   - Ein echter PV-Überschuss-Heizzyklus: Ramp → EWMA-Feedback →
     Kalibrierungs-Update → Cutoff bei Überschuss-Drop
   - PyScript-Scheduler-Transition (LEGIONELLA/NORMAL) landet korrekt
7. **Rollback-Probe (einmal testen):**
   ```bash
   systemctl stop heatingrod-controller-v3
   systemctl start heatingrod-controller-v2
   # Python stellt aus denselben state.json/calibration.json wieder her
   # (Schemata kompatibel — im Shadow trocken verifiziert)
   systemctl stop heatingrod-controller-v2
   systemctl start heatingrod-controller-v3
   ```

## Soak

- 7 Tage sauberer Betrieb → `systemctl disable heatingrod-controller-v2`
- Python-Baum 1 Monat behalten, dann archivieren
- Erst danach: `heatingrod-reset-dac.py`-Legacy + v2-Unit entfernen
