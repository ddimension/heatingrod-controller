//! Entity types — 1:1 port of `aioesphomeserver/entities.py`.
//!
//! Identity rules (never regress, see CLAUDE.md):
//! - `device_id` is set in BOTH list and state responses, otherwise HA marks
//!   entities unavailable.
//! - Keys come from [`crate::keys::entity_key`] (MD5), stable across restarts.
//! - Climate object_id defaults WITHOUT slash replacement (Python quirk).

use esphome_native_api::parser::ProtoMessage;
use esphome_native_api::proto::{
    BinarySensorStateResponse, ClimateStateResponse, ListEntitiesBinarySensorResponse,
    ListEntitiesClimateResponse, ListEntitiesNumberResponse, ListEntitiesSensorResponse,
    ListEntitiesTextSensorResponse, NumberStateResponse, SensorStateResponse,
    TextSensorStateResponse,
};

use crate::keys::{default_object_id, entity_key};

/// Round-half-to-even with `decimals` digits — matches Python's built-in
/// `round()` (banker's rounding), which the Python server uses in
/// `SensorEntity.set_state`. Rust's `f64::round` rounds half away from zero
/// and would drift 0.1 in HA for *.5 values.
pub fn round_half_even(value: f64, decimals: i32) -> f64 {
    if decimals == 0 {
        // Python: round(x) with ndigits=None/0 returns int — round half to even.
        return round_half_even_no_decimals(value);
    }
    let d = decimals as usize;
    format!("{value:.d$}")
        .parse::<f64>()
        .expect("formatted float parse cannot fail")
}

fn round_half_even_no_decimals(value: f64) -> f64 {
    let floor = value.floor();
    let frac = value - floor;
    if frac < 0.5 || (frac == 0.5 && floor as i64 % 2 == 0) {
        floor
    } else {
        floor + 1.0
    }
}

/// Numeric sensor entity (Python `SensorEntity`).
#[derive(Debug, Clone)]
pub struct SensorEntity {
    pub name: String,
    pub object_id: String,
    pub unit_of_measurement: String,
    pub accuracy_decimals: i32,
    pub device_class: String,
    pub state_class: i32, // 0=none, 1=measurement, 2=total_increasing, 3=total
    pub icon: String,
    pub key: u32,
    pub device_id: u32,
    pub state: f64, // f64 like Python; the wire carries f32 (protobuf float)
    pub missing_state: bool,
}

impl SensorEntity {
    pub fn new(name: &str) -> Self {
        let object_id = default_object_id(name);
        Self {
            name: name.to_string(),
            key: entity_key(&object_id),
            object_id,
            unit_of_measurement: String::new(),
            accuracy_decimals: 0,
            device_class: String::new(),
            state_class: 0,
            icon: String::new(),
            device_id: 0,
            state: 0.0,
            missing_state: true,
        }
    }

    pub fn set_state(&mut self, value: f64) {
        // Python: round(value, accuracy_decimals) if accuracy_decimals else value
        self.state = if self.accuracy_decimals != 0 {
            round_half_even(value, self.accuracy_decimals)
        } else {
            value
        };
        self.missing_state = false;
    }

    #[allow(deprecated)] // legacy_last_reset_type is required by the wire format
    pub fn list_response(&self) -> ProtoMessage {
        ProtoMessage::ListEntitiesSensorResponse(ListEntitiesSensorResponse {
            object_id: self.object_id.clone(),
            key: self.key,
            name: self.name.clone(),
            icon: self.icon.clone(),
            unit_of_measurement: self.unit_of_measurement.clone(),
            accuracy_decimals: self.accuracy_decimals,
            force_update: false,
            device_class: self.device_class.clone(),
            state_class: self.state_class,
            legacy_last_reset_type: 0,
            disabled_by_default: false,
            entity_category: 0,
            device_id: self.device_id,
        })
    }

    pub fn state_response(&self) -> ProtoMessage {
        ProtoMessage::SensorStateResponse(SensorStateResponse {
            key: self.key,
            state: self.state as f32,
            missing_state: self.missing_state,
            device_id: self.device_id,
        })
    }
}

