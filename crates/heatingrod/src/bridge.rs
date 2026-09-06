//! Entity registration + dispatch — mirrors `heatingrod/esphome_bridge.py`.
//!
//! Registration is platform-independent (like Python: the platform only
//! decides which update path feeds the entity). Dispatch: ds100 (field),
//! controller (field), sorel (topic), onewire (address + auto-registration),
//! binary sensors, climates, HA-state → climate current_temperature.

use std::collections::HashMap;
use std::sync::Arc;

use esphome_api_server::{
    ApiServer, BinarySensorEntity, ClimateEntity, NumberEntity, SensorEntity, SensorEntry,
    SensorValue, ServerConfig, TextSensorEntity,
};
use tokio::sync::mpsc;

use crate::config::{mode_int, state_class_int, ClimateDef, Config};

/// SOREL topic payload kind (numeric sensor vs relay binary sensor).
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum TopicKind {
    Numeric,
    Binary,
}

/// Climate entities with their command handling context.
#[derive(Debug, Clone)]
pub struct ClimateBinding {
    #[allow(dead_code)] // used from Phase 3 dispatch on
    pub key: u32,
    pub object_id: String,
    /// Current target temperature in °C (stored; survives commands without target).
    pub target_temperature: f64,
    /// Current mode (mirrors the entity state, used for HA-temp updates).
    pub mode: i32,
}

pub struct Bridge {
    pub server: Arc<ApiServer>,
    /// object_id → sensor key (Python `_sensors` dict; push by object_id).
    pub sensor_by_object_id: HashMap<String, u32>,
    /// controller field name → sensor keys (one field can feed several sensors).
    field_keys: HashMap<String, Vec<u32>>,
    /// ds100 field name → sensor keys (platform ds100 dispatch).
    ds100_field_keys: HashMap<String, Vec<u32>>,
    /// sorel topic → sensor key + kind (sensors AND binary sensors, like
    /// Python's shared `_sensors` dict).
    topic_keys: HashMap<String, (u32, TopicKind)>,
    /// 1-Wire address (lowercase, dots) → sensor key.
    address_keys: HashMap<String, u32>,
    /// climate key → binding.
    climates: HashMap<u32, ClimateBinding>,
    /// HA entity_id → climate key, for current_temperature forwarding.
    climate_temp_map: HashMap<String, u32>,
    /// 1-Wire sub-device id (device whose name contains "1-wire"/"1_wire").
    onewire_device_id: u32,
}

