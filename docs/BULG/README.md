# BULG SMP4/V2 Wärmepumpe (12 kW, R290) — Modbus-Integration

Angebot: eBay (bulg-kessel), ~2.999 €, Titel „BULG Luft-/Wasser Mono Pro
DC Inverter Wärmepumpe | 12 KW | R290 | Wifi | Bafa/K". Bewertung
20.09.2026: Modbus-Doku ist echt und für die v3-Steuerung ausreichend
(siehe PDF in diesem Ordner). Kaufbedingungen: Phase (1~ vs 3~) klären,
BAfA-Listeneintrag prüfen, Garantie-Abwicklung in DE bestätigen.

## RS485-Parameter

- 19200 Baud, 8 Datenbits, **2 Stoppbits**, keine Parität
- Modbus RTU, 16-Bit, CRC16, Low-Byte zuerst
- Geräteadressen 1–8 (DIP 2–4); **nur Adresse 1 akzeptiert Schreibzugriffe**
- FC: 03 (Read Holding), 06 (Write Single), 10 (Write Multiple),
  01 (Read Coils), 05 (Write Single Coil)

## Register (Auszug)

| Adresse | R/W | Bedeutung | Skalierung |
|---|---|---|---|
| 0x0000 | R | Kompressorlaufzeit | — |
| 0x0001 | R | COP | — |
| 0x0003 | R | Schalt-/Arbeitsstatus-Bits: Heizbedarf, WW-Bedarf, Abtauen, Antilegionellen, Netz-SG-Signal | Bitmap |
| 0x000E | R | Wasserzulauftemperatur | ×0,1 °C |
| 0x000F | R | Wassertanktemperatur | ×0,1 °C |
| 0x0011 | R | Umgebungstemperatur | ×0,5 °C |
| 0x0012 | R | Wasseraustrittstemperatur | ×0,1 °C |
| 0x0032 | RW | Bit 0: Ein/Aus (Bitadresse 0x0320, Coil 05H) | Bitmap |
| 0x0034 | RW | Bit 6: Smart Grid ein, Bit 7: SG mit Elektroheizung; Bit 0: Antilegionellen | Bitmap |
| 0x0036 | RW | Betriebsmodus: 0=WW, 1=Heizung, 2=Kühlung, 3=Heizung+WW, 4=Kühlung+WW | — |
| 0x00CA | RW | WW-Sollwert 10–55 | ×0,5 °C |
| 0x00CC | RW | Heizsollwert 10–55 | ×0,5 °C |
| 0x00C6/0x00C8 | RW | Temperaturdifferenzen Kühlen/Heizen bzw. WW | 2–18 °C |

## v3-Integrationsplan

- Client-Modul `hardware/heatpump.rs` nach DS100-Muster (serialport +
  Modbus-Frames), erweitert um FC01/05 (Coils) für Ein/Aus
- RS485: eigener USB-RS485-Adapter (19200/8N2 ≠ DS100-Parameter)
- Zweiter DS100 misst den WP-Verbrauch (getrennte Leitung)
- Regelstrategie: PV-Überschuss → WP an + Heizsollwert/Modus setzen;
  Status-/Bedarf-Bits + Temperaturen als Sensoren in HA (ESPHome-Sub-Device)
- SG-Signal (0x0003 Bit 7) für die Scheduler-Rückmeldung

## Varianten (EPREL-Fiches)

| Modell | ηs NT / MT | Pdesign NT | Schall außen |
|---|---|---|---|
| BULG Mono Pro 12 KW (EPREL 2576278) | **195 % / 147 %**, A+++/A++ | 9 kW | 58 dB |
| BULG Mono Pro 15,2 KW (EPREL 2592867) | 190 % / 146 %, A+++/A++ | 13 kW | 60 dB |

Die 12-kW-Variante ist im NT-Zielbetrieb minimal effizienter und leiser;
die 15,2-kW-Variante hat mehr Leistungsreserve (13 kW Pdesign). Phase
(1~ vs 3~) geht aus den Fiches nicht hervor — Installationshandbuch oder
Hersteller fragen.

## Nachtrag (20.09.2026): Leistungs-/Frequenz-Register

| Adresse | R/W | Bedeutung |
|---|---|---|
| 0x0017 | R | Elektrische Leistungsaufnahme (W) |
| 0x0019 | R | Heiz-/Kühlleistung (W) |
| 0x001A | R | Wechselstrom (A) |
| 0x0018 | R | Pumpenwasserdurchfluss (m³/h) |
| 0x001E | R | Aktuelle Kompressorfrequenz |
| 0x0024 | R | Zielfrequenz (intern) |
| 0x0025 | R | Smart-Grid-Status |
| 0x0032 Bit 2 | RW | **Manuelle Frequenzauswahl** (Modus-Bit) |

OFFEN bei BULG: Wohin wird im manuellen Frequenzmodus der Frequenz-Sollwert
geschrieben? (kein RW-Hz-Register in der PDF dokumentiert). Falls
beantwortet → direkte grobe Leistungssteuerung möglich (30–100 %,
Minuten-Takt); Heizstab bleibt Feinregler.