/// Binary sensor entity (Python `BinarySensorEntity`).
#[derive(Debug, Clone)]
pub struct BinarySensorEntity {
    pub name: String,
    pub object_id: String,
    pub device_class: String,
    pub icon: String,
    pub key: u32,
    pub device_id: u32,
    pub state: bool,
    pub missing_state: bool,
}

impl BinarySensorEntity {
    pub fn new(name: &str) -> Self {
        let object_id = default_object_id(name);
        Self {
            name: name.to_string(),
            key: entity_key(&object_id),
            object_id,
            device_class: String::new(),
            icon: String::new(),
            device_id: 0,
            state: false,
            missing_state: true,
        }
    }

    pub fn set_state(&mut self, value: bool) {
        self.state = value;
        self.missing_state = false;
    }

    pub fn list_response(&self) -> ProtoMessage {
        ProtoMessage::ListEntitiesBinarySensorResponse(ListEntitiesBinarySensorResponse {
            object_id: self.object_id.clone(),
            key: self.key,
            name: self.name.clone(),
            device_class: self.device_class.clone(),
            is_status_binary_sensor: false,
            disabled_by_default: false,
            icon: self.icon.clone(),
            entity_category: 0,
            device_id: self.device_id,
        })
    }

    pub fn state_response(&self) -> ProtoMessage {
        ProtoMessage::BinarySensorStateResponse(BinarySensorStateResponse {
            key: self.key,
            state: self.state,
            missing_state: self.missing_state,
            device_id: self.device_id,
        })
    }
}

/// Text sensor entity (Python `TextSensorEntity`).
#[derive(Debug, Clone)]
pub struct TextSensorEntity {
    pub name: String,
    pub object_id: String,
    pub icon: String,
    pub key: u32,
    pub device_id: u32,
    pub state: String,
    pub missing_state: bool,
}

impl TextSensorEntity {
    pub fn new(name: &str) -> Self {
        let object_id = default_object_id(name);
        Self {
            name: name.to_string(),
            key: entity_key(&object_id),
            object_id,
            icon: String::new(),
            device_id: 0,
            state: String::new(),
            missing_state: true,
        }
    }

    pub fn set_state(&mut self, value: &str) {
        self.state = value.to_string();
        self.missing_state = false;
    }

    pub fn list_response(&self) -> ProtoMessage {
        ProtoMessage::ListEntitiesTextSensorResponse(ListEntitiesTextSensorResponse {
            object_id: self.object_id.clone(),
            key: self.key,
            name: self.name.clone(),
            icon: self.icon.clone(),
            disabled_by_default: false,
            entity_category: 0,
            device_class: String::new(),
            device_id: self.device_id,
        })
    }

    pub fn state_response(&self) -> ProtoMessage {
        ProtoMessage::TextSensorStateResponse(TextSensorStateResponse {
            key: self.key,
            state: self.state.clone(),
            missing_state: self.missing_state,
            device_id: self.device_id,
        })
    }
}

/// Climate entity (Python `ClimateEntity`) — writable from HA.
#[derive(Debug, Clone)]
pub struct ClimateEntity {
    pub name: String,
    pub object_id: String,
    pub icon: String,
    pub supported_modes: Vec<i32>, // [3, 0] = HEAT, OFF
    pub visual_min_temperature: f32,
    pub visual_max_temperature: f32,
    pub visual_temperature_step: f32,
    pub key: u32,
    pub device_id: u32,
    pub mode: i32,               // 0=OFF, 3=HEAT
    pub current_temperature: f64,
    pub target_temperature: f64,
    pub action: i32, // 0=OFF, 3=HEATING, 4=IDLE
}