impl Bridge {
    pub fn build(
        config: &Config,
        command_tx: mpsc::Sender<esphome_api_server::Command>,
        ha_event_tx: mpsc::Sender<esphome_api_server::HaStateEvent>,
    ) -> Result<Self, String> {
        let mac = config.resolve_mac()?;
        let server_config = ServerConfig {
            name: config.esphome.name.clone(),
            friendly_name: config.esphome.friendly_name.clone(),
            mac_address: mac,
            model: String::new(),
            manufacturer: String::new(),
            esphome_version: "2026.3.1".into(),
            project_name: config.esphome.project.name.clone(),
            project_version: config.esphome.project.version.clone(),
            bind_address: config.api.bind_address.clone(),
            port: config.api.port,
            noise_psk: config.api.encryption.key.clone(),
            allow_plaintext: config.api.allow_plaintext,
            devices: config
                .devices
                .iter()
                .map(|d| (d.id, d.name.clone()))
                .collect(),
        };
        let server = Arc::new(ApiServer::new(server_config, command_tx, ha_event_tx));

        let onewire_device_id = config
            .devices
            .iter()
            .find(|d| {
                let n = d.name.to_lowercase();
                n.contains("1-wire") || n.contains("1_wire")
            })
            .map(|d| d.id)
            .unwrap_or(0);

        let mut field_keys: HashMap<String, Vec<u32>> = HashMap::new();
        let mut sensor_by_object_id: HashMap<String, u32> = HashMap::new();
        let mut ds100_field_keys: HashMap<String, Vec<u32>> = HashMap::new();
        let mut topic_keys: HashMap<String, (u32, TopicKind)> = HashMap::new();
        let mut address_keys: HashMap<String, u32> = HashMap::new();

        // Sensors — platform-independent registration, Python esphome_bridge.setup().
        for def in &config.sensor {
            let key = if def.entity_type.as_deref() == Some("text_sensor") {
                let mut e = TextSensorEntity::new(&def.name);
                set_object_id(&mut e.object_id, &mut e.key, &def.object_id);
                e.icon = def.icon.clone();
                e.device_id = def.device_id;
                server.add_sensor(SensorEntry::Text(e))
            } else {
                let mut e = SensorEntity::new(&def.name);
                set_object_id(&mut e.object_id, &mut e.key, &def.object_id);
                e.unit_of_measurement = def.unit_of_measurement.clone();
                e.device_class = def.device_class.clone();
                e.state_class = state_class_int(&def.state_class);
                e.accuracy_decimals = def.accuracy_decimals;
                e.icon = def.icon.clone();
                e.device_id = def.device_id;
                server.add_sensor(SensorEntry::Numeric(e))
            };
            sensor_by_object_id.insert(def.object_id.clone(), key);
            match def.platform.as_str() {
                "controller" if !def.field.is_empty() => {
                    field_keys.entry(def.field.clone()).or_default().push(key);
                }
                "ds100" if !def.field.is_empty() => {
                    ds100_field_keys
                        .entry(def.field.clone())
                        .or_default()
                        .push(key);
                }
                "sorel" if !def.topic.is_empty() => {
                    topic_keys.insert(def.topic.clone(), (key, TopicKind::Numeric));
                }
                "onewire" if !def.address.is_empty() => {
                    address_keys.insert(def.address.to_lowercase(), key);
                }
                _ => {}
            }
        }

        // Binary sensors — same shared dict in Python.
        for def in &config.binary_sensor {
            let mut e = BinarySensorEntity::new(&def.name);
            set_object_id(&mut e.object_id, &mut e.key, &def.object_id);
            e.device_class = def.device_class.clone();
            e.icon = def.icon.clone();
            e.device_id = def.device_id;
            let key = server.add_sensor(SensorEntry::Binary(e));
            if !def.topic.is_empty() {
                topic_keys.insert(def.topic.clone(), (key, TopicKind::Binary));
            }
        }

        for def in &config.number {
            let mut n = NumberEntity::new(&def.name);
            set_object_id(&mut n.object_id, &mut n.key, &def.object_id);
            n.min_value = def.min_value;
            n.max_value = def.max_value;
            n.step = def.step;
            n.unit_of_measurement = def.unit_of_measurement.clone();
            n.mode = def.mode;
            n.device_id = def.device_id;
            let key = server.add_number(n);
            // Seed an initial state so HA does not show "unknown" forever
            // (Python semantics: missing_state until first set_state).
            server.update_number(key, 0.0);
        }

        let mut climates = HashMap::new();
        let mut climate_temp_map = HashMap::new();
        for def in &config.climate {
            let mut c = build_climate(config, def);
            c.target_temperature = def.default_target_temperature as f64;
            let key = server.add_climate(c);
            climates.insert(
                key,
                ClimateBinding {
                    key,
                    object_id: def.object_id.clone(),
                    target_temperature: def.default_target_temperature as f64,
                    mode: 0,
                },
            );
            if !def.current_temperature_entity.is_empty() {
                climate_temp_map.insert(def.current_temperature_entity.clone(), key);
                // Python subscribes the climate's current_temperature_entity
                // FIRST — without this subscription HA never pushes its
                // state and the climate shows no current temperature.
                server.subscribe_ha_entity(&def.current_temperature_entity);
            }
        }

        // HA entity subscriptions — same fixed list as Python.
        let ha = &config.ha_entities;
        for entity_id in [
            &ha.pv_dc_power,
            &ha.export_power_raw,
            &ha.balkonpv_power,
            &ha.heatingrod_switch,
            &ha.grid_power,
            &ha.heatingrod_power,
            &ha.heatingrod_temperature,
            &ha.tank_top_temperature,
        ] {
            if !entity_id.is_empty() {
                server.subscribe_ha_entity(entity_id);
            }
        }

        Ok(Self {
            server,
            sensor_by_object_id,
            field_keys,
            ds100_field_keys,
            topic_keys,
            address_keys,
            climates,
            climate_temp_map,
            onewire_device_id,
        })
    }

    // ---- dispatch paths (mirror esphome_bridge.py) ----

    /// platform `controller`: push a field → sensors bound to it.
    pub fn update_controller_field(&self, field: &str, value: f64) {
        if let Some(keys) = self.field_keys.get(field) {
            for &key in keys {
                self.server.update_sensor(key, SensorValue::Number(value));
            }
        }
    }

