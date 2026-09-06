//! Self-learning voltage→watts curve — 1:1 port of `heatingrod/calibration.py`.
//!
//! The tests below are ports of `tests/test_calibration.py` (30 cases) —
//! they encode four days of lost heating (incident 2026-08-20: poisoned
//! all-zero curve pinned the DAC to 1.01V) and the extrapolation semantics
//! that keep the curve from capping the rod at the last sweep's power.

use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use esphome_api_server::entities::round_half_even;

fn now() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
}

/// One learned point on the curve (Python `CalibrationPoint`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CalibrationPoint {
    pub voltage: f64,
    pub watts: f64,
    #[serde(default = "default_count")]
    pub count: i64,
    #[serde(default)]
    pub last_updated: f64,
}

fn default_count() -> i64 {
    1
}

impl CalibrationPoint {
    pub fn new(voltage: f64, watts: f64) -> Self {
        Self {
            voltage,
            watts,
            count: 1,
            last_updated: now(),
        }
    }
}

/// Persisted shape (Python `CalibrationData`).
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct CalibrationData {
    #[serde(default)]
    pub points: Vec<CalibrationPoint>,
    #[serde(default)]
    pub created_at: f64,
    #[serde(default)]
    pub last_calibration_at: f64,
}

pub struct CalibrationManager {
    pub file_path: String,
    pub max_voltage: f64,
    pub max_age_days: u64,
    pub deviation_threshold: f64,
    pub ewma_alpha: f64,
    pub data: CalibrationData,
    deviation_history: Vec<f64>,
}

impl CalibrationManager {
    /// A point at or below this power is "no output", not a curve point: the
    /// analog thermal limiter (~67°C) drives the rod to ~0W at every voltage.
    pub const ZERO_WATTS: f64 = 10.0;
    /// Baseline width for extrapolating above the calibrated range.
    pub const EXTRAPOLATION_BASELINE_V: f64 = 0.5;
    /// Below this voltage a 0W reading is legitimate (Chiemtronic T-Drive
    /// dead band); above it, flat 0W means limiter or fault.
    pub const MIN_MEANINGFUL_VOLTAGE: f64 = 2.0;

    pub fn new(file_path: &str, max_age_days: u64, deviation_threshold: f64, ewma_alpha: f64, max_voltage: f64) -> Self {
        let t = now();
        Self {
            file_path: file_path.to_string(),
            max_voltage,
            max_age_days,
            deviation_threshold,
            ewma_alpha,
            data: CalibrationData {
                points: Vec::new(),
                created_at: t,
                last_calibration_at: t,
            },
            deviation_history: Vec::new(),
        }
    }

    pub fn load(&mut self) -> bool {
        if !Path::new(&self.file_path).exists() {
            tracing::info!("No calibration file found at {}", self.file_path);
            return false;
        }
        let raw = match std::fs::read_to_string(&self.file_path) {
            Ok(r) => r,
            Err(e) => {
                tracing::error!("Failed to load calibration: {e}");
                return false;
            }
        };
        let mut data: CalibrationData = match serde_json::from_str(&raw) {
            Ok(d) => d,
            Err(e) => {
                tracing::error!("Failed to load calibration: {e}");
                return false;
            }
        };
        // Python __post_init__: zero timestamps become "now".
        let t = now();
        if data.created_at == 0.0 {
            data.created_at = t;
        }
        if data.last_calibration_at == 0.0 {
            data.last_calibration_at = t;
        }
        for p in &mut data.points {
            if p.last_updated == 0.0 {
                p.last_updated = t;
            }
        }
        data.points.sort_by(|a, b| a.voltage.total_cmp(&b.voltage));
        tracing::info!(
            "Loaded calibration with {} points from {}",
            data.points.len(),
            self.file_path
        );
        self.data = data;
        true
    }

    pub fn save(&self) {
        let raw = match serde_json::to_string_pretty(&self.data) {
            Ok(r) => r,
            Err(e) => {
                tracing::error!("Failed to save calibration: {e}");
                return;
            }
        };
        if let Err(e) = std::fs::write(&self.file_path, raw) {
            tracing::error!("Failed to save calibration: {e}");
        }
    }

    pub fn has_calibration(&self) -> bool {
        if self.data.points.len() < 2 {
            return false;
        }
        // An all-~0W curve carries no information (thermal limiter during
        // sweep). Treating it as valid is worse than having none.
        self.data.points.iter().any(|p| p.watts > Self::ZERO_WATTS)
    }

