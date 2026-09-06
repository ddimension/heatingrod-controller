# ProControl 3 — Systemanalyse & Fernsteuerung

Stand: 2026-03-31. Quellen: Service Manual SW 1.53, Konfiguration Heizkreise.txt, Schaltbild.

---

## Hydraulische Struktur

```
                    ┌─────────────────────────────┐
                    │     Götz PC1 15 Kessel       │
                    │  Kessel:     86.5°C (aktuell) │
                    │  Kessel RL:  69.5°C           │
                    │  Abgas:      32.5°C           │
                    │  Brenner:    AUS              │
                    └──────────────┬───────────────┘
                                   │ HK-1 Ladepumpe
                                   ▼
              ┌────────────────────────────────────┐
              │     Austria Email PSR 880L          │
              │     Pufferspeicher                 │
              │                                    │
              │  Oben:  80.5°C  ← BF-Fühler       │
              │  Unten:  50.0°C ← PUF-Fühler      │
              └──┬─────────────────────┬───────────┘
                 │                     │
                 │ HK-2 Mischer        │ Frischwasserstation
                 ▼                     ▼
          Heizkörperkreis         Warmwasser (autark,
          VL: 59°C / Soll: 47°C   kein ProControl-Sensor)
```

**Wichtig:** „Boiler" im ProControl = der Pufferspeicher (BF-Fühler oben). Kein separater WW-Boiler. Die Frischwasserstation ist hydraulisch direkt am Puffer, hat keinen eigenen ProControl-Eingang.

---

## Aktuelle Konfiguration (erfasst 2026-03-31)

### HK-1: Pufferladekreis

| Parameter | Wert |
|-----------|------|
| Betriebsart | Uhrzeit |
| Heizzeiten | Mo-So 06:00–23:00 |
| Absenkung | 8°C |
| Fußpunkt | 30°C |
| Heizbetrieb unter | 20°C Außen |
| Absenkbetrieb unter | 10°C Außen |

HK-1 ist **kein Heizkörperkreis** — er pumpt Kesselwasser in den Puffer (Ladepumpe). Im Sommer (Außen > 20°C) ist HK-1 inaktiv.

### HK-2: Heizkörperkreis (witterungsgeführt)

| Parameter | Wert |
|-----------|------|
| Betriebsart | Uhrzeit |
| Heizzeiten | Mo-So 06:00–23:00 |
| Absenkung | 5°C |
| Fußpunkt | 35°C |
| Heizbetrieb unter | 20°C Außen |
| Absenkbetrieb unter | 20°C Außen |

HK-2 regelt via Mischer den Vorlauf witterungsabhängig. Bei 6°C Außen ergibt sich Soll ~47°C.

### Boiler (= Pufferspeicher)

| Parameter | Wert |
|-----------|------|
| Solltemperatur | 60°C |
| Absenkung | 5°C |
| Hysterese (Par 16) | 5K → Ladestart bei 55°C |
| Heizzeiten | 05:30–09:30, 16:00–20:30 |
| Legionellenschutz (Par 59) | Freigegeben — montags 1h auf Max |

### Aktueller Status (Momentaufnahme)

| Sensor | Wert | Bewertung |
|--------|------|-----------|
| Kessel | 86.5°C | Nachlauf, Brenner aus (RL > Par4: 67°C) |
| Kessel RL | 69.5°C | Zu hoch für Brennerstart |
| Boiler (Puffer oben) | 80°C | 20°C über Soll — Heizstab hat geladen |
| Abgas | 32.5°C | Sehr gut, Vollbrennwert kondensiert |
| HK-2 ist/soll | 59°C / 47°C | Mischer regelt runter |
| Puffer unten | 50°C | Ausreichend für HK-2 |
| Außen | 6°C | Heizbedarf vorhanden |
| Fernsteller1 | −3°C | Bewohner haben manuell leicht abgesenkt |
| Steuerung | 33.5°C | Raumtemperatur (Heizungsraum?) |
| Kesselpumpe | AUS | Korrekt, Brenner aus |
| HK-1 Pumpe | AUS | Puffer bereits voll |
| HK-2 Pumpe | EIN | Heizkörper werden versorgt |

---

## Relevante Service-Parameter

### Kesselschutz

| Par | Bezeichnung | Werkeinstellung | Bedeutung |
|-----|-------------|-----------------|-----------|
| 1 | Kessel RL min | 53°C | Verbraucher aus bei Unterschreitung — Korrosionsschutz Stahl-WT |
| 2 | Kesseltemp. max | 82°C | Brenner aus bei Überschreitung |
| 3 | Kessel RL ein | 56°C | Brenner startet wenn RL darunter |
| 4 | Kessel RL aus | 67°C | Brenner stoppt wenn RL darüber |
| 5 | Sollwertüberhöhung | 0°C | Keine zusätzliche Überhöhung |
| 20 | HK1 bei Puffer | 5K | Kessel muss 5K heißer sein als HK-1 Soll um Puffer zu laden |

**Brenner-Hysterese:** Startet bei RL < 56°C, stoppt bei RL ≥ 67°C → 11K Hysterese verhindert Takten.