impl ClimateEntity {
    pub fn new(name: &str) -> Self {
        // Python quirk: climate object_id replaces spaces only, NOT slashes.
        let object_id = name.to_lowercase().replace(' ', "_");
        Self {
            name: name.to_string(),
            key: entity_key(&object_id),
            object_id,
            icon: String::new(),
            supported_modes: vec![3, 0],
            visual_min_temperature: 0.0,
            visual_max_temperature: 100.0,
            visual_temperature_step: 1.0,
            device_id: 0,
            mode: 0,
            current_temperature: 0.0,
            target_temperature: 60.0,
            action: 0,
        }
    }

    pub fn set_state(
        &mut self,
        mode: i32,
        target_temperature: f64,
        current_temperature: Option<f64>,
        action: Option<i32>,
    ) {
        self.mode = mode;
        self.target_temperature = target_temperature;
        if let Some(v) = current_temperature {
            self.current_temperature = v;
        }
        if let Some(v) = action {
            self.action = v;
        }
    }

    #[allow(deprecated)] // legacy_supports_away is required by the wire format
    pub fn list_response(&self) -> ProtoMessage {
        ProtoMessage::ListEntitiesClimateResponse(ListEntitiesClimateResponse {
            object_id: self.object_id.clone(),
            key: self.key,
            name: self.name.clone(),
            supports_current_temperature: true,
            supports_two_point_target_temperature: false,
            supported_modes: self.supported_modes.clone(),
            visual_min_temperature: self.visual_min_temperature,
            visual_max_temperature: self.visual_max_temperature,
            visual_target_temperature_step: self.visual_temperature_step,
            legacy_supports_away: false,
            supports_action: true,
            supported_fan_modes: vec![],
            supported_swing_modes: vec![],
            supported_custom_fan_modes: vec![],
            supported_presets: vec![],
            supported_custom_presets: vec![],
            disabled_by_default: false,
            icon: self.icon.clone(),
            entity_category: 0,
            visual_current_temperature_step: self.visual_temperature_step,
            supports_current_humidity: false,
            supports_target_humidity: false,
            visual_min_humidity: 0.0,
            visual_max_humidity: 0.0,
            device_id: self.device_id,
            feature_flags: 0,
            temperature_unit: 0,
        })
    }

    #[allow(deprecated)] // unused_legacy_away is required by the wire format
    pub fn state_response(&self) -> ProtoMessage {
        ProtoMessage::ClimateStateResponse(ClimateStateResponse {
            key: self.key,
            mode: self.mode,
            current_temperature: self.current_temperature as f32,
            target_temperature: self.target_temperature as f32,
            target_temperature_low: 0.0,
            target_temperature_high: 0.0,
            unused_legacy_away: false,
            action: self.action,
            fan_mode: 0,
            swing_mode: 0,
            custom_fan_mode: String::new(),
            preset: 0,
            custom_preset: String::new(),
            current_humidity: 0.0,
            target_humidity: 0.0,
            device_id: self.device_id,
        })
    }
}

/// Number entity (Python `NumberEntity`) — writable from HA.
#[derive(Debug, Clone)]
pub struct NumberEntity {
    pub name: String,
    pub object_id: String,
    pub min_value: f32,
    pub max_value: f32,
    pub step: f32,
    pub unit_of_measurement: String,
    pub icon: String,
    pub mode: i32, // 2 = NUMBER_MODE_SLIDER
    pub key: u32,
    pub device_id: u32,
    pub state: f64,
    pub missing_state: bool,
}

impl NumberEntity {
    pub fn new(name: &str) -> Self {
        let object_id = default_object_id(name);
        Self {
            name: name.to_string(),
            key: entity_key(&object_id),
            object_id,
            min_value: 0.0,
            max_value: 100.0,
            step: 1.0,
            unit_of_measurement: String::new(),
            icon: String::new(),
            mode: 2,
            device_id: 0,
            state: 0.0,
            missing_state: true,
        }
    }

    pub fn set_state(&mut self, value: f64) {
        self.state = value;
        self.missing_state = false;
    }

