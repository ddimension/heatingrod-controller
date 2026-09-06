//! state.json persistence — mirrors controller.py `_load_state`/`_save_state`.
//!
//! Stores the ProControl CH1 voltage (boiler setpoint) so it survives
//! restarts. Path: next to the calibration file (Python `_state_path`).

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

#[derive(Debug, Serialize, Deserialize, Default)]
pub struct ControllerState {
    pub procontrol_ch1_voltage: f64,
}

/// Python `_state_path`: `dirname(calibration.file_path) or "."` + state.json.
pub fn state_path(calibration_file_path: &str) -> PathBuf {
    let dir = Path::new(calibration_file_path)
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    dir.join("state.json")
}

/// Python `_load_state`: clamped 0..10, default = ch1 safe voltage.
pub fn load_ch1_voltage(path: &Path, ch1_safe_voltage: f64) -> f64 {
    let Ok(raw) = std::fs::read_to_string(path) else {
        return ch1_safe_voltage;
    };
    let Ok(state) = serde_json::from_str::<ControllerState>(&raw) else {
        tracing::warn!("state.json unreadable, using safe CH1 voltage");
        return ch1_safe_voltage;
    };
    let v = state.procontrol_ch1_voltage.clamp(0.0, 10.0);
    tracing::info!("state.json: restored CH1={v:.2}V");
    v
}

pub fn save_ch1_voltage(path: &Path, voltage: f64) {
    let state = ControllerState {
        procontrol_ch1_voltage: voltage.clamp(0.0, 10.0),
    };
    if let Ok(raw) = serde_json::to_string_pretty(&state)
        && let Err(e) = std::fs::write(path, raw)
    {
        tracing::error!("state.json save failed: {e}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
        let dir = std::env::temp_dir().join(format!("hr-state-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("state.json");
        let _ = std::fs::remove_file(&p);
        assert_eq!(load_ch1_voltage(&p, 6.0), 6.0);
        save_ch1_voltage(&p, 7.2);
        assert_eq!(load_ch1_voltage(&p, 6.0), 7.2);
        save_ch1_voltage(&p, 99.0);
        assert_eq!(load_ch1_voltage(&p, 6.0), 10.0);
    }
}
