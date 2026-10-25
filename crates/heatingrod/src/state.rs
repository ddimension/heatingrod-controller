//! state.json persistence — mirrors controller.py `_load_state`/`_save_state`.
//!
//! Stores the ProControl CH1 voltage (boiler setpoint) so it survives
//! restarts, plus the grid-heating setpoint and the rod's grid energy
//! counters (Rust-only fields). Path: next to the calibration file
//! (Python `_state_path`).

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
#[serde(default)]
pub struct ControllerState {
    /// `None` = never stored → caller falls back to the CH1 safe voltage
    /// (a missing field must not read as 0V = Brennersperre).
    pub procontrol_ch1_voltage: Option<f64>,
    /// Netz-Solltemperatur (°C), 0 = grid heating off.
    pub grid_heat_setpoint: f64,
    /// Rod energy drawn from the grid, all-time (Wh).
    pub overdraw_energy_wh: f64,
    /// Rod energy drawn from the grid on `overdraw_day` (Wh).
    pub overdraw_today_wh: f64,
    /// Local date of `overdraw_today_wh` as YYYYMMDD.
    pub overdraw_day: i64,
}

/// Python `_state_path`: `dirname(calibration.file_path) or "."` + state.json.
pub fn state_path(calibration_file_path: &str) -> PathBuf {
    let dir = Path::new(calibration_file_path)
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    dir.join("state.json")
}

/// Missing file → defaults; unreadable file → defaults + warning.
pub fn load(path: &Path) -> ControllerState {
    let Ok(raw) = std::fs::read_to_string(path) else {
        return ControllerState::default();
    };
    serde_json::from_str(&raw).unwrap_or_else(|_| {
        tracing::warn!("state.json unreadable, using defaults (safe CH1 voltage)");
        ControllerState::default()
    })
}

/// Read-modify-write; written via temp file + rename so a crash mid-write
/// never leaves a truncated state.json behind.
pub fn update(path: &Path, f: impl FnOnce(&mut ControllerState)) {
    let mut state = load(path);
    f(&mut state);
    let tmp = path.with_extension("json.tmp");
    let res = serde_json::to_string_pretty(&state)
        .map_err(std::io::Error::other)
        .and_then(|raw| std::fs::write(&tmp, raw))
        .and_then(|()| std::fs::rename(&tmp, path));
    if let Err(e) = res {
        tracing::error!("state.json save failed: {e}");
    }
}

/// Python `_load_state`: clamped 0..10, default = ch1 safe voltage.
pub fn load_ch1_voltage(path: &Path, ch1_safe_voltage: f64) -> f64 {
    let Some(v) = load(path).procontrol_ch1_voltage else {
        return ch1_safe_voltage;
    };
    let v = v.clamp(0.0, 10.0);
    tracing::info!("state.json: restored CH1={v:.2}V");
    v
}

pub fn save_ch1_voltage(path: &Path, voltage: f64) {
    update(path, |s| s.procontrol_ch1_voltage = Some(voltage.clamp(0.0, 10.0)));
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_state(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("hr-state-{name}-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("state.json");
        let _ = std::fs::remove_file(&p);
        p
    }

    #[test]
    fn state_path_next_to_calibration() {
        assert_eq!(
            state_path("/root/heatingrod-rust/calibration.json"),
            PathBuf::from("/root/heatingrod-rust/state.json")
        );
        // Python: os.path.dirname("calibration.json") = "" → "." + state.json
        assert_eq!(
            state_path("calibration.json"),
            PathBuf::from("./state.json")
        );
    }

    #[test]
    fn load_defaults_to_safe_voltage() {
        let p = temp_state("ch1");
        assert_eq!(load_ch1_voltage(&p, 6.0), 6.0);
        save_ch1_voltage(&p, 7.2);
        assert_eq!(load_ch1_voltage(&p, 6.0), 7.2);
        save_ch1_voltage(&p, 99.0);
        assert_eq!(load_ch1_voltage(&p, 6.0), 10.0);
    }

    #[test]
    fn legacy_file_keeps_ch1_and_defaults_new_fields() {
        let p = temp_state("legacy");
        std::fs::write(&p, r#"{"procontrol_ch1_voltage": 0.0}"#).unwrap();
        assert_eq!(load_ch1_voltage(&p, 6.0), 0.0);
        let s = load(&p);
        assert_eq!(s.grid_heat_setpoint, 0.0);
        assert_eq!(s.overdraw_energy_wh, 0.0);
    }

    #[test]
    fn missing_ch1_field_falls_back_to_safe_voltage() {
        let p = temp_state("noch1");
        update(&p, |s| s.grid_heat_setpoint = 45.0);
        assert_eq!(load_ch1_voltage(&p, 6.0), 6.0);
    }

    #[test]
    fn updates_preserve_other_fields() {
        let p = temp_state("merge");
        save_ch1_voltage(&p, 6.0);
        update(&p, |s| {
            s.grid_heat_setpoint = 45.0;
            s.overdraw_energy_wh = 1234.5;
        });
        save_ch1_voltage(&p, 0.0);
        let s = load(&p);
        assert_eq!(s.procontrol_ch1_voltage, Some(0.0));
        assert_eq!(s.grid_heat_setpoint, 45.0);
        assert_eq!(s.overdraw_energy_wh, 1234.5);
    }
}
