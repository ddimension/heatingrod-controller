# Migration: MQTT/HA → ESPHome Native API (komplett)

## Entity-ID Mapping (Alt → Neu)

### DS100 Heizstab (3 Entities)
| Alt (MQTT Discovery / HA Modbus) | Neu (ESPHome) | InfluxDB Punkte |
|---|---|---|
| `sensor.heizstab_leistung` | `sensor.ds100_power` | ~115.000 |
| `sensor.heizstab_energie` | `sensor.ds100_energy_total` | ~96.000 |
| `sensor.heizstab_level` | `sensor.ctrl_dac_level` | ~129.000 |

### 1-Wire Temperaturen (8 Entities)
| Alt (HA owserver) | Neu (ESPHome) | 1-Wire Adresse |
|---|---|---|
| `sensor.speicher_oben_temperatur` | `sensor.temp_speicher_oben` | 28.FF110F721603 |
| `sensor.28_ff7296711605_temperatur` | `sensor.temp_speicher_vorlauf` | 28.FF7296711605 |
| `sensor.heizstab_temperatur` | `sensor.temp_heizstab` | 28.FFFC03711603 |
| `sensor.speicher_rucklauf_temperatur` | `sensor.temp_speicher_ruecklauf` | 28.FFD9DF711604 |
| `sensor.speicher_heizung_vorlauf_temperatur` | `sensor.temp_heizung_vorlauf` | 28.FFDD82711605 |
| `sensor.speicher_heizung_rucklauf_temperatur` | `sensor.temp_heizung_ruecklauf` | 28.FF9B40721603 |
| `sensor.keller_speicher_temperatur` | `sensor.temp_keller_speicher` | 28.FFE0DB701603 |
| `sensor.28_6a01bd000000_temperatur` | `sensor.temp_keller` (?) | 28.FF6E15721603 |

### Nicht betroffen (bleiben in HA)
- Alle `sorel_*` Entities — MQTT Discovery (vorerst beibehalten)
- Wechselrichter/PV: `import_power`, `export_power`, `total_dc_power`
- Brenner: `schaltaktor_heizung_brenner_leistung`, `nutzwasser_leistung`
- Heizöl: `heizoel_level`, `heizoelstand_liter`
- Pool-Temperaturen: `pool_oben`, `pool_unten`, `pool_steuerung`

## Betroffene Systeme

### 1. InfluxDB (homeassistant Bucket)
- **URL:** http://influxdb.kalnet.hooya.de:8086
- **Write-Token:** `kFLze5cU-QCs1q29UgJC78qwme-RI84pXyaPy4MP_qy-Wp86IjNipNFVqKucm2pSmFsyunazmm_dzP4GJLHYnw==`
- **Read-Token:** `G_FpCAkKbNIOjWJfbc0eflt3rOgn3ERjsLdnhWnDldQJmaVWmr-k4PcAHbqq3lFP6RsvlG3v05J_xV87Jvvrgg==`
- **Aktion:** Entity-ID Tags in historischen Daten umtaggen (alt → neu)
- **~340k Datenpunkte** für DS100 Entities
- **~XXk Datenpunkte** für 1-Wire Entities (jahrelange Historie!)

### 2. Grafana Dashboards — Projekt-lokal (grafana/)
| Dashboard | Betroffene Entities |
|---|---|
| heizung.json | heizstab_leistung, heizstab_energie, heizstab_level, heizstab_temperatur, speicher_oben, speicher_rucklauf, heizung_VL/RL, keller_speicher |
| ds100.json | heatingrod_ctrl_level → ctrl_dac_level |
| sorel.json | keine |

### 3. Grafana Dashboards — Kubernetes (proxmox-talos-opentofu-v2)
| Dashboard | Betroffene Entities |
|---|---|
| heizung.json | heizstab_leistung, heizstab_energie, heizstab_level, heizstab_temperatur, alle Speicher-Temps |
| energy.json | heizstab_energie |
| energie-effizienz.json | heizstab_energie (5x!), speicher_oben (3x), heizung_VL/RL, keller_speicher |

### 4. HA Dashboards (Lovelace)
- Default Dashboard: heizstab_leistung (2x), heizstab_energie (1x), heizstab_temperatur (2x)
- Keine Custom Dashboards betroffen

### 5. HA Automations
- **Keine** Automations referenzieren betroffene Entities direkt
- **Keine** pyscript Skripte vorhanden

### 6. Climatebox Projekt
| Datei | Betroffene Entities |
|---|---|
| data_collect.py | `sensor.28_27b1bc000000_temperatur`, `sensor.28_6a01bd000000_temperatur` |
| data_export_influx.py | `28_27b1bc000000_temperatur`, `28_6a01bd000000_temperatur` |

**Hinweis:** Das sind 2 der 4 unbenannten 1-Wire Sensoren! Sie werden in der Climatebox als `temp_dallas_1` und `temp_dallas_2` genutzt. Bei Umbenennung müssen die Climatebox-Scripts angepasst werden.

### 7. Helm Charts (Kubernetes)
- `local_objects.tf`: InfluxDB Datasource-Config (Token, URL) — keine Entity-Änderung nötig
- Grafana Dashboards werden via ConfigMap deployt — müssen aktualisiert werden

## Migrationsschritte (Reihenfolge)

1. **InfluxDB: Daten kopieren** — alt → neu entity_id Tags
2. **Grafana lokal: Dashboards anpassen** — entity_id ersetzen
3. **Grafana k8s: Dashboards anpassen** — entity_id ersetzen
4. **HA Lovelace: Dashboard anpassen** — entity_id ersetzen
5. **Climatebox: Scripts anpassen** — entity_id ersetzen
6. **ESPHome in HA einrichten** — heizung.kalnet.hooya.de:6053 hinzufügen
7. **HA Modbus Config entfernen** — DS100 aus configuration.yaml
8. **HA 1-Wire Integration entfernen** — owserver nicht mehr von HA genutzt
9. **MQTT Discovery deaktivieren** — DS100 Entities aus ha_discovery.py
10. **Verifizieren** — alle Dashboards, Automations, Climatebox funktional