### Temperatur-Grenzen

| Par | Bezeichnung | Werkeinstellung |
|-----|-------------|-----------------|
| 11 | Temp. HK-1 max | 70°C |
| 13 | Temp. HK-2 max | 60°C |
| 14 | Temp. Boiler min (Absenkung) | 40°C |
| 15 | Temp. Boiler max (Legionellenschutz) | 65°C |

### Konfiguration (F=gesperrt, G=freigegeben)

| Par | Bezeichnung | Werkeinst. | Status bei uns |
|-----|-------------|------------|----------------|
| 37 | Boilerkreis | F | Freigegeben (Boiler aktiv) |
| 39 | Heizkreis Nr. 2 | G | Freigegeben (HK-2 aktiv) |
| 41 | Außenfühler HK1 | F | Gesperrt? → HK-1 läuft auf festen Sollwert |
| 42 | Außenfühler HK2 | G | Freigegeben → witterungsgeführt |
| 43 | Pumpe HK1 | F | Gesperrt → Kesselpumpe übernimmt |
| 46 | Kessel abkühlen | G | Freigegeben → Kessel kühlt bei Kaskadensperre |
| 51 | Kas. Eing. Invert. | G | **Kessel frei wenn 230V am Kaskadeneingang** |
| 55 | Puffer | G | Freigegeben — Pufferspeicher aktiv |
| 59 | Legionellenschutz | F | Gesperrt (Werkeinst.) — prüfen |
| **61** | **Analog 0-10V** | **G** | **Werkeinst. freigegeben — HK-1 Sollwert extern** |

---

## Externe Schnittstellen für Fernsteuerung

### Buchsenleiste (aus Schaltbild 6.2)

```
Obere Reihe (Temperaturfühler):
  AGF  AF   BF   KRF  HK1  ZF   KF
  Abgas Außen Boiler Rückl VL   Zirkul Kessel

Untere Reihe (Eingang/Analog):
  KOF  FB2  PUF  HK2  FB1  0-10  DIGE
  ...  Fern Puffer VL   Fern Analog Digitaleing.
       HK2  unten  HK2  HK1  0-10V  0-5VDC
```

### Schnittstelle 1: Analog 0-10V → HK-1 Sollwert (Par 61)

