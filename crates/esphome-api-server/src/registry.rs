//! Entity registry with per-entity state cache.
//!
//! Mirrors the Python server's `_sensors`/`_numbers`/`_climates` dicts, but
//! preserves insertion order explicitly: HA lists entities in the order the
//! ListEntities stream sends them, and the golden-diff test (Phase 2) compares
//! byte streams with the Python server — dict order is insertion order there.

use std::collections::HashMap;

use esphome_native_api::parser::ProtoMessage;
use esphome_native_api::proto::ListEntitiesDoneResponse;

use crate::entities::{
    BinarySensorEntity, ClimateEntity, NumberEntity, SensorEntity, TextSensorEntity,
};

/// One entry of the shared sensor dict (Python uses one dict for all three).
#[derive(Debug, Clone)]
pub enum SensorEntry {
    Numeric(SensorEntity),
    Binary(BinarySensorEntity),
    Text(TextSensorEntity),
}

impl SensorEntry {
    pub fn key(&self) -> u32 {
        match self {
            SensorEntry::Numeric(e) => e.key,
            SensorEntry::Binary(e) => e.key,
            SensorEntry::Text(e) => e.key,
        }
    }

    pub fn object_id(&self) -> &str {
        match self {
            SensorEntry::Numeric(e) => &e.object_id,
            SensorEntry::Binary(e) => &e.object_id,
            SensorEntry::Text(e) => &e.object_id,
        }
    }

    fn list_response(&self) -> ProtoMessage {
        match self {
            SensorEntry::Numeric(e) => e.list_response(),
            SensorEntry::Binary(e) => e.list_response(),
            SensorEntry::Text(e) => e.list_response(),
        }
    }

    fn state_response(&self) -> ProtoMessage {
        match self {
            SensorEntry::Numeric(e) => e.state_response(),
            SensorEntry::Binary(e) => e.state_response(),
            SensorEntry::Text(e) => e.state_response(),
        }
    }

    fn set(&mut self, value: SensorValue) {
        match (self, value) {
            (SensorEntry::Numeric(e), SensorValue::Number(v)) => e.set_state(v),
            (SensorEntry::Binary(e), SensorValue::Bool(v)) => e.set_state(v),
            (SensorEntry::Text(e), SensorValue::Text(v)) => e.set_state(&v),
            (entry, value) => {
                tracing::warn!(
                    object_id = entry.object_id(),
                    "type mismatch on update: {:?}",
                    value.kind()
                );
            }
        }
    }
}

/// Value passed to [`Registry::update_sensor`] — mirrors Python's
/// `update_sensor(key, value: float | bool | str)`.
#[derive(Debug, Clone)]
pub enum SensorValue {
    Number(f64),
    Bool(bool),
    Text(String),
}

impl SensorValue {
    fn kind(&self) -> &'static str {
        match self {
            SensorValue::Number(_) => "number",
            SensorValue::Bool(_) => "bool",
            SensorValue::Text(_) => "text",
        }
    }
}

/// All entities of the device, in registration (insertion) order.
#[derive(Debug, Default)]
pub struct Registry {
    sensors: Vec<SensorEntry>,
    numbers: Vec<NumberEntity>,
    climates: Vec<ClimateEntity>,
    sensor_index: HashMap<u32, usize>,
    number_index: HashMap<u32, usize>,
    climate_index: HashMap<u32, usize>,
}

