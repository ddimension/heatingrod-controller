//! Trait definitions + mock implementations (offline mode).

use std::collections::HashMap;
use std::time::{SystemTime, UNIX_EPOCH};

/// DAC abstraction — mirrors `heatingrod/dac.py` semantics.
pub trait Dac: Send {
    fn set_voltage(&mut self, voltage: f64);
    fn set_voltage_ch1(&mut self, voltage: f64);
    fn current_voltage(&self) -> f64;
    fn current_voltage_ch1(&self) -> f64;
    /// current / max_voltage (0..1).
    fn current_level(&self) -> f64;
    fn is_responsive(&self) -> bool;
    /// CH0 → 0V, CH1 → safe voltage (fail-safe for the boiler).
    fn shutdown(&mut self);
}

/// Mock DAC with Python `DACController` mock semantics: clamp to max_voltage,
/// store software state, CH1 starts at the safe voltage.
pub struct MockDac {
    pub current_voltage: f64,
    pub current_voltage_ch1: f64,
    pub max_voltage: f64,
    /// CH1 fail-safe voltage (Python `ch1_safe_voltage`, default 6V = 60°C).
    pub ch1_safe_voltage: f64,
}

impl MockDac {
    pub fn new(max_voltage: f64) -> Self {
        Self {
            current_voltage: 0.0,
            current_voltage_ch1: 6.0,
            max_voltage,
            ch1_safe_voltage: 6.0,
        }
    }
}

impl Dac for MockDac {
    fn set_voltage(&mut self, voltage: f64) {
        self.current_voltage = voltage.clamp(0.0, self.max_voltage);
    }

    fn set_voltage_ch1(&mut self, voltage: f64) {
        self.current_voltage_ch1 = voltage.clamp(0.0, 10.0);
    }

    fn current_voltage(&self) -> f64 {
        self.current_voltage
    }

    fn current_voltage_ch1(&self) -> f64 {
        self.current_voltage_ch1
    }

    fn current_level(&self) -> f64 {
        if self.max_voltage == 0.0 {
            0.0
        } else {
            self.current_voltage / self.max_voltage
        }
    }

    fn is_responsive(&self) -> bool {
        true
    }

    fn shutdown(&mut self) {
        self.current_voltage = 0.0;
        self.current_voltage_ch1 = self.ch1_safe_voltage;
    }
}

/// Which physical sensor backs a value (fallback chains differ per role).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SensorRole {
    /// Powerlogger client first, HA entity as fallback.
    Grid,
    /// DS100 direct first, HA entity as fallback.
    RodPower,
    /// 1-Wire "1-Wire Heizstab" first, HA entity as fallback.
    RodTemp,
    /// 1-Wire "1-Wire Speicher oben" first, HA entity as fallback.
    TankTemp,
}

/// Everything the controller logic reads/writes beyond the DAC and the
/// calibration curve — HA states, sensor values, ESPHome pushes, time.
pub trait ControllerIo {
    fn now(&self) -> f64;
    /// HA entity state with freshness; None = missing/stale/unavailable.
    fn ha_state(&self, entity_id: &str, max_age: f64) -> Option<String>;
    /// Resolved sensor value for a role (hardware first, HA fallback).
    fn role_state(&self, role: SensorRole, entity_id: &str, max_age: f64) -> Option<f64>;
    /// Number of HA state subscribers (0 = HA not connected).
    fn ha_subscribers(&self) -> usize;
    /// Push a sensor value to ESPHome subscribers.
    fn push_sensor(&mut self, object_id: &str, value: f64);
}

pub fn system_now() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
}

/// Offline environment for tests and `--mock-all` runs.
pub struct MockEnv {
    /// entity_id → (state, timestamp)
    pub states: HashMap<String, (String, f64)>,
    /// Direct role values (powerlogger/ds100 role), None = no direct source.
    pub overrides: HashMap<SensorRole, Option<f64>>,
    pub clock: f64,
    pub subscribers: usize,
    /// Recorded sensor pushes (object_id, value).
    pub pushes: Vec<(String, f64)>,
}

impl Default for MockEnv {
    fn default() -> Self {
        Self {
            states: HashMap::new(),
            overrides: HashMap::new(),
            clock: system_now(),
            subscribers: 1,
            pushes: Vec::new(),
        }
    }
}

impl MockEnv {
    pub fn new() -> Self {
        Self::default()
    }

    /// Set a HA entity state; None → "unavailable" (Python `_set_sensor`).
    pub fn set(&mut self, entity_id: &str, value: Option<f64>) {
        let state = match value {
            Some(v) => v.to_string(),
            None => "unavailable".into(),
        };
        self.states
            .insert(entity_id.to_string(), (state, self.clock));
    }

    /// Set with a given age (timestamp = clock - age); used for staleness tests.
    pub fn set_aged(&mut self, entity_id: &str, value: Option<f64>, age: f64) {
        let state = match value {
            Some(v) => v.to_string(),
            None => "unavailable".into(),
        };
        self.states
            .insert(entity_id.to_string(), (state, self.clock - age));
    }

    pub fn set_str(&mut self, entity_id: &str, value: Option<&str>) {
        let state = value.unwrap_or("unavailable").to_string();
        self.states
            .insert(entity_id.to_string(), (state, self.clock));
    }

    pub fn set_override(&mut self, role: SensorRole, value: Option<f64>) {
        self.overrides.insert(role, value);
    }

    /// Advance the mock clock.
    pub fn advance(&mut self, secs: f64) {
        self.clock += secs;
    }
}

impl ControllerIo for MockEnv {
    fn now(&self) -> f64 {
        self.clock
    }

    fn ha_state(&self, entity_id: &str, max_age: f64) -> Option<String> {
        let (state, ts) = self.states.get(entity_id)?;
        if state == "unavailable" || state == "unknown" {
            return None;
        }
        if self.clock - ts > max_age {
            return None;
        }
        Some(state.clone())
    }

    fn role_state(&self, role: SensorRole, entity_id: &str, max_age: f64) -> Option<f64> {
        if let Some(direct) = self.overrides.get(&role).copied().flatten() {
            return Some(direct);
        }
        self.ha_state(entity_id, max_age)?.parse::<f64>().ok()
    }

    fn ha_subscribers(&self) -> usize {
        self.subscribers
    }

    fn push_sensor(&mut self, object_id: &str, value: f64) {
        self.pushes.push((object_id.to_string(), value));
    }
}
