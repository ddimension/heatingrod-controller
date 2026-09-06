# 1-Wire Direct Integration Plan

## Aktueller Zustand
- **USB Adapter:** Dallas DS1490F (DS9490R kompatibel) an USB Bus 001 Device 006
- **12 DS18B20 Sensoren** auf dem Bus
- **owserver** als Middleware (Port 4304), HA liest via owserver
- **Probleme:** Timeouts, fehlende Werte, owserver als Single-Point-of-Failure

## Timing-Messung (12 Sensoren)
- Einzelner Sensor Read: ~680ms (DS18B20 12-bit Konvertierung)
- Sequentieller Durchlauf: ~8.2s für alle 12
- Gecachte Reads: 2-3ms
- 1 langsamer Sensor (28.27A7BC): 1036ms (Kabelqualität?)

## Optimale Strategie: Simultaneous Conversion + Bulk Read

### DS18B20 Simultaneous Conversion
Der DS18B20 unterstützt "Skip ROM + Convert T" — ein einziger Bus-Befehl triggert
ALLE Sensoren gleichzeitig zur Temperaturkonvertierung. Danach liest man nur die
fertigen Scratchpads (9 Bytes pro Sensor, ~1ms).

Ablauf:
1. Reset Pulse → Presence
2. Skip ROM (0xCC) → Convert T (0x44) → warten 750ms (12-bit)
3. Für jeden Sensor: Reset → Match ROM (0x55) + 8 Byte ID → Read Scratchpad (0xBE) → 9 Bytes

**Gesamtzeit: ~750ms + 12 × ~5ms = ~810ms** (statt 8.2s sequentiell)

### Implementierungsoptionen

#### Option A: pyownet über owserver (aktuell)
- Pro: Funktioniert, HA-kompatibel
- Contra: owserver Overhead, Caching-Probleme, Timeouts
- Simultaneous: `ow.write('/simultaneous/temperature', b'1')` + uncached reads
- **Empfehlung: Kurzfristig, owserver-Tuning**

#### Option B: Direkt USB via python-usb (libusb)
- Pro: Kein owserver, volle Kontrolle, event-basiert möglich
- Contra: Low-level DS2490 USB-Protokoll implementieren
- DS2490 Chip: USB → 1-Wire Bridge, eigenes Command-Set
- **Empfehlung: Mittelfristig, wenn owserver nicht reicht**

#### Option C: pyownet ohne owserver Cache
- Wie A, aber owserver mit `--uncached` + `--timeout_volatile=3`
- owserver Restart mit optimierten Parametern
- **Empfehlung: Sofort umsetzbar**

### Empfehlung: Option C + eigener Poller

1. owserver mit optimierten Parametern neustarten:
   ```
   owserver --foreground --usb=all --timeout_volatile=3 --timeout_usb=2
   ```

2. Eigener 1-Wire Poller im Controller (wie DS100Poller):
   - `pyownet.protocol.proxy` Verbindung
   - Simultaneous Conversion triggern
   - 750ms warten
   - Alle 12 Sensoren aus `/uncached/` lesen
   - Fehlerhafte Reads wiederholen (max 1x)
   - Werte an ESPHome Server + MQTT pushen
   - Intervall: alle 10s (reicht für Temperaturen)

3. Sensoren mappen:
   | 1-Wire ID | Name | HA Entity |
   |-----------|------|-----------|
   | 28.FF110F721603 | Speicher oben | sensor.speicher_oben_temperatur |
   | 28.FF7296711605 | Speicher VL | sensor.28_ff7296711605_temperatur |
   | 28.FFFC03711603 | Heizstab | sensor.heizstab_temperatur |
   | ... | ... | ... |

4. HA 1-Wire Integration entfernen → Controller liefert via ESPHome API
