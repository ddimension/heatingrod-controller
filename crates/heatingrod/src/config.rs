//! Config loading — mirrors `heatingrod/config.py`.
//!
//! Full schema: esphome, api, dac, ha_entities, control, calibration, devices,
//! sensor, binary_sensor, number, climate, powerlogger, kaskade, ds100,
//! onewire, canbus. serde ignores unknown keys like the Python loader does
//! (the production config contains sections we only consume in later phases).

use std::collections::BTreeMap;
use std::path::Path;

use serde::Deserialize;

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(default)]
pub struct Config {
    pub esphome: EsphomeSection,
    pub api: ApiSection,
    pub dac: DacSection,
    pub ha_entities: HaEntitiesSection,
    pub control: ControlSection,
    pub calibration: CalibrationSection,
    pub devices: Vec<DeviceDef>,
    pub sensor: Vec<SensorDef>,
    pub binary_sensor: Vec<BinarySensorDef>,
    pub number: Vec<NumberDef>,
    pub climate: Vec<ClimateDef>,
    pub powerlogger: PowerloggerSection,
    pub kaskade: KaskadeSection,
    pub ds100: Ds100Section,
    pub onewire: OnewireSection,
    pub canbus: CanbusSection,
}

impl Config {
    pub fn load(path: &Path) -> Result<Self, String> {
        let raw = std::fs::read_to_string(path)
            .map_err(|e| format!("cannot read config {}: {e}", path.display()))?;
        let config: Config = serde_yaml::from_str(&raw)
            .map_err(|e| format!("cannot parse config {}: {e}", path.display()))?;
        Ok(config)
    }