    pub fn is_degenerate(&self) -> bool {
        self.data.points.len() >= 2
            && !self.data.points.iter().any(|p| p.watts > Self::ZERO_WATTS)
    }

    pub fn needs_recalibration(&self) -> bool {
        if !self.has_calibration() {
            return true;
        }
        let age_s = now() - self.data.last_calibration_at;
        // Cooldown: no recalibration within 10 minutes of the last one.
        if age_s < 600.0 {
            return false;
        }
        let age_days = age_s / 86400.0;
        if age_days > self.max_age_days as f64 {
            tracing::info!(
                "Calibration is {age_days:.1} days old (max {}), needs recalibration",
                self.max_age_days
            );
            return true;
        }
        if self.deviation_history.len() >= 5 {
            let last: Vec<f64> = self.deviation_history.iter().rev().take(10).copied().collect();
            let avg: f64 = last.iter().sum::<f64>() / last.len() as f64;
            if avg > self.deviation_threshold {
                tracing::info!(
                    "Average deviation {:.1}% exceeds threshold {:.1}%, needs recalibration",
                    avg * 100.0,
                    self.deviation_threshold * 100.0
                );
                return true;
            }
        }
        false
    }

    /// The learned points projected onto a non-decreasing curve (pool
    /// adjacent violators, weighted by reading count).
    pub fn monotonic_points(&self) -> Vec<CalibrationPoint> {
        let pts = &self.data.points;
        // blocks of [weighted watt sum, weight, member count]
        let mut blocks: Vec<(f64, f64, i64)> = Vec::new();
        for p in pts {
            let w = f64::max(1.0, p.count as f64);
            blocks.push((p.watts * w, w, 1));
            while blocks.len() >= 2 {
                let (l0, l1, _) = blocks[blocks.len() - 2];
                let (r0, r1, _) = blocks[blocks.len() - 1];
                if l0 / l1 <= r0 / r1 {
                    break;
                }
                let merged = blocks.pop().unwrap();
                let last = blocks.last_mut().unwrap();
                last.0 += merged.0;
                last.1 += merged.1;
                last.2 += merged.2;
            }
        }

        let mut out: Vec<CalibrationPoint> = Vec::with_capacity(pts.len());
        let mut i = 0usize;
        for (total, weight, members) in blocks {
            let level = total / weight;
            for p in &pts[i..i + members as usize] {
                out.push(CalibrationPoint {
                    voltage: p.voltage,
                    watts: level,
                    count: p.count,
                    last_updated: p.last_updated,
                });
            }
            i += members as usize;
        }
        out
    }

    pub fn voltage_for_power(&self, target_watts: f64) -> Option<f64> {
        if !self.has_calibration() {
            return None;
        }
        let target_watts = target_watts.max(0.0);
        let points = self.monotonic_points();

        if target_watts <= points[0].watts {
            if points[0].watts == 0.0 {
                return Some(points[0].voltage);
            }
            let ratio = target_watts / points[0].watts;
            return Some(points[0].voltage * ratio);
        }

        if target_watts >= points[points.len() - 1].watts {
            // Extrapolate above the range along a wide baseline (the pooled
            // tail is often flat; a narrow last segment is noisy).
            let top = &points[points.len() - 1];
            let mut base = &points[0];
            for p in &points {
                if p.voltage <= top.voltage - Self::EXTRAPOLATION_BASELINE_V {
                    base = p;
                }
            }
            let dw = top.watts - base.watts;
            let dv = top.voltage - base.voltage;
            if dw > 0.0 && dv > 0.0 {
                let extra = (target_watts - top.watts) * dv / dw;
                return Some((top.voltage + extra).min(self.max_voltage));
            }
            return Some(top.voltage);
        }

        for i in 0..points.len() - 1 {
            let p1 = &points[i];
            let p2 = &points[i + 1];
            if p1.watts <= target_watts && target_watts <= p2.watts {
                if p2.watts == p1.watts {
                    return Some(p1.voltage);
                }
                let ratio = (target_watts - p1.watts) / (p2.watts - p1.watts);
                return Some(p1.voltage + ratio * (p2.voltage - p1.voltage));
            }
        }
        Some(points[points.len() - 1].voltage)
    }