    /// platform `ds100`: push one field of a reading (Python update_ds100).
    pub fn update_ds100_field(&self, field: &str, value: f64) {
        if let Some(keys) = self.ds100_field_keys.get(field) {
            for &key in keys {
                self.server.update_sensor(key, SensorValue::Number(value));
            }
        }
    }

    /// Push by object_id (Python `update_sensor`).
    pub fn update_sensor_by_object_id(&self, object_id: &str, value: SensorValue) {
        if let Some(&key) = self.sensor_by_object_id.get(object_id) {
            self.server.update_sensor(key, value);
        }
    }

    /// sorel numeric + binary dispatch (Python `on_canbus_update`).
    /// Relay binary sensors: 0=off, anything >0 = on.
    pub fn on_canbus_update(&self, topic: &str, value: f64) {
        if let Some(&(key, kind)) = self.topic_keys.get(topic) {
            let v = match kind {
                TopicKind::Numeric => SensorValue::Number(value),
                TopicKind::Binary => SensorValue::Bool(value > 0.0),
            };
            self.server.update_sensor(key, v);
        }
    }

    /// 1-Wire update incl. auto-registration (Python `on_onewire_update`).
    pub fn on_onewire_update(&mut self, address: &str, temperature: f64) {
        let addr_lc = address.to_lowercase();
        if let Some(&key) = self.address_keys.get(&addr_lc) {
            self.server.update_sensor(key, SensorValue::Number(temperature));
            return;
        }
        // Auto-register unknown 1-Wire sensors so they appear in HA
        let obj_id = format!("1w_{}", addr_lc.replace('.', "_"));
        let mut e = SensorEntity::new(address);
        // Identity invariant: key = MD5(object_id) — recompute after the
        // override (Python derives the key from object_id in __post_init__;
        // a stale key would create a second HA entity, 04.09.2026 audit).
        set_object_id(&mut e.object_id, &mut e.key, &obj_id);
        e.unit_of_measurement = "°C".into();
        e.device_class = "temperature".into();
        e.state_class = 1;
        e.accuracy_decimals = 1;
        e.icon = "mdi:thermometer".into();
        e.device_id = self.onewire_device_id;
        let key = self.server.add_sensor(SensorEntry::Numeric(e));
        self.address_keys.insert(addr_lc, key);
        tracing::info!("Auto-registered 1-Wire sensor {address} as {key:#x}");
        self.server.update_sensor(key, SensorValue::Number(temperature));
    }

    // ---- climate helpers ----

    pub fn climate_keys(&self) -> Vec<u32> {
        self.climates.keys().copied().collect()
    }

    pub fn climate_binding(&self, key: u32) -> Option<&ClimateBinding> {
        self.climates.get(&key)
    }

    pub fn climate_binding_mut(&mut self, key: u32) -> Option<&mut ClimateBinding> {
        self.climates.get_mut(&key)
    }

    /// HA state push → climate current_temperature (Python `_on_ha_state`).
    pub fn on_ha_state(&self, entity_id: &str, state_str: &str) {
        let Some(&key) = self.climate_temp_map.get(entity_id) else {
            return;
        };
        let Ok(temp) = state_str.parse::<f64>() else {
            return;
        };
        let Some(binding) = self.climates.get(&key) else {
            return;
        };
        self.server.update_climate(
            key,
            binding.mode,
            binding.target_temperature,
            Some(temp),
            None,
        );
    }

    /// Apply a climate command result: update stored mode/target + echo.
    pub fn update_climate_state(
        &mut self,
        key: u32,
        mode: i32,
        target: f64,
        current: Option<f64>,
        action: Option<i32>,
    ) {
        if let Some(b) = self.climates.get_mut(&key) {
            b.mode = mode;
            b.target_temperature = target;
        }
        self.server
            .update_climate(key, mode, target, current, action);
    }

    pub fn sensor_count(&self) -> usize {
        self.field_keys.values().map(Vec::len).sum::<usize>()
            + self.topic_keys.len()
            + self.address_keys.len()
    }
}

/// Set object_id from config (empty → keep name-derived default) and
/// recompute the MD5 key — Python `__post_init__` computes the key AFTER
/// object_id assignment, so a config object_id changes the key.
fn set_object_id(object_id: &mut String, key: &mut u32, configured: &str) {
    if !configured.is_empty() {
        *object_id = configured.to_string();
        *key = esphome_api_server::keys::entity_key(configured);
    }
}