    /// MAC: explicit `mac_address` wins; otherwise read the interface MAC from
    /// sysfs (`mac_interface`, e.g. ethx). Always uppercased (HA relies on it).
    pub fn resolve_mac(&self) -> Result<String, String> {
        if !self.esphome.mac_address.is_empty() {
            return Ok(self.esphome.mac_address.to_uppercase());
        }
        if let Some(iface) = &self.esphome.mac_interface {
            let path = format!("/sys/class/net/{iface}/address");
            let mac = std::fs::read_to_string(&path)
                .map_err(|e| format!("cannot read {path}: {e}"))?
                .trim()
                .to_uppercase();
            if mac.len() != 17 || !mac.bytes().all(|b| b.is_ascii_hexdigit() || b == b':') {
                return Err(format!("invalid MAC from {path}: {mac}"));
            }
            tracing::info!("MAC from interface {iface}: {mac}");
            return Ok(mac);
        }
        Err("config: neither esphome.mac_address nor esphome.mac_interface set".into())
    }
}

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(default)]
pub struct EsphomeSection {
    pub name: String,
    pub friendly_name: String,
    pub comment: String,
    pub mac_address: String,
    pub mac_interface: Option<String>,
    pub project: ProjectSection,
}

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(default)]
pub struct ProjectSection {
    pub name: String,
    pub version: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct ApiSection {
    pub port: u16,
    pub bind_address: String,
    pub encryption: EncryptionSection,
    pub allow_plaintext: bool,
}

impl Default for ApiSection {
    fn default() -> Self {
        Self {
            port: 6053,
            bind_address: "0.0.0.0".into(),
            encryption: EncryptionSection::default(),
            allow_plaintext: false,
        }
    }
}

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(default)]
pub struct EncryptionSection {
    pub key: String,
}

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(default)]
pub struct DacSection {
    pub i2c_address: String, // "0x5f" — parsed in Phase 4
    pub mock: bool,
    pub max_voltage: f64,
    pub ch1_safe_celsius: f64,
}

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(default)]
pub struct HaEntitiesSection {
    pub grid_power: String,
    pub heatingrod_power: String,
    pub heatingrod_temperature: String,
    pub tank_top_temperature: String,
    pub pv_dc_power: String,
    pub export_power_raw: String,
    pub balkonpv_power: String,
    pub heatingrod_switch: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct ControlSection {
    pub mode: String,
    pub surplus_override_watts: f64, // >0: bypass grid sensor (test only)
    pub power_limit_start: f64,
    pub power_limit_stop: f64,
    pub max_power_watts: f64,
    pub power_use_factor: f64,
    pub tank_temp_limit: f64,
    pub sensor_max_age_seconds: f64,
    pub watchdog_interval: f64,
    pub feedback_tolerance_watts: f64,
    pub feedback_damping: f64,
    pub max_voltage_step: f64,
    pub step_interval: f64,
    pub step_initial_voltage: f64,
    pub step_ramp_rate: f64,
    pub step_ramp_target: f64,
    pub step_settle_time: f64,
    pub internal_cutoff_voltage_threshold: f64,
    pub internal_cutoff_power_threshold: f64,
    pub internal_cutoff_duration: f64,
    pub internal_cutoff_recovery_temp: f64,
}

impl Default for ControlSection {
    fn default() -> Self {
        Self {
            mode: String::new(),
            surplus_override_watts: 0.0,
            power_limit_start: 0.0,
            power_limit_stop: 0.0,
            max_power_watts: 0.0,
            power_use_factor: 0.0,
            tank_temp_limit: 0.0,
            sensor_max_age_seconds: 0.0,
            watchdog_interval: 0.0,
            feedback_tolerance_watts: 0.0,
            feedback_damping: 0.0,
            max_voltage_step: 0.0,
            step_interval: 0.1,
            step_initial_voltage: 1.0,
            step_ramp_rate: 5.0,
            step_ramp_target: 0.8,
            step_settle_time: 7.0,
            internal_cutoff_voltage_threshold: 1.0,
            internal_cutoff_power_threshold: 200.0,
            internal_cutoff_duration: 60.0,
            internal_cutoff_recovery_temp: 60.0,
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct CalibrationSection {
    pub file_path: String,
    pub steps: u32,
    pub step_duration: f64,
    pub min_power_for_calibration: f64,
    pub max_age_days: u64,
    pub deviation_threshold: f64,
    pub ewma_alpha: f64,
    pub min_sweep_voltage: f64,
}

impl Default for CalibrationSection {
    fn default() -> Self {
        Self {
            file_path: "calibration.json".into(),
            steps: 6,
            step_duration: 30.0,
            min_power_for_calibration: 2000.0,
            max_age_days: 7,
            deviation_threshold: 0.15,
            ewma_alpha: 0.3,
            min_sweep_voltage: 4.0,
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct DeviceDef {
    pub id: u32,
    pub name: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct SensorDef {
    pub platform: String,
    pub name: String,
    pub object_id: String,
    pub unit_of_measurement: String,
    pub accuracy_decimals: i32,
    pub device_class: String,
    /// String like Python: "measurement"/"total_increasing"; mapped to the
    /// wire int by [`state_class_int`].
    pub state_class: String,
    pub icon: String,
    pub device_id: u32,
    /// "text_sensor" → TextSensorEntity (Python esphome_bridge behavior).
    pub entity_type: Option<String>,
    /// platform `controller`: internal field name.
    pub field: String,
    /// platform `sorel`: CAN topic.
    pub topic: String,
    /// platform `onewire`: sensor address.
    pub address: String,
}

impl Default for SensorDef {
    fn default() -> Self {
        Self {
            platform: String::new(),
            name: String::new(),
            object_id: String::new(),
            unit_of_measurement: String::new(),
            accuracy_decimals: 0,
            device_class: String::new(),
            state_class: "measurement".into(),
            icon: String::new(),
            device_id: 0,
            entity_type: None,
            field: String::new(),
            topic: String::new(),
            address: String::new(),
        }
    }
}

/// Wire mapping from esphome_bridge.py:
/// `state_class_map = {"measurement": 1, "total_increasing": 2}`, else 0.
pub fn state_class_int(name: &str) -> i32 {
    match name {
        "measurement" => 1,
        "total_increasing" => 2,
        _ => 0,
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct BinarySensorDef {
    pub platform: String,
    pub name: String,
    pub object_id: String,
    pub topic: String,
    pub device_class: String,
    pub icon: String,
    pub device_id: u32,
}

impl Default for BinarySensorDef {
    fn default() -> Self {
        Self {
            platform: String::new(),
            name: String::new(),
            object_id: String::new(),
            topic: String::new(),
            device_class: String::new(),
            icon: String::new(),
            device_id: 0,
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct NumberDef {
    pub name: String,
    pub object_id: String,
    pub min_value: f32,
    pub max_value: f32,
    pub step: f32,
    pub unit_of_measurement: String,
    pub mode: i32,
    pub device_id: u32,
}

impl Default for NumberDef {
    fn default() -> Self {
        Self {
            name: String::new(),
            object_id: String::new(),
            min_value: 0.0,
            max_value: 100.0,
            step: 1.0,
            unit_of_measurement: String::new(),
            mode: 2, // NUMBER_MODE_SLIDER
            device_id: 0,
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct ClimateDef {
    pub platform: String,
    pub name: String,
    pub object_id: String,
    pub channel: u8,
    pub device_id: u32,
    pub current_temperature_entity: String,
    pub default_target_temperature: f32,
    pub visual_min_temperature: f32,
    pub visual_max_temperature: f32,
    pub visual_temperature_step: f32,
    pub icon: String,
    /// YAML quirk: bare `off`/`on` parse as booleans (production config has
    /// `supported_modes: [false, heat]`). Parsed as raw values; mapped via
    /// [`mode_int`].
    pub supported_modes: Vec<serde_yaml::Value>,
}

impl Default for ClimateDef {
    fn default() -> Self {
        Self {
            platform: String::new(),
            name: String::new(),
            object_id: String::new(),
            channel: 1,
            device_id: 0,
            current_temperature_entity: String::new(),
            default_target_temperature: 60.0,
            visual_min_temperature: 0.0,
            visual_max_temperature: 100.0,
            visual_temperature_step: 1.0,
            icon: String::new(),
            supported_modes: vec![],
        }
    }
}

/// Climate mode mapping (Python esphome_bridge): off=0, heat_cool=1, cool=2,
/// heat=3, fan_only=4, auto=6. Bare YAML booleans: false→"off", true→"on".
pub fn mode_int(value: &serde_yaml::Value) -> Option<i32> {
    let name = match value {
        serde_yaml::Value::Bool(b) => {
            if *b {
                "on"
            } else {
                "off"
            }
        }
        serde_yaml::Value::String(s) => s.as_str(),
        _ => return None,
    };
    match name {
        "off" => Some(0),
        "heat_cool" => Some(1),
        "cool" => Some(2),
        "heat" => Some(3),
        "fan_only" => Some(4),
        "auto" => Some(6),
        _ => None,
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct PowerloggerSection {
    pub host: String,
    pub port: u16,
    pub noise_psk: String,
    pub password: String,
    pub entity_object_id: String,
    pub reconnect_delay: f64,
}

impl Default for PowerloggerSection {
    fn default() -> Self {
        Self {
            host: String::new(),
            port: 6053,
            noise_psk: String::new(),
            password: String::new(),
            entity_object_id: "aktueller_verbrauch".into(),
            reconnect_delay: 5.0,
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct KaskadeSection {
    pub enabled: bool,
    pub host: String,
    pub port: u16,
    pub noise_psk: String,
    pub switch_object_id: String,
    pub reconnect_delay: f64,
}

impl Default for KaskadeSection {
    fn default() -> Self {
        Self {
            enabled: false,
            host: String::new(),
            port: 6053,
            noise_psk: String::new(),
            switch_object_id: "pro_condens_brennersperre".into(),
            reconnect_delay: 5.0,
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct Ds100Section {
    pub enabled: bool,
    pub serial_port: String,
    pub unit_id: u8,
    pub target_baud: u32,
    pub timeout: f64,
    pub poll_interval_fast: f64,
    pub poll_interval_full: f64,
    pub poll_interval_demand: f64,
    pub reconnect_delay: f64,
}

impl Default for Ds100Section {
    fn default() -> Self {
        Self {
            enabled: true,
            serial_port: String::new(),
            unit_id: 1,
            target_baud: 115200,
            timeout: 0.5,
            poll_interval_fast: 0.1,
            poll_interval_full: 30.0,
            poll_interval_demand: 300.0,
            reconnect_delay: 5.0,
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct OnewireSection {
    pub enabled: bool,
    pub poll_interval: u64,
    pub change_threshold: f64,
    pub reconnect_delay: f64,
    pub sensor_names: BTreeMap<String, String>,
}

impl Default for OnewireSection {
    fn default() -> Self {
        Self {
            enabled: true, // Python OnewireConfig.enabled default
            poll_interval: 10,
            change_threshold: 0.1,
            reconnect_delay: 5.0,
            sensor_names: BTreeMap::new(),
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct CanbusSection {
    pub enabled: bool,
    pub serial_port: String,
    pub bitrate: u32,
    pub tty_baudrate: u32,
    pub reconnect_delay: f64,
    pub unknown_buffer_size: usize,
    pub sensor_names: BTreeMap<u32, String>,
}

impl Default for CanbusSection {
    fn default() -> Self {
        Self {
            enabled: true,
            serial_port: String::new(),
            bitrate: 250000,
            tty_baudrate: 115200,
            reconnect_delay: 5.0,
            unknown_buffer_size: 1000,
            sensor_names: BTreeMap::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_minimal_sections() {
        let yaml = r#"
esphome:
  name: heatingrod-test
  mac_address: "b8:27:eb:96:ae:01"
  project:
    name: custom.heatingrod
    version: "2.0.0"
api:
  port: 6054
  bind_address: 192.168.203.199
  encryption:
    key: "AAECAwQFBgcICQoLDA0ODxAREhMUFRYXGBkaGxwdHh8="
devices:
  - id: 1
    name: "DS100 Energiezähler"
sensor:
  - platform: controller
    name: Uptime
    object_id: rust_uptime
    field: uptime
    unit_of_measurement: s
number:
  - name: Test Limit
    object_id: rust_limit
climate:
  - platform: dac
    name: Test Heizkessel
    object_id: rust_test_klima
"#;
        let c: Config = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(c.api.port, 6054);
        assert_eq!(c.api.bind_address, "192.168.203.199");
        assert_eq!(c.devices.len(), 1);
        assert_eq!(c.sensor[0].field, "uptime");
        assert_eq!(c.number[0].object_id, "rust_limit");
        assert_eq!(c.climate[0].channel, 1);
    }

    #[test]
    fn unknown_keys_are_ignored() {
        let yaml = "esphome:\n  name: x\ncontrol:\n  mode: calibration\n";
        let c: Config = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(c.esphome.name, "x");
    }

    #[test]
    fn resolve_mac_prefers_explicit_uppercase() {
        let mut c = Config::default();
        c.esphome.mac_address = "b8:27:eb:96:ae:00".into();
        c.esphome.mac_interface = Some("eth0".into());
        assert_eq!(c.resolve_mac().unwrap(), "B8:27:EB:96:AE:00");
    }

    #[test]
    fn climate_supported_modes_yaml_bool_quirk() {
        // Production config literally contains `supported_modes: [false, heat]`.
        let yaml = r#"
climate:
  - platform: dac
    name: Heizkessel HK1
    supported_modes:
      - false
      - heat
"#;
        let c: Config = serde_yaml::from_str(yaml).unwrap();
        let modes: Vec<i32> = c.climate[0]
            .supported_modes
            .iter()
            .filter_map(mode_int)
            .collect();
        assert_eq!(modes, vec![0, 3]); // off, heat
    }

    #[test]
    fn parses_full_production_shape() {
        // Representative slice of the production config (all sections).
        let yaml = r#"
esphome:
  name: heatingrod
  friendly_name: Heizstab Controller
  mac_address: B8:27:EB:96:AE:00
api:
  port: 6053
  encryption:
    key: x
dac:
  i2c_address: 0x5f
  mock: false
  max_voltage: 10.0
ha_entities:
  grid_power: sensor.powerlogger_aktueller_verbrauch
control:
  mode: calibration
  tank_temp_limit: 85.0
calibration:
  steps: 6
  deviation_threshold: 0.30
devices:
  - id: 5
    name: ProControl 3
sensor:
  - platform: ds100
    name: DS100 Wirkleistung
    object_id: ds100_power
    field: power_combined
    unit_of_measurement: W
    device_class: power
    device_id: 1
  - platform: onewire
    name: 1-Wire Speicher oben
    object_id: 1w_speicher_oben_temp
    address: 28.FF110F721603
    device_id: 4
binary_sensor:
  - platform: sorel
    name: SOREL Hauptpumpe (R0)
    object_id: sorel_pumpe_r0
    topic: ltdc/DLG_RELAY/0/switched
    device_class: running
    device_id: 3
climate:
  - platform: dac
    name: Heizkessel HK1
    object_id: heizkessel_hk1
    device_id: 5
    channel: 1
    supported_modes:
      - false
      - heat
powerlogger:
  host: powerlogger.kalnet.hooya.de
kaskade:
  enabled: true
  host: esp-heizung-ksk.kalnet.hooya.de
ds100:
  serial_port: /dev/serial/by-id/usb-FTDI_USB_Serial_Converter_FTB6SPL3-if00-port0
onewire:
  sensor_names:
    28.FF110F721603: Speicher oben
canbus:
  serial_port: /dev/serial/by-id/usb-Microchip_Technology__Inc._USBtin_A0215B51-if00
  sensor_names:
    0: Kaltwasser
"#;
        let c: Config = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(c.esphome.name, "heatingrod");
        assert_eq!(c.dac.i2c_address, "0x5f");
        assert_eq!(c.control.tank_temp_limit, 85.0);
        assert_eq!(c.calibration.deviation_threshold, 0.30);
        assert_eq!(c.devices[0].id, 5);
        assert_eq!(c.sensor[0].platform, "ds100");
        assert_eq!(c.sensor[1].address, "28.FF110F721603");
        assert_eq!(c.binary_sensor[0].topic, "ltdc/DLG_RELAY/0/switched");
        assert_eq!(c.climate[0].device_id, 5);
        assert_eq!(c.ha_entities.grid_power, "sensor.powerlogger_aktueller_verbrauch");
        assert_eq!(c.onewire.sensor_names.get("28.FF110F721603").unwrap(), "Speicher oben");
        assert_eq!(c.canbus.sensor_names.get(&0).unwrap(), "Kaltwasser");
    }
}