    pub fn expected_power(&self, voltage: f64) -> Option<f64> {
        if !self.has_calibration() {
            return None;
        }
        let points = self.monotonic_points();

        if voltage <= points[0].voltage {
            if points[0].voltage == 0.0 {
                return Some(0.0);
            }
            let ratio = voltage / points[0].voltage;
            return Some(points[0].watts * ratio);
        }
        if voltage >= points[points.len() - 1].voltage {
            return Some(points[points.len() - 1].watts);
        }
        for i in 0..points.len() - 1 {
            let p1 = &points[i];
            let p2 = &points[i + 1];
            if p1.voltage <= voltage && voltage <= p2.voltage {
                if p2.voltage == p1.voltage {
                    return Some(p1.watts);
                }
                let ratio = (voltage - p1.voltage) / (p2.voltage - p1.voltage);
                return Some(p1.watts + ratio * (p2.watts - p1.watts));
            }
        }
        Some(points[points.len() - 1].watts)
    }

    pub fn update(&mut self, voltage: f64, actual_watts: f64) {
        if actual_watts < 0.0 {
            return;
        }
        // Refuse to learn "no output" at a voltage that should produce power
        // (thermal limiter or tripped breaker — poisons the curve).
        if actual_watts <= Self::ZERO_WATTS && voltage > Self::MIN_MEANINGFUL_VOLTAGE {
            tracing::debug!(
                "Calibration: ignoring {actual_watts:.0}W at {voltage:.2}V (no output, likely thermal limiter)"
            );
            return;
        }
        let expected = self.expected_power(voltage);
        if let Some(expected) = expected
            && expected > 0.0
        {
            let deviation = (actual_watts - expected).abs() / expected;
            self.deviation_history.push(deviation);
            if self.deviation_history.len() > 100 {
                // Python: history[-50:] — keep the last 50 entries.
                let keep_from = self.deviation_history.len() - 50;
                self.deviation_history = self.deviation_history.split_off(keep_from);
            }
        }
        if let Some(existing) = self
            .data
            .points
            .iter_mut()
            .find(|p| (p.voltage - voltage).abs() < 0.05)
        {
            existing.watts = self.ewma_alpha * actual_watts + (1.0 - self.ewma_alpha) * existing.watts;
            existing.count += 1;
            existing.last_updated = now();
        } else {
            self.data.points.push(CalibrationPoint::new(voltage, actual_watts));
            self.data
                .points
                .sort_by(|a, b| a.voltage.total_cmp(&b.voltage));
        }
        self.save();
    }

    pub fn set_calibration(&mut self, points: &[(f64, f64)]) {
        if !points.iter().any(|(_, w)| *w > Self::ZERO_WATTS) {
            tracing::warn!(
                "Calibration rejected: all {} points are ~0W (thermal limiter active?) — keeping previous curve",
                points.len()
            );
            return;
        }
        let mut pts: Vec<CalibrationPoint> =
            points.iter().map(|(v, w)| CalibrationPoint::new(*v, *w)).collect();
        pts.sort_by(|a, b| a.voltage.total_cmp(&b.voltage));
        self.data.points = pts;
        self.data.last_calibration_at = now();
        self.deviation_history.clear();
        self.save();
        tracing::info!("Calibration set with {} points", self.data.points.len());
    }