    pub fn list_response(&self) -> ProtoMessage {
        ProtoMessage::ListEntitiesNumberResponse(ListEntitiesNumberResponse {
            object_id: self.object_id.clone(),
            key: self.key,
            name: self.name.clone(),
            icon: self.icon.clone(),
            min_value: self.min_value,
            max_value: self.max_value,
            step: self.step,
            disabled_by_default: false,
            entity_category: 0,
            unit_of_measurement: self.unit_of_measurement.clone(),
            mode: self.mode,
            device_class: String::new(),
            device_id: self.device_id,
        })
    }

    pub fn state_response(&self) -> ProtoMessage {
        ProtoMessage::NumberStateResponse(NumberStateResponse {
            key: self.key,
            state: self.state as f32,
            missing_state: self.missing_state,
            device_id: self.device_id,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_half_even_matches_python_round() {
        // Python: round(2.5, 1) == 2.5; round(0.25, 1) == 0.2 (banker's)
        assert_eq!(round_half_even(0.25, 1), 0.2);
        assert_eq!(round_half_even(0.35, 1), 0.3);
        assert_eq!(round_half_even(1.5, 0), 2.0);
        assert_eq!(round_half_even(2.5, 0), 2.0);
        assert_eq!(round_half_even(0.5, 0), 0.0);
    }

    #[test]
    fn sensor_defaults_match_python() {
        let e = SensorEntity::new("DS100 Wirkleistung");
        assert_eq!(e.object_id, "ds100_wirkleistung");
        assert_eq!(e.key, entity_key("ds100_wirkleistung"));
        assert!(e.missing_state);
        assert_eq!(e.state_class, 0);
    }

    #[test]
    fn sensor_set_state_rounds_like_python() {
        let mut e = SensorEntity::new("T");
        e.accuracy_decimals = 1;
        e.set_state(22.45);
        // Python round(22.45, 1) = 22.4 (banker's)
        assert_eq!(e.state, 22.4);
        assert!(!e.missing_state);
    }

    #[test]
    fn climate_object_id_has_no_slash_replacement() {
        // Python: climate replaces spaces only; sensors replace '/' too.
        let c = ClimateEntity::new("Heizkessel HK1/Test");
        assert_eq!(c.object_id, "heizkessel_hk1/test");
        let s = SensorEntity::new("Heizkessel HK1/Test");
        assert_eq!(s.object_id, "heizkessel_hk1_test");
    }

    #[test]
    fn climate_defaults_match_python() {
        let c = ClimateEntity::new("Heizkessel HK1");
        assert_eq!(c.supported_modes, vec![3, 0]);
        assert_eq!(c.visual_min_temperature, 0.0);
        assert_eq!(c.visual_max_temperature, 100.0);
        assert_eq!(c.visual_temperature_step, 1.0);
        assert_eq!(c.mode, 0);
        assert_eq!(c.target_temperature, 60.0);
    }

    #[test]
    fn climate_set_state_partial_updates() {
        let mut c = ClimateEntity::new("K");
        c.current_temperature = 42.0;
        c.set_state(3, 65.0, None, Some(3));
        assert_eq!(c.mode, 3);
        assert_eq!(c.target_temperature, 65.0);
        assert_eq!(c.current_temperature, 42.0); // unchanged
        assert_eq!(c.action, 3);
    }

    #[test]
    fn number_defaults_match_python() {
        let n = NumberEntity::new("Power Limit");
        assert_eq!(n.min_value, 0.0);
        assert_eq!(n.max_value, 100.0);
        assert_eq!(n.step, 1.0);
        assert_eq!(n.mode, 2);
        assert!(n.missing_state);
    }

    #[test]
    fn device_id_in_list_and_state_frames() {
        // device_id must be present in both; HA marks entities unavailable otherwise.
        let mut e = SensorEntity::new("T");
        e.device_id = 3;
        let list = e.list_response();
        let ProtoMessage::ListEntitiesSensorResponse(l) = &list else {
            panic!("wrong variant")
        };
        assert_eq!(l.device_id, 3);
        let ProtoMessage::SensorStateResponse(s) = e.state_response() else {
            panic!("wrong variant")
        };
        assert_eq!(s.device_id, 3);
    }
}