/// Python `ClimateEntity` construction from config (esphome_bridge.setup()).
fn build_climate(config: &Config, def: &ClimateDef) -> ClimateEntity {
    let mut c = ClimateEntity::new(&def.name);
    set_object_id(&mut c.object_id, &mut c.key, &def.object_id);
    c.icon = def.icon.clone();
    // device_id fallback: first device whose name contains "procontrol"
    let dev_id = if def.device_id != 0 {
        def.device_id
    } else {
        config
            .devices
            .iter()
            .find(|d| d.name.to_lowercase().contains("procontrol"))
            .map(|d| d.id)
            .unwrap_or(0)
    };
    c.device_id = dev_id;
    // YAML bool quirk: bare off/on parse as booleans — mode_int handles both.
    let modes: Vec<i32> = def.supported_modes.iter().filter_map(mode_int).collect();
    if !modes.is_empty() {
        c.supported_modes = modes;
    }
    c.visual_min_temperature = def.visual_min_temperature;
    c.visual_max_temperature = def.visual_max_temperature;
    c.visual_temperature_step = def.visual_temperature_step;
    c
}

#[cfg(test)]
mod tests {
    use super::*;
    use esphome_api_server::keys::entity_key;

    /// production.yaml is gitignored (contains PSKs) — on fresh clones
    /// the tests fall back to the sanitized production.example.yaml.
    fn production_config_path() -> &'static str {
        const REAL: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../config/production.yaml");
        const EXAMPLE: &str =
            concat!(env!("CARGO_MANIFEST_DIR"), "/../../config/production.example.yaml");
        if std::path::Path::new(REAL).exists() {
            REAL
        } else {
            EXAMPLE
        }
    }

    #[test]
    fn production_tree_builds_with_md5_keys() {
        let config = Config::load(std::path::Path::new(production_config_path()))
            .expect("production config parses");
        let (tx, _rx) = mpsc::channel(16);
        let (htx, _hrx) = mpsc::channel(16);
        let bridge = Bridge::build(&config, tx, htx).expect("bridge builds");

        // Every entity key must equal MD5(object_id) — identity critical.
        let mut object_ids: Vec<String> = config
            .sensor
            .iter()
            .map(|s| s.object_id.clone())
            .chain(config.binary_sensor.iter().map(|b| b.object_id.clone()))
            .chain(config.number.iter().map(|n| n.object_id.clone()))
            .collect();
        for cd in &config.climate {
            // Climate object_id default: lowercase, spaces→underscores
            object_ids.push(if cd.object_id.is_empty() {
                cd.name.to_lowercase().replace(' ', "_")
            } else {
                cd.object_id.clone()
            });
        }
        let registry = bridge.server.registry_snapshot();
        assert_eq!(registry.len(), object_ids.len());
        for oid in &object_ids {
            let key = entity_key(oid);
            assert!(registry.contains(&key), "missing entity {oid} key {key:#x}");
        }
    }

    #[test]
    fn climate_device_id_fallback_procontrol() {
        let mut config = Config::default();
        config.devices.push(crate::config::DeviceDef {
            id: 5,
            name: "ProControl 3".into(),
        });
        let cd = ClimateDef {
            name: "Heizkessel HK1".into(),
            ..Default::default()
        };
        let c = build_climate(&config, &cd);
        assert_eq!(c.device_id, 5);
    }

    /// 04.09.2026 audit I1: the auto-registered 1-Wire key must equal
    /// MD5(object_id) — the stale key (MD5 of the dotted address) would
    /// create a second HA entity vs. Python.
    #[test]
    fn onewire_autoregistration_recomputes_md5_key() {
        let config =
            Config::load(std::path::Path::new(production_config_path())).expect("config");
        let (tx, _rx) = mpsc::channel(16);
        let (htx, _hrx) = mpsc::channel(16);
        let mut bridge = Bridge::build(&config, tx, htx).expect("bridge builds");
        bridge.on_onewire_update("28.AABBCCDDEEFF", 42.0);
        let expected = esphome_api_server::keys::entity_key("1w_28_aabbccddeeff");
        assert!(
            bridge.server.registry_snapshot().contains(&expected),
            "auto-registered key must be MD5(1w_28_aabbccddeeff)"
        );
    }
}
