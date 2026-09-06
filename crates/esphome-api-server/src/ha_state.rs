//! HA entity states pushed back over the API connection.
//!
//! Mirrors `server.py`: `_ha_states` stores `(state_str, timestamp)` per
//! entity_id; `get_ha_state` returns None for missing entries,
//! "unavailable"/"unknown" values, and stale entries.

use std::collections::HashMap;
use std::time::{Duration, Instant};

#[derive(Debug, Default)]
pub struct HaStateStore {
    states: HashMap<String, (String, Instant)>,
}

impl HaStateStore {
    pub fn store(&mut self, entity_id: &str, state: &str) {
        self.states
            .insert(entity_id.to_string(), (state.to_string(), Instant::now()));
    }

    /// Last known state, or None if missing/unavailable/unknown/stale.
    pub fn get(&self, entity_id: &str, max_age: Duration) -> Option<&str> {
        let (state, ts) = self.states.get(entity_id)?;
        if state == "unavailable" || state == "unknown" {
            return None;
        }
        if ts.elapsed() > max_age {
            return None;
        }
        Some(state)
    }

    pub fn len(&self) -> usize {
        self.states.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_is_none() {
        let s = HaStateStore::default();
        assert!(s.get("sensor.x", Duration::from_secs(60)).is_none());
    }

    #[test]
    fn unavailable_and_unknown_are_none() {
        let mut s = HaStateStore::default();
        s.store("sensor.x", "unavailable");
        assert!(s.get("sensor.x", Duration::from_secs(60)).is_none());
        s.store("sensor.x", "unknown");
        assert!(s.get("sensor.x", Duration::from_secs(60)).is_none());
    }

    #[test]
    fn stale_is_none() {
        let mut s = HaStateStore::default();
        s.store("sensor.x", "42");
        assert_eq!(s.get("sensor.x", Duration::from_secs(0)), None);
        assert_eq!(s.get("sensor.x", Duration::from_secs(60)), Some("42"));
    }

    #[test]
    fn fresh_value_returned() {
        let mut s = HaStateStore::default();
        s.store("sensor.x", "1337.5");
        assert_eq!(s.get("sensor.x", Duration::from_secs(60)), Some("1337.5"));
    }
}