impl Registry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn add_sensor(&mut self, entry: SensorEntry) -> u32 {
        let key = entry.key();
        self.sensor_index.insert(key, self.sensors.len());
        self.sensors.push(entry);
        key
    }

    pub fn add_number(&mut self, number: NumberEntity) -> u32 {
        let key = number.key;
        self.number_index.insert(key, self.numbers.len());
        self.numbers.push(number);
        key
    }

    pub fn add_climate(&mut self, climate: ClimateEntity) -> u32 {
        let key = climate.key;
        self.climate_index.insert(key, self.climates.len());
        self.climates.push(climate);
        key
    }

    /// Update a sensor and return the state frame to broadcast, if any.
    pub fn update_sensor(&mut self, key: u32, value: SensorValue) -> Option<ProtoMessage> {
        let idx = self.sensor_index.get(&key).copied()?;
        self.sensors[idx].set(value);
        Some(self.sensors[idx].state_response())
    }

    pub fn update_number(&mut self, key: u32, value: f64) -> Option<ProtoMessage> {
        let idx = self.number_index.get(&key).copied()?;
        self.numbers[idx].set_state(value);
        Some(self.numbers[idx].state_response())
    }

    pub fn update_climate(
        &mut self,
        key: u32,
        mode: i32,
        target_temperature: f64,
        current_temperature: Option<f64>,
        action: Option<i32>,
    ) -> Option<ProtoMessage> {
        let idx = self.climate_index.get(&key).copied()?;
        self.climates[idx].set_state(mode, target_temperature, current_temperature, action);
        Some(self.climates[idx].state_response())
    }

    /// ListEntities stream: all sensors, then numbers, then climates, then Done
    /// — exactly the Python `_handle_message` order.
    pub fn list_frames(&self) -> Vec<ProtoMessage> {
        let mut frames: Vec<ProtoMessage> = self.sensors.iter().map(|e| e.list_response()).collect();
        frames.extend(self.numbers.iter().map(|e| e.list_response()));
        frames.extend(self.climates.iter().map(|e| e.list_response()));
        frames.push(ProtoMessage::ListEntitiesDoneResponse(ListEntitiesDoneResponse {}));
        frames
    }

    /// Initial state burst on SubscribeStates — same order as list_frames
    /// (minus Done), per Python.
    pub fn initial_state_frames(&self) -> Vec<ProtoMessage> {
        let mut frames: Vec<ProtoMessage> = self.sensors.iter().map(|e| e.state_response()).collect();
        frames.extend(self.numbers.iter().map(|e| e.state_response()));
        frames.extend(self.climates.iter().map(|e| e.state_response()));
        frames
    }

    pub fn sensor_count(&self) -> usize {
        self.sensors.len()
    }

    /// All registered entity keys (sensors, numbers, climates) — for parity
    /// tests and diagnostics.
    pub fn all_keys(&self) -> Vec<u32> {
        let mut keys: Vec<u32> = self.sensors.iter().map(|e| e.key()).collect();
        keys.extend(self.numbers.iter().map(|e| e.key));
        keys.extend(self.climates.iter().map(|e| e.key));
        keys
    }

    pub fn climate_count(&self) -> usize {
        self.climates.len()
    }

    pub fn number_count(&self) -> usize {
        self.numbers.len()
    }

    pub fn get_climate(&self, key: u32) -> Option<&ClimateEntity> {
        self.climate_index.get(&key).map(|&i| &self.climates[i])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn insertion_order_preserved_across_types() {
        let mut r = Registry::new();
        let k1 = r.add_sensor(SensorEntry::Numeric(SensorEntity::new("A")));
        let k2 = r.add_number(NumberEntity::new("B"));
        let k3 = r.add_climate(ClimateEntity::new("C"));
        let k4 = r.add_sensor(SensorEntry::Binary(BinarySensorEntity::new("D")));

        // Python uses ONE dict for all sensor types: A, D (sensors), then B
        // (numbers), then C (climates), then done.
        let frames = r.list_frames();
        assert_eq!(frames.len(), 5); // 4 entities + done
        assert!(matches!(frames[0], ProtoMessage::ListEntitiesSensorResponse(_)));
        assert!(matches!(frames[1], ProtoMessage::ListEntitiesBinarySensorResponse(_)));
        assert!(matches!(frames[2], ProtoMessage::ListEntitiesNumberResponse(_)));
        assert!(matches!(frames[3], ProtoMessage::ListEntitiesClimateResponse(_)));
        assert!(matches!(frames[4], ProtoMessage::ListEntitiesDoneResponse(_)));

        assert_eq!(
            r.list_frames().iter().filter(|f| !matches!(f, ProtoMessage::ListEntitiesDoneResponse(_))).count(),
            4
        );
        // keys unique and lookupable
        assert!(r.get_climate(k3).is_some());
        let _ = (k1, k2, k4);
    }

    #[test]
    fn update_sensor_returns_frame_and_clears_missing() {
        let mut r = Registry::new();
        let k = r.add_sensor(SensorEntry::Numeric(SensorEntity::new("T")));
        let frame = r.update_sensor(k, SensorValue::Number(12.5)).expect("frame");
        let ProtoMessage::SensorStateResponse(s) = frame else {
            panic!("wrong variant")
        };
        assert_eq!(s.state, 12.5);
        assert!(!s.missing_state);
    }

    #[test]
    fn update_unknown_key_returns_none() {
        let mut r = Registry::new();
        assert!(r.update_sensor(0xdeadbeef, SensorValue::Number(1.0)).is_none());
    }

    #[test]
    fn type_mismatch_does_not_panic() {
        let mut r = Registry::new();
        let k = r.add_sensor(SensorEntry::Numeric(SensorEntity::new("T")));
        assert!(r.update_sensor(k, SensorValue::Text("nope".into())).is_some());
        // value unchanged
        let idx = *r.sensor_index.get(&k).unwrap();
        let SensorEntry::Numeric(e) = &r.sensors[idx] else {
            panic!("wrong kind")
        };
        assert_eq!(e.state, 0.0);
        assert!(e.missing_state); // set_state never ran
    }

    #[test]
    fn initial_burst_matches_list_order_minus_done() {
        let mut r = Registry::new();
        r.add_sensor(SensorEntry::Numeric(SensorEntity::new("A")));
        r.add_climate(ClimateEntity::new("C"));
        let burst = r.initial_state_frames();
        assert_eq!(burst.len(), 2);
        assert!(matches!(burst[0], ProtoMessage::SensorStateResponse(_)));
        assert!(matches!(burst[1], ProtoMessage::ClimateStateResponse(_)));
    }
}