**Buchse:** `0-10` (untere Reihe, Bezeichnung „Analogeingang 0–10 VDC" = Fernsteller HK1)
**Funktion:** 0V = 0°C, 10V = 100°C Sollwert für HK-1 (Pufferladekreis)
**Werkeinstellung Par 61:** G = freigegeben

Wenn freigegeben: Der externe Wert **überschreibt** den intern eingestellten HK-1-Sollwert vollständig.

**Wir nutzen DAC CH1 (GP8403) für diesen Eingang.** Aktuell: 1.6V = 16°C = faktische Kesselsperre.

| Spannung | Sollwert | Wirkung |
|----------|----------|---------|
| 0.5V | 5°C | Kessel heizt HK-1 nie (Puffer immer wärmer) → Sperre |
| 4.0V | 40°C | Kessel heizt nur wenn Puffer sehr kalt |
| 6.5V | 65°C | Normalbetrieb Winter |
| 8.0V | 80°C | Hochtemperatur Kältetage |

### Schnittstelle 2: Kaskadeneingang 230V → Brenner sperren/freigeben

**Buchse:** `KN` / `KL` (230V Buchsenleiste, ganz links, Aufschrift „Kaskade Brennerstop")
**Referenz:** Abschnitt 6.3 Buchsenleiste 230V~; Par 51 Text explizit „230V~"
**Funktion (Par 51 = G):** 230V anliegend = Kessel freigegeben; 230V weg = Kessel gesperrt
**Hardware:** ESP8266 mit 230V-Relaismodul (z.B. SRD-05VDC-SL-C, 10A/250V AC)

#### Belegung Buchsenleiste 230V~ (Auszug, von links)

```
| KN  KL | BN  BL | Mischer HK2 | Mischer HK1 | Pumpe HK2 | KKP | STB | Netz |
  ↑    ↑    ↑    ↑
  Kaskade  Boiler-
  (Eingang) ladepumpe
            (Ausgang)
```

- **KN** = Kaskade Neutral (Eingang)
- **KL** = Kaskade Live 230V (Eingang)
- **BN / BL** = Boilerladepumpe N/L — das ist ein **Ausgang** der ProControl (schaltet Pumpe), nicht relevant für uns

#### Verdrahtung mit ESP8266-Relais

```
Netz L (von Netz-Klemme L1 der ProControl) ──→ [Relais NO] ──→ KL
Netz N ────────────────────────────────────────────────────→ KN
```

- **NO** (normally open) = Sommer-Default: Kessel gesperrt wenn ESP8266 aus/offline
- ESP8266 zieht Relais an → 230V auf KL/KN → Kessel freigegeben
- L von der ProControl-eigenen Netz-Klemme (L1, ganz rechts) abgreifen — nicht fremde Phase

#### Par 46 — Kessel abkühlen (Werkeinstellung: G)

Zwei unabhängige Auslöser:

1. **Kaskadensperre aktiv** → Kessel darf auch bei RL < Par3 (56°C) nicht starten → kühlt aus
2. **Alle Sollwerte um >3K überschritten** (ext. Wärmeerzeuger, z.B. Heizstab) → Kessel bleibt gesperrt

Bedeutung: Wenn Heizstab Puffer auf 80°C heizt (Boiler-Soll 60°C → 20K drüber), hält Par 46 den Kessel automatisch aus — auch ohne Kaskadeneingang. Das erklärt warum der Kessel tagsüber bei PV-Betrieb still steht.

#### Par 47 — Kask. Kessel Warmst. (Werkeinstellung: G)

Nach Freigabe der Kaskadensperre wird der Kessel über Rücklauf vom warmen Puffer vorgewärmt, bevor er neu startet. Schützt vor Kaltstart und schlechter Kondensation.

### Schnittstelle 3: Digitaleingang 0-5 VDC (Par 67)

**Buchse:** `DIGE` (untere Reihe)
**Funktion:** Kondensatpumpen-Steuerung — für unsere Zwecke nicht relevant.

---

## Energiegefälle und Wechselwirkungen

```
Heizstab (PV-Überschuss)
  → Puffer oben auf 80°C
  → Boiler-Sollwert (60°C) weit überschritten
  → Kessel macht nichts (kein Bedarf)
  → Kein Öl verbraucht ✓

Kessel (wenn aktiv):
  → Brenner an bei RL < 56°C
  → Heizt bis RL = 67°C (11K Hysterese)
  → Vollbrennwert: Abgas 32.5°C, kondensiert → hoher Wirkungsgrad
  → Rücklauf ≥ 60°C nötig (Korrosionsschutz Stahl-WT, Par 1: 53°C Mindest-RL)
  → Problem: niedriger Rücklauf schadet dem Kessel langfristig

HK-2 Mischer:
  → Mischt heißes Pufferwasser mit Rücklauf
  → Witterungsgeführt: kalt draußen → hoher VL-Soll
  → Verhindert dass 80°C direkt zu den Heizkörpern gehen
```

**Wichtige Nebenbedingung:** Der Kessel braucht Rücklauf ≥ 53°C (Par 1) damit Verbraucher nicht abschalten. Bei Niedertemperatur-Betrieb (z.B. niedrigen HK-1-Sollwert über 0-10V) besteht das Risiko zu niedriger Rücklauftemperaturen. Dieser Schutz ist in der Steuerung integriert — aber zu berücksichtigen.

---

## Optimierungsstrategie (Phasen)

### Phase 1 — Sofort: 0-10V dynamisch steuern (kein Löten nötig)

**Ziel:** Kessel nur zuschalten wenn Heizstab nicht ausreicht.

Regellogik im heatingrod-Controller:

```
Sommer (Außen > 18°C):
  CH1 = 0.5V (5°C) → Kessel heizt Puffer nie nach

Übergang (10–18°C Außen):
  Puffer oben > 60°C → CH1 = 0.5V (Heizstab reicht)
  Puffer oben < 55°C → CH1 = 6.0V (60°C, Kessel hilft)

Winter (< 10°C Außen):
  CH1 = f(Außentemperatur) → z.B. 7.0V bei -5°C
  Bei PV-Überschuss aktiv: CH1 um 1-2V reduzieren

Legionellenschutz (montags):
  CH1 = 6.5V mindestens → Kessel darf auf 65°C heizen
```

### Phase 2 — Kaskadeneingang (230V-Relais)

**Ziel:** Kessel komplett sperren im Sommerbetrieb.

Einfacheres Interface als 0-10V, aber gröber (nur ein/aus). Sinnvoll als zusätzliche Absicherung wenn 0-10V-Steuerung nicht ausreicht.

### Phase 3 — Nach Firmware-Analyse (ProControl RE)

- Alle internen Parameter live lesen (Brennerstarts, Betriebsstunden, Temperaturen)
- Vollständige HA-Integration via ESP8266 an ISP-Header
- Optimierung der Heizkurve HK-2 basierend auf Wettervorhersage

---

## Offene Fragen (zu klären)

1. **Par 61 Status:** Ist 0-10V tatsächlich freigegeben? Prüfung: Menue → Einstellungen → Service → Konfiguration → Par 61 = G?
2. **Kaskadeneingang:** Ist KN/KL aktuell verkabelt oder nicht?
3. **BF-Fühler:** Sitzt der am Puffer oben (= identisch mit unserem `sensor.speicher_oben_temperatur`)? Oder separate Position?
4. **Außenfühler HK-1 (Par 41):** Gesperrt oder freigegeben? Wenn gesperrt → HK-1 läuft auf konstantem Sollwert (den wir über 0-10V vorgeben). Wenn freigegeben → witterungsgeführt, 0-10V als Parallelverschiebung.
5. **Fernsteller1 −3°C:** Interner Drehknopf (Par 45) oder externer Raumfernsteller (FB1-Buchse)?
6. **Legionellenschutz (Par 59):** Ist er freigegeben? Wenn ja, überschreibt er montags unsere 0-10V-Vorgabe automatisch (gut so).