    pub fn get_calibration_voltages(&self, num_steps: u32, max_voltage: f64) -> Vec<f64> {
        let step_size = max_voltage / (num_steps + 1) as f64;
        (1..=num_steps)
            .map(|i| round_half_even(step_size * i as f64, 1))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    static DIR_SEQ: AtomicU64 = AtomicU64::new(0);

    fn make_manager(max_age_days: u64, deviation_threshold: f64) -> CalibrationManager {
        let dir = std::env::temp_dir().join(format!(
            "heatingrod-cal-{}-{}",
            std::process::id(),
            DIR_SEQ.fetch_add(1, Ordering::SeqCst)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("calibration.json");
        CalibrationManager::new(
            path.to_str().unwrap(),
            max_age_days,
            deviation_threshold,
            0.3,
            10.0,
        )
    }

    // ---- ports of test_calibration.py ----

    #[test]
    fn no_calibration_initially() {
        let cm = make_manager(7, 0.15);
        assert!(!cm.has_calibration());
        assert!(cm.needs_recalibration());
    }

    #[test]
    fn set_calibration() {
        let mut cm = make_manager(7, 0.15);
        cm.set_calibration(&[(1.0, 500.0), (3.0, 2000.0), (5.0, 4000.0), (7.0, 6000.0)]);
        assert!(cm.has_calibration());
        assert!(!cm.needs_recalibration());
    }

    #[test]
    fn save_and_load() {
        let mut cm = make_manager(7, 0.15);
        cm.set_calibration(&[(1.0, 500.0), (5.0, 4000.0)]);
        let path = cm.file_path.clone();

        let mut cm2 = CalibrationManager::new(&path, 7, 0.15, 0.3, 10.0);
        assert!(cm2.load());
        assert!(cm2.has_calibration());
        assert_eq!(cm2.data.points.len(), 2);
    }

    #[test]
    fn voltage_for_power_interpolation() {
        let mut cm = make_manager(7, 0.15);
        cm.set_calibration(&[(1.0, 1000.0), (5.0, 5000.0)]);
        let v = cm.voltage_for_power(3000.0).unwrap();
        assert!((v - 3.0).abs() < 0.01);
    }

    #[test]
    fn voltage_for_power_below_range() {
        let mut cm = make_manager(7, 0.15);
        cm.set_calibration(&[(2.0, 1000.0), (5.0, 4000.0)]);
        let v = cm.voltage_for_power(500.0).unwrap();
        assert!((v - 1.0).abs() < 0.01);
    }

    #[test]
    fn voltage_for_power_above_range() {
        let mut cm = make_manager(7, 0.15);
        cm.set_calibration(&[(1.0, 1000.0), (5.0, 4000.0)]);
        // 5.0V + (5500-4000)/750 = 7.0V
        let v = cm.voltage_for_power(5500.0).unwrap();
        assert!((v - 7.0).abs() < 0.02);
        // 8000W would extrapolate to 10.33V and is capped at max_voltage
        assert!((cm.voltage_for_power(8000.0).unwrap() - 10.0).abs() < 1e-9);
    }

    #[test]
    fn voltage_for_power_no_calibration() {
        let cm = make_manager(7, 0.15);
        assert!(cm.voltage_for_power(3000.0).is_none());
    }

    #[test]
    fn expected_power_interpolation() {
        let mut cm = make_manager(7, 0.15);
        cm.set_calibration(&[(1.0, 1000.0), (5.0, 5000.0)]);
        let p = cm.expected_power(3.0).unwrap();
        assert!((p - 3000.0).abs() < 1.0);
    }

    #[test]
    fn update_existing_point_ewma() {
        let mut cm = make_manager(7, 0.15);
        cm.set_calibration(&[(1.0, 1000.0), (5.0, 5000.0)]);
        cm.update(1.0, 1200.0);
        let expected = 0.3 * 1200.0 + 0.7 * 1000.0;
        assert!((cm.data.points[0].watts - expected).abs() < 1.0);
    }

    #[test]
    fn update_adds_new_point_sorted() {
        let mut cm = make_manager(7, 0.15);
        cm.set_calibration(&[(1.0, 1000.0), (5.0, 5000.0)]);
        cm.update(3.0, 3000.0);
        assert_eq!(cm.data.points.len(), 3);
        assert_eq!(cm.data.points[1].voltage, 3.0);
    }

    #[test]
    fn needs_recalibration_age() {
        let mut cm = make_manager(7, 0.15);
        cm.set_calibration(&[(1.0, 1000.0), (5.0, 5000.0)]);
        cm.data.last_calibration_at = now() - 8.0 * 86400.0;
        assert!(cm.needs_recalibration());
    }

    #[test]
    fn needs_recalibration_deviation() {
        let mut cm = make_manager(7, 0.15);
        cm.set_calibration(&[(1.0, 1000.0), (5.0, 5000.0)]);
        cm.data.last_calibration_at -= 700.0; // bypass cooldown
        for _ in 0..10 {
            cm.update(1.0, 2000.0);
        }
        assert!(cm.needs_recalibration());
    }

    #[test]
    fn get_calibration_voltages_shape() {
        let cm = make_manager(7, 0.15);
        let v = cm.get_calibration_voltages(6, 10.0);
        assert_eq!(v.len(), 6);
        assert!(v[0] > 0.0);
        assert!(v[5] < 10.0);
        assert!(v.windows(2).all(|w| w[0] <= w[1]));
    }

    #[test]
    fn negative_watts_ignored() {
        let mut cm = make_manager(7, 0.15);
        cm.set_calibration(&[(1.0, 1000.0), (5.0, 5000.0)]);
        cm.update(1.0, -500.0);
        assert_eq!(cm.data.points[0].watts, 1000.0);
    }

    #[test]
    fn zero_power_interpolation() {
        let mut cm = make_manager(7, 0.15);
        cm.set_calibration(&[(1.0, 1000.0), (5.0, 5000.0)]);
        let v = cm.voltage_for_power(0.0).unwrap();
        assert!((v - 0.0).abs() < 0.01);
    }

    #[test]
    fn points_sorted_after_updates() {
        let mut cm = make_manager(7, 0.15);
        cm.set_calibration(&[(1.0, 1000.0), (5.0, 5000.0)]);
        cm.update(3.0, 3000.0);
        cm.update(0.5, 200.0);
        cm.update(8.0, 7000.0);
        let voltages: Vec<f64> = cm.data.points.iter().map(|p| p.voltage).collect();
        let mut sorted = voltages.clone();
        sorted.sort_by(|a, b| a.total_cmp(b));
        assert_eq!(voltages, sorted);
    }

    // --- thermal-limiter poisoning (incident 2026-08-20) ---------------------

    fn poisoned() -> CalibrationManager {
        // The curve that actually shipped on the Pi: four points, all 0W.
        let mut cm = make_manager(7, 0.15);
        cm.data.points = [0.2, 0.4, 0.6988, 1.01416]
            .iter()
            .map(|v| CalibrationPoint {
                voltage: *v,
                watts: 0.0,
                count: 1,
                last_updated: now(),
            })
            .collect();
        cm
    }

    #[test]
    fn all_zero_curve_is_not_a_calibration() {
        let cm = poisoned();
        assert!(cm.is_degenerate());
        assert!(!cm.has_calibration());
        assert!(cm.needs_recalibration());
    }

    #[test]
    fn all_zero_curve_does_not_pin_the_dac() {
        let cm = poisoned();
        assert!(cm.voltage_for_power(3400.0).is_none());
        assert!(cm.expected_power(1.01).is_none());
    }

    #[test]
    fn zero_watts_at_driving_voltage_is_not_learned() {
        let mut cm = make_manager(7, 0.15);
        cm.set_calibration(&[(1.0, 1000.0), (5.0, 5000.0)]);
        cm.update(3.0, 0.0);
        let voltages: Vec<f64> = cm.data.points.iter().map(|p| p.voltage).collect();
        assert_eq!(voltages, vec![1.0, 5.0]);
        assert!((cm.expected_power(5.0).unwrap() - 5000.0).abs() < 1e-6);
    }

    #[test]
    fn zero_watts_does_not_erode_an_existing_point() {
        let mut cm = make_manager(7, 0.15);
        cm.set_calibration(&[(1.0, 1000.0), (5.0, 5000.0)]);
        for _ in 0..20 {
            cm.update(5.0, 0.0);
        }
        assert!((cm.expected_power(5.0).unwrap() - 5000.0).abs() < 1e-6);
    }

    #[test]
    fn zero_watts_in_the_dead_band_is_still_learned() {
        let mut cm = make_manager(7, 0.15);
        cm.set_calibration(&[(1.0, 1000.0), (5.0, 5000.0)]);
        cm.update(0.2, 0.0);
        assert!(cm.data.points.iter().any(|p| (p.voltage - 0.2).abs() < 1e-9));
    }

    #[test]
    fn dead_band_covers_the_poisoned_sweep() {
        assert!(CalibrationManager::MIN_MEANINGFUL_VOLTAGE > 1.01416);
    }

    #[test]
    fn set_calibration_rejects_all_zero_points() {
        let mut cm = make_manager(7, 0.15);
        cm.set_calibration(&[(1.0, 1000.0), (5.0, 5000.0)]);
        cm.set_calibration(&[(0.2, 0.0), (0.4, 0.0), (0.7, 0.0), (1.0, 0.0)]);
        assert!(cm.has_calibration());
        assert!((cm.expected_power(5.0).unwrap() - 5000.0).abs() < 1e-6);
    }

    // --- extrapolation above the calibrated range ---------------------------

    #[test]
    fn extrapolates_above_the_top_point() {
        let mut cm = make_manager(7, 0.15);
        cm.set_calibration(&[(2.9, 849.0), (3.5, 1473.0)]);
        let v = cm.voltage_for_power(3000.0).unwrap();
        // slope 1040 W/V -> 3.5V + 1527W/1040 = 4.97V
        assert!((v - 4.97).abs() < 0.02);
        assert!(v > 3.5);
    }

    #[test]
    fn extrapolation_is_capped_at_max_voltage() {
        let mut cm = make_manager(7, 0.15);
        cm.set_calibration(&[(2.9, 849.0), (3.5, 1473.0)]);
        assert!((cm.voltage_for_power(99_000.0).unwrap() - 10.0).abs() < 1e-9);
    }

    #[test]
    fn flat_top_segment_does_not_extrapolate() {
        let mut cm = make_manager(7, 0.15);
        cm.set_calibration(&[(2.9, 1473.0), (3.5, 1473.0)]);
        assert!((cm.voltage_for_power(3000.0).unwrap() - 3.5).abs() < 1e-9);
    }

    // --- monotonic projection ------------------------------------------------

    #[test]
    fn dip_is_pooled_away() {
        let mut cm = make_manager(7, 0.15);
        cm.set_calibration(&[(2.9, 849.0), (3.7, 1800.0), (3.8, 1600.0), (3.9, 1700.0)]);
        let watts: Vec<f64> = cm.monotonic_points().iter().map(|p| p.watts).collect();
        assert!(watts.windows(2).all(|w| w[0] <= w[1]));
        assert!((watts[0] - 849.0).abs() < 1e-9);
        assert!((watts[1] - 1700.0).abs() < 1e-6);
        assert!((watts[2] - 1700.0).abs() < 1e-6);
        assert!((watts[3] - 1700.0).abs() < 1e-6);
    }

    #[test]
    fn pooling_is_weighted_by_reading_count() {
        let mut cm = make_manager(7, 0.15);
        cm.set_calibration(&[(3.0, 1000.0), (3.1, 900.0)]);
        cm.data.points[0].count = 9;
        cm.data.points[1].count = 1;
        let level = cm.monotonic_points()[0].watts;
        assert!((level - (1000.0 * 9.0 + 900.0) / 10.0).abs() < 1e-6);
    }

    #[test]
    fn lookups_use_the_projection() {
        let mut cm = make_manager(7, 0.15);
        cm.set_calibration(&[(2.9, 849.0), (3.7, 1800.0), (3.8, 1600.0)]);
        assert!(cm.expected_power(3.8).unwrap() >= cm.expected_power(3.7).unwrap());
        assert!(cm.voltage_for_power(1500.0).unwrap() <= cm.voltage_for_power(1700.0).unwrap());
    }

    #[test]
    fn flat_pooled_tail_still_extrapolates() {
        let mut cm = make_manager(7, 0.15);
        cm.set_calibration(&[(2.9, 849.0), (3.6, 1700.0), (3.7, 1800.0), (3.8, 1600.0)]);
        let tail: Vec<f64> = cm
            .monotonic_points()
            .iter()
            .rev()
            .take(2)
            .map(|p| p.watts)
            .collect();
        assert_eq!(tail[0], tail[1]);
        assert!(cm.voltage_for_power(3000.0).unwrap() > cm.monotonic_points().last().unwrap().voltage);
    }
}

#[cfg(test)]
mod parity_sweep {
    use super::*;

    /// Parity sweep over the PRODUCTION calibration curve (gitignored copy
    /// at config/calibration.json). Prints the same sweep as the
    /// tests-py parity script for diffing.
    ///
    /// Run: cargo test -p heatingrod --bin heatingrod parity_sweep -- --ignored --nocapture
    #[test]
    #[ignore]
    fn parity_sweep() {
        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../../config/calibration.json");
        let mut cm = CalibrationManager::new(path, 7, 0.30, 0.3, 10.0);
        assert!(cm.load(), "production calibration must load");
        println!("rust: {} points", cm.data.points.len());
        for p in &cm.data.points {
            println!("P {:.4} {:.2} {}", p.voltage, p.watts, p.count);
        }
        for w in [0.0, 500.0, 1000.0, 1500.0, 2000.0, 3000.0, 4000.0, 5000.0, 6000.0, 7500.0] {
            match cm.voltage_for_power(w) {
                Some(v) => println!("V {w} {v:.4}"),
                None => println!("V {w} None"),
            }
        }
        for v in [0.5, 1.0, 1.5, 2.0, 2.5, 3.0, 3.5, 4.0, 5.0, 6.0] {
            match cm.expected_power(v) {
                Some(p) => println!("E {v} {p:.2}"),
                None => println!("E {v} None"),
            }
        }
    }
}
