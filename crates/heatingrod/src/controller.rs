//! Controller decision logic — 1:1 port of `heatingrod/controller.py`.
//!
//! Safety invariants (CLAUDE.md, must never regress):
//! - DAC CH0 → 0V on every error/stale-sensor/HA-disconnect path
//! - heat only on PV surplus (hysteresis power_limit_start/stop)
//! - internal cutoff detection (smooth < 50% of calibration-expected power)
//! - tank temp null → warn once and continue (analog limiter protects)
//! - rod power null while heating → shut down
//!
//! The tests are ports of `tests/test_controller.py` (23 cases).

use crate::calibration::CalibrationManager;
use crate::config::Config;
use crate::hardware::{ControllerIo, Dac, SensorRole};

pub struct Controller {
    pub config: Config,
    pub dac: Box<dyn Dac>,
    pub calibration: CalibrationManager,

    pub running: bool,
    pub heating_active: bool,
    pub target_power: f64,
    pub last_adjustment_ts: f64,
    pub calibrating: bool,
    pub internal_cutoff: bool,
    pub internal_cutoff_since: f64,
    pub tank_temp_warned: bool,
    pub last_status_log_ts: f64,

    // Feedback delay / settling measurement
    pub dac_change_ts: f64,
    pub dac_change_power: f64,
    pub dac_change_voltage: f64,
    pub feedback_delay_ewma: f64,
    pub feedback_delay_count: u32,

    // Step mode: timer-based settle after each DAC change
    pub settling: bool,
    pub settle_start_ts: f64,

    // EWMA of DS100 power — smooths the thyristor burst pattern (τ≈3s)
    pub power_ewma: f64,
    pub last_esphome_power_push: f64,
    pub last_esphome_full_push: f64,
    pub start_time: f64,
    pub overdraw_energy_wh: f64,
    pub last_overdraw_ts: f64,
    pub grid_source: String,
    pub co2_start_energy: Option<f64>,

    /// When set, calibration sleeps abort early if this flag flips —
    /// Python cancels the calibration sleep on shutdown immediately, the
    /// blocking port must not hold the DAC up for the rest of a 30s step
    /// (04.09.2026 audit S2).
    pub stop_flag: Option<std::sync::Arc<std::sync::atomic::AtomicBool>>,
    /// Optional per-second hook during calibration steps — the production
    /// wiring drains the climate command queue here so scheduler commands
    /// are applied between steps instead of waiting out the whole sweep.
    pub calibration_hook: Option<Box<dyn FnMut(&mut Controller)>>,
}

/// EWMA alpha for the thyristor burst pattern (Python `_POWER_ALPHA`).
const POWER_ALPHA: f64 = 0.033;

impl Controller {
    pub fn new(config: Config, dac: Box<dyn Dac>, calibration: CalibrationManager, now: f64) -> Self {
        Self {
            config,
            dac,
            calibration,
            running: false,
            heating_active: false,
            target_power: 0.0,
            last_adjustment_ts: 0.0,
            calibrating: false,
            internal_cutoff: false,
            internal_cutoff_since: 0.0,
            tank_temp_warned: false,
            last_status_log_ts: 0.0,
            dac_change_ts: 0.0,
            dac_change_power: 0.0,
            dac_change_voltage: 0.0,
            feedback_delay_ewma: 0.1,
            feedback_delay_count: 0,
            settling: false,
            settle_start_ts: 0.0,
            power_ewma: 0.0,
            last_esphome_power_push: 0.0,
            last_esphome_full_push: 0.0,
            start_time: now,
            overdraw_energy_wh: 0.0,
            last_overdraw_ts: 0.0,
            grid_source: "none".into(),
            co2_start_energy: None,
            stop_flag: None,
            calibration_hook: None,
        }
    }

    /// Interruptible calibration sleep: 1s slices checking the stop flag,
    /// with the per-second hook (production wiring drains climate commands).
    /// Returns false when aborted by the stop flag.
    fn calibration_sleep(
        &mut self,
        seconds: f64,
        sleep: &mut dyn FnMut(f64),
    ) -> bool {
        let mut remaining = seconds;
        while remaining > 0.0 {
            if let Some(stop) = &self.stop_flag {
                if stop.load(std::sync::atomic::Ordering::Relaxed) {
                    return false;
                }
            }
            if let Some(mut hook) = self.calibration_hook.take() {
                hook(self);
                self.calibration_hook = Some(hook);
            }
            let chunk = remaining.min(1.0);
            sleep(chunk);
            remaining -= chunk;
        }
        true
    }

    /// Python `emergency_shutdown`: DAC → 0V, heating off.
    pub fn emergency_shutdown(&mut self, reason: &str) {
        tracing::warn!("EMERGENCY SHUTDOWN: {reason}");
        self.dac.set_voltage(0.0);
        self.heating_active = false;
        self.target_power = 0.0;
    }

    fn stop_heating(&mut self, reason: &str) {
        tracing::info!("{reason}");
        self.dac.set_voltage(0.0);
        self.heating_active = false;
        self.target_power = 0.0;
    }

    /// Python `_control_loop`. `sleep` executes calibration step waits
    /// (no-op in tests, real sleep in the app).
    pub fn control_loop(&mut self, io: &mut dyn ControllerIo, sleep: &mut dyn FnMut(f64)) {
        let cfg = self.config.clone();

        // Manual switch in HA (freshness 120s like Python)
        let switch_val = io.ha_state(&cfg.ha_entities.heatingrod_switch, 120.0);
        if switch_val.as_deref() == Some("off") {
            if self.heating_active {
                self.stop_heating("Heizstab Schalter is OFF, shutting down");
            }
            return;
        }

        let surplus_override = cfg.control.surplus_override_watts > 0.0;
        let grid_power = if surplus_override {
            -cfg.control.surplus_override_watts
        } else {
            match io.role_state(
                SensorRole::Grid,
                &cfg.ha_entities.grid_power,
                cfg.control.sensor_max_age_seconds,
            ) {
                Some(g) => g,
                None => {
                    if self.heating_active {
                        self.emergency_shutdown("No valid grid power reading");
                    }
                    return;
                }
            }
        };

        let tank_temp = io.role_state(
            SensorRole::TankTemp,
            &cfg.ha_entities.tank_top_temperature,
            cfg.control.sensor_max_age_seconds,
        );
        if tank_temp.is_none() {
            if !self.tank_temp_warned {
                tracing::warn!(
                    "Tank temperature sensor unavailable, continuing without tank limit \
                     (heizstab has analog thermal cutoff)"
                );
                self.tank_temp_warned = true;
            }
        } else {
            self.tank_temp_warned = false;
        }
        if let Some(tank_temp) = tank_temp
            && tank_temp >= cfg.control.tank_temp_limit
        {
            if self.heating_active {
                self.stop_heating(&format!(
                    "Tank temperature {tank_temp:.1}°C >= limit {:.1}°C, shutting down",
                    cfg.control.tank_temp_limit
                ));
            }
            return;
        }

        let heatingrod_power = io.role_state(
            SensorRole::RodPower,
            &cfg.ha_entities.heatingrod_power,
            cfg.control.sensor_max_age_seconds,
        );
        if heatingrod_power.is_none() && self.heating_active {
            tracing::warn!("Heatingrod power sensor unavailable while heating, shutting down");
            self.stop_heating("Heatingrod power sensor unavailable while heating, shutting down");
            return;
        }
        let current_consumption = heatingrod_power.unwrap_or(0.0);

        // Internal cutoff detection (analog thermal limiter, ~67°C): actual
        // power far below the calibration expectation at a driving voltage.
        if self.heating_active
            && let Some(rod) = heatingrod_power
        {
            let voltage = self.dac.current_voltage();
            let expected = self
                .calibration
                .expected_power(voltage)
                .unwrap_or((voltage / cfg.dac.max_voltage) * cfg.control.max_power_watts);
            let smooth = if self.power_ewma > 0.0 {
                self.power_ewma
            } else {
                rod
            };
            let cutoff_detected = voltage >= cfg.control.internal_cutoff_voltage_threshold
                && expected > cfg.control.internal_cutoff_power_threshold
                && smooth < expected * 0.5;

            if cutoff_detected {
                let now = io.now();
                if self.internal_cutoff_since == 0.0 {
                    self.internal_cutoff_since = now;
                } else if now - self.internal_cutoff_since >= cfg.control.internal_cutoff_duration
                {
                    if !self.internal_cutoff {
                        tracing::warn!(
                            "Internal heatingrod cutoff detected: DAC={voltage:.1}V, expected={expected:.0}W, \
                             actual={rod:.0}W for {:.0}s",
                            now - self.internal_cutoff_since
                        );
                        self.internal_cutoff = true;
                        self.stop_heating("Internal cutoff");
                        return;
                    }
                }
            } else {
                self.internal_cutoff_since = 0.0;
            }
        }

        // Recovery from internal cutoff (falls through to normal logic)
        if self.internal_cutoff {
            let rod_temp = io.role_state(
                SensorRole::RodTemp,
                &cfg.ha_entities.heatingrod_temperature,
                cfg.control.sensor_max_age_seconds,
            );
            match rod_temp {
                Some(t) if t < cfg.control.internal_cutoff_recovery_temp => {
                    tracing::info!(
                        "Internal cutoff recovery: heatingrod temp {t:.1}°C < {:.1}°C, resuming",
                        cfg.control.internal_cutoff_recovery_temp
                    );
                    self.internal_cutoff = false;
                    self.internal_cutoff_since = 0.0;
                }
                _ => return,
            }
        }

        let surplus = if surplus_override {
            cfg.control.surplus_override_watts
        } else {
            -grid_power + current_consumption
        };

        if grid_power > -cfg.control.power_limit_stop {
            if self.heating_active {
                self.stop_heating(&format!(
                    "Surplus below stop threshold (grid={grid_power:.0}W), shutting down heater"
                ));
            }
            return;
        }

        if grid_power > cfg.control.power_limit_start && !self.heating_active {
            return;
        }

        let target_power = ((surplus + cfg.control.power_limit_start) * cfg.control.power_use_factor)
            .clamp(0.0, cfg.control.max_power_watts);

        if target_power < 200.0 {
            if self.heating_active {
                self.stop_heating("Target power below minimum");
            }
            return;
        }

        // Step mode: _on_ds100_power handles voltage stepping; here we only
        // manage on/off.
        if cfg.control.mode == "step" {
            let was_heating = self.heating_active;
            self.target_power = target_power;
            self.heating_active = true;
            if !was_heating {
                self.dac.set_voltage(cfg.control.step_initial_voltage);
                self.settling = true;
                self.settle_start_ts = io.now();
                tracing::info!(
                    "Heizstab AN (step): ziel={target_power:.0}W surplus={surplus:.0}W grid={grid_power:.0}W"
                );
            }
            return;
        }

        // Calibration mode
        if self.calibration.needs_recalibration()
            && surplus >= cfg.calibration.min_power_for_calibration
        {
            let rod_fresh = io
                .role_state(
                    SensorRole::RodPower,
                    &cfg.ha_entities.heatingrod_power,
                    cfg.control.sensor_max_age_seconds,
                )
                .is_some();
            if rod_fresh {
                self.run_calibration(io, surplus, sleep);
                return;
            }
        }

        let was_heating = self.heating_active;
        self.target_power = target_power;
        self.heating_active = true;
        if !was_heating {
            let mut init_voltage = self.calibration.voltage_for_power(target_power);
            if init_voltage.is_none() {
                tracing::warn!("No calibration data for {target_power:.0}W, using linear estimate");
                init_voltage =
                    Some((target_power / cfg.control.max_power_watts) * cfg.dac.max_voltage * 0.5);
            }
            let init_voltage = init_voltage.unwrap().clamp(0.0, cfg.dac.max_voltage);
            self.power_ewma = 0.0;
            self.dac.set_voltage(init_voltage);
            self.settling = true;
            self.settle_start_ts = io.now();
            tracing::info!(
                "Heizstab AN (cal): ziel={target_power:.0}W initial={init_voltage:.2}V \
                 surplus={surplus:.0}W grid={grid_power:.0}W"
            );
        }
    }

    /// Python `_on_ds100_power` (~100ms): overdraw tracking, 1s push
    /// throttle, EWMA smoothing, settle timer, ramp + feedback correction.
    pub fn on_ds100_power(&mut self, io: &mut dyn ControllerIo, power: f64) {
        let now = io.now();
        let cfg = self.config.clone();

        // Track grid overdraw: rod draws from grid instead of PV
        let grid = io.role_state(SensorRole::Grid, &cfg.ha_entities.grid_power, 5.0);
        let overdraw_w = match grid {
            Some(g) if g > 0.0 && power > 0.0 => {
                let od = g.min(power);
                if self.last_overdraw_ts > 0.0 {
                    let dt_h = (now - self.last_overdraw_ts) / 3600.0;
                    self.overdraw_energy_wh += od * dt_h;
                }
                od
            }
            _ => 0.0,
        };
        self.last_overdraw_ts = now;

        // Push power + overdraw to ESPHome subscribers (1s throttle)
        if now - self.last_esphome_power_push >= 1.0 {
            io.push_sensor("ds100_power", power);
            io.push_sensor("ctrl_overdraw", overdraw_w);
            self.last_esphome_power_push = now;
        }

        if !self.heating_active || self.calibrating {
            self.power_ewma = 0.0; // reinitialize fresh on next heating start
            return;
        }

        // EWMA smoothing (α≈0.033, τ≈3s) for the burst pattern
        if self.power_ewma == 0.0 {
            self.power_ewma = power;
        } else {
            self.power_ewma = POWER_ALPHA * power + (1.0 - POWER_ALPHA) * self.power_ewma;
        }
        let power_smooth = self.power_ewma;

        // Settle timer: wait ≥1 full burst cycle after each DAC change
        if self.settling {
            let elapsed = now - self.settle_start_ts;
            if elapsed < cfg.control.step_settle_time {
                return;
            }
            self.settling = false;
            tracing::debug!(
                "Settled: {:.0}ms smooth={power_smooth:.0}W raw={power:.0}W",
                elapsed * 1000.0
            );
        }

        if self.target_power <= 0.0 {
            return;
        }

        // Phase 1 — open-loop ramp (step mode only)
        if cfg.control.mode == "step" {
            let v_est = (self.target_power / cfg.control.max_power_watts) * cfg.dac.max_voltage;
            let v_ramp_ceil = v_est * cfg.control.step_ramp_target;
            let current = self.dac.current_voltage();
            if current < v_ramp_ceil {
                let ramp_step = cfg.control.step_ramp_rate * cfg.control.step_interval;
                let new_v = (current + ramp_step).min(v_ramp_ceil);
                self.dac.set_voltage(new_v);
                self.last_adjustment_ts = now;
                self.settling = true;
                self.settle_start_ts = now;
                tracing::debug!(
                    "Ramp: {current:.2}V → {new_v:.2}V (ceil={v_ramp_ceil:.2}V target={:.0}W)",
                    self.target_power
                );
                return;
            }
        }

        // Phase 2 — closed-loop correction
        let error = self.target_power - power_smooth;
        if error.abs() <= cfg.control.feedback_tolerance_watts {
            return;
        }

        // Calibration mode: skip upward correction while thermal limiter active
        if cfg.control.mode != "step" && error > 0.0 {
            let cur_v = self.dac.current_voltage();
            if cur_v >= cfg.control.internal_cutoff_voltage_threshold {
                let expected = self
                    .calibration
                    .expected_power(cur_v)
                    .unwrap_or((cur_v / cfg.dac.max_voltage) * cfg.control.max_power_watts);
                if expected > cfg.control.internal_cutoff_power_threshold
                    && power_smooth < expected * 0.5
                {
                    tracing::debug!(
                        "Feedback: thermal limiter active ({cur_v:.1}V → {power_smooth:.0}W smooth, \
                         expected {expected:.0}W), skip upward"
                    );
                    return;
                }
            }
        }

        let correction = (error / cfg.control.max_power_watts)
            * cfg.dac.max_voltage
            * cfg.control.feedback_damping;
        let correction = correction.clamp(
            -cfg.control.max_voltage_step,
            cfg.control.max_voltage_step,
        );
        let voltage =
            (self.dac.current_voltage() + correction).clamp(0.0, cfg.dac.max_voltage);

        // Learn the point we are about to LEAVE, not the one we are moving to
        // (power_smooth belongs to the current voltage; the new one takes
        // effect only after the settle timer).
        if cfg.control.mode != "step" {
            self.calibration.update(self.dac.current_voltage(), power_smooth);
        }

        self.dac.set_voltage(voltage);
        self.last_adjustment_ts = now;
        self.settling = true;
        self.settle_start_ts = now;

        tracing::debug!(
            "Feedback: target={:.0}W smooth={power_smooth:.0}W raw={power:.0}W err={error:.0}W \
             corr={correction:.2}V → {voltage:.2}V",
            self.target_power
        );
    }

    /// Python `_run_calibration`: stepped sweep with abort rules.
    pub fn run_calibration(
        &mut self,
        io: &mut dyn ControllerIo,
        available_surplus: f64,
        sleep: &mut dyn FnMut(f64),
    ) {
        if self.calibrating {
            return;
        }
        let cfg = self.config.clone();
        let max_voltage = ((available_surplus / cfg.control.max_power_watts) * cfg.dac.max_voltage)
            .min(cfg.dac.max_voltage);
        // A sweep that stays inside the thyristor dead band measures 0W at
        // every step and stores an all-zero curve (incident 2026-08-20).
        if max_voltage < cfg.calibration.min_sweep_voltage {
            tracing::info!(
                "Calibration skipped: surplus {available_surplus:.0}W only spans {max_voltage:.2}V \
                 (< {:.2}V), every step would sit in the thyristor dead band",
                cfg.calibration.min_sweep_voltage
            );
            return;
        }

        self.calibrating = true;
        tracing::info!("Starting calibration with {available_surplus:.0}W available surplus");

        let voltages = self
            .calibration
            .get_calibration_voltages(cfg.calibration.steps, max_voltage);

        let mut points: Vec<(f64, f64)> = Vec::new();
        for voltage in voltages {
            if !self.running {
                break;
            }
            let grid = io.role_state(
                SensorRole::Grid,
                &cfg.ha_entities.grid_power,
                cfg.control.sensor_max_age_seconds,
            );
            let rod_now = if self.power_ewma > 0.0 { self.power_ewma } else { 0.0 };
            if grid.is_none() || grid.unwrap() > cfg.control.power_limit_stop {
                tracing::warn!(
                    "Calibration aborted: insufficient surplus (grid={:.0}W rod={rod_now:.0}W)",
                    grid.unwrap_or(0.0)
                );
                break;
            }

            self.dac.set_voltage(voltage);
            self.heating_active = true;
            tracing::info!(
                "Calibration: testing {voltage:.1}V, waiting {:.0}s...",
                cfg.calibration.step_duration
            );

            if !self.calibration_sleep(cfg.calibration.step_duration, sleep) {
                tracing::warn!("Calibration aborted: shutdown requested");
                break;
            }

            // EWMA smoothed power (accounts for the burst pattern)
            let actual = if self.power_ewma > 0.0 {
                Some(self.power_ewma)
            } else {
                io.role_state(
                    SensorRole::RodPower,
                    &cfg.ha_entities.heatingrod_power,
                    cfg.control.sensor_max_age_seconds,
                )
            };
            if let Some(actual) = actual
                && actual >= 0.0
            {
                if voltage >= 3.0 && actual < cfg.control.internal_cutoff_power_threshold {
                    tracing::warn!(
                        "Calibration aborted: internal cutoff detected ({voltage:.1}V → {actual:.0}W)"
                    );
                    break;
                }
                if voltage > CalibrationManager::MIN_MEANINGFUL_VOLTAGE
                    && actual <= CalibrationManager::ZERO_WATTS
                {
                    tracing::warn!(
                        "Calibration aborted: no output at {voltage:.1}V ({actual:.0}W) — \
                         thermal limiter active or heatingrod not powered"
                    );
                    break;
                }
                points.push((voltage, actual));
                tracing::info!("Calibration point: {voltage:.1}V = {actual:.0}W");
            }
        }

        // finally (Python): DAC to 0V, flags reset — no matter what.
        self.dac.set_voltage(0.0);
        self.heating_active = false;
        self.calibrating = false;
        self.target_power = 0.0;

        if points.len() >= 2 {
            self.calibration.set_calibration(&points);
            tracing::info!("Calibration completed with {} points", points.len());
        } else {
            tracing::warn!("Calibration failed: only {} valid points", points.len());
        }
    }

    /// Python `_run_max_calibration`: thermal-peak detection.
    pub fn run_max_calibration(
        &mut self,
        io: &mut dyn ControllerIo,
        available_surplus: f64,
        sleep: &mut dyn FnMut(f64),
    ) {
        if self.calibrating {
            return;
        }
        let cfg = self.config.clone();
        self.calibrating = true;

        let max_voltage = ((available_surplus / cfg.control.max_power_watts) * cfg.dac.max_voltage)
            .min(cfg.dac.max_voltage);
        tracing::info!(
            "Thermal peak calibration: ramping to {max_voltage:.2}V (surplus={available_surplus:.0}W)"
        );

        let mut peak_power = 0.0;
        let peak_voltage = max_voltage;
        let mut consecutive_drops = 0u32;
        let mut prev_power: Option<f64> = None;
        let mut found_peak = false;
        let mut elapsed = 0.0;
        let poll_interval = 2.0;
        let max_wait = 600.0;

        self.dac.set_voltage(max_voltage);
        self.heating_active = true;

        while elapsed < max_wait && self.running {
            if !self.calibration_sleep(poll_interval, sleep) {
                tracing::warn!("Thermal cal aborted: shutdown requested");
                break;
            }
            elapsed += poll_interval;

            let grid = io.role_state(
                SensorRole::Grid,
                &cfg.ha_entities.grid_power,
                cfg.control.sensor_max_age_seconds,
            );
            // Python quirk (ported as-is): abort when grid > -power_limit_stop.
            if grid.is_none() || grid.unwrap() > -cfg.control.power_limit_stop {
                tracing::warn!("Thermal cal aborted: insufficient surplus at {elapsed:.0}s elapsed");
                break;
            }

            let actual = io.role_state(
                SensorRole::RodPower,
                &cfg.ha_entities.heatingrod_power,
                cfg.control.sensor_max_age_seconds,
            );
            let Some(actual) = actual else {
                consecutive_drops = 0;
                prev_power = None;
                continue;
            };

            if actual > peak_power {
                peak_power = actual;
                consecutive_drops = 0;
                tracing::debug!("Thermal cal: new peak {peak_power:.0}W at {elapsed:.0}s");
            } else if let Some(prev) = prev_power
                && actual < prev - 50.0
            {
                consecutive_drops += 1;
                tracing::debug!("Thermal cal: drop {consecutive_drops}/3 ({prev:.0}W→{actual:.0}W)");
                if consecutive_drops >= 3 {
                    tracing::info!(
                        "Thermal cal: limiter peak confirmed at {peak_power:.0}W ({peak_voltage:.2}V)"
                    );
                    found_peak = true;
                    break;
                }
            } else {
                consecutive_drops = 0;
            }
            prev_power = Some(actual);
        }

        // finally (Python)
        self.dac.set_voltage(0.0);
        self.heating_active = false;
        self.calibrating = false;
        self.target_power = 0.0;

        if found_peak && peak_power >= cfg.calibration.min_power_for_calibration {
            self.calibration
                .set_calibration(&[(0.0, 0.0), (peak_voltage, peak_power)]);
            tracing::info!("Thermal peak cal complete: {peak_voltage:.2}V → {peak_power:.0}W");
        } else {
            tracing::warn!(
                "Thermal peak cal incomplete: peak={peak_power:.0}W found={found_peak}, \
                 falling back to stepped"
            );
            self.run_calibration(io, available_surplus, sleep);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hardware::{MockDac, MockEnv};
    use std::path::PathBuf;

    fn test_config() -> Config {
        let mut cfg = Config::default();
        cfg.dac.max_voltage = 10.0;
        cfg.dac.mock = true;
        cfg.ha_entities.grid_power = "sensor.grid_power".into();
        cfg.ha_entities.heatingrod_power = "sensor.heatingrod_power".into();
        cfg.ha_entities.heatingrod_temperature = "sensor.heatingrod_temperature".into();
        cfg.ha_entities.tank_top_temperature = "sensor.tank_top_temperature".into();
        cfg.ha_entities.pv_dc_power = "sensor.pv_dc_power".into();
        cfg.ha_entities.heatingrod_switch = "input_boolean.heizstab_schalter".into();
        cfg.control.mode = "calibration".into();
        cfg.control.sensor_max_age_seconds = 60.0;
        cfg.control.power_limit_start = -800.0;
        cfg.control.power_limit_stop = -600.0;
        cfg.control.max_power_watts = 7500.0;
        cfg.control.power_use_factor = 0.9;
        cfg.control.tank_temp_limit = 80.0;
        cfg.control.watchdog_interval = 10.0;
        cfg.control.feedback_tolerance_watts = 100.0;
        cfg.control.feedback_damping = 0.3;
        cfg.control.max_voltage_step = 0.5;
        cfg.calibration.file_path = "/tmp/test_calibration.json".into();
        cfg.calibration.ewma_alpha = 0.3;
        cfg.calibration.max_age_days = 7;
        cfg.calibration.deviation_threshold = 0.15;
        cfg.calibration.steps = 6;
        cfg.calibration.step_duration = 30.0;
        cfg.calibration.min_power_for_calibration = 2000.0;
        cfg
    }

    fn make_controller() -> (Controller, MockEnv) {
        let cfg = test_config();
        let env = MockEnv::new();
        let cal = CalibrationManager::new(
            &cfg.calibration.file_path,
            cfg.calibration.max_age_days,
            cfg.calibration.deviation_threshold,
            cfg.calibration.ewma_alpha,
            cfg.dac.max_voltage,
        );
        let ctrl = Controller::new(
            cfg,
            Box::new(MockDac::new(10.0)),
            cal,
            env.clock,
        );
        (ctrl, env)
    }

    fn set_base_sensors(ctrl: &Controller, env: &mut MockEnv) {
        env.set(&ctrl.config.ha_entities.tank_top_temperature, Some(50.0));
        env.set(&ctrl.config.ha_entities.heatingrod_power, Some(0.0));
    }

    fn set_calibration(ctrl: &mut Controller) {
        ctrl.calibration
            .set_calibration(&[(1.0, 1000.0), (5.0, 5000.0)]);
    }

    fn no_sleep(_: f64) {}

    // ---- TestControllerSafety ----

    #[test]
    fn emergency_shutdown_sets_dac_zero() {
        let (mut ctrl, mut env) = make_controller();
        ctrl.dac.set_voltage(5.0);
        assert_eq!(ctrl.dac.current_voltage(), 5.0);
        ctrl.emergency_shutdown("test");
        assert_eq!(ctrl.dac.current_voltage(), 0.0);
        assert!(!ctrl.heating_active);
        let _ = &mut env;
    }

    #[test]
    fn no_ha_connection_stops_heating() {
        let (mut ctrl, mut env) = make_controller();
        ctrl.dac.set_voltage(5.0);
        ctrl.heating_active = true;
        env.subscribers = 0; // HA not connected
        ctrl.emergency_shutdown("HA not connected via ESPHome API");
        assert_eq!(ctrl.dac.current_voltage(), 0.0);
        assert!(!ctrl.heating_active);
    }

    // ---- TestManualSwitch ----

    #[test]
    fn switch_off_stops_heating() {
        let (mut ctrl, mut env) = make_controller();
        set_calibration(&mut ctrl);
        set_base_sensors(&ctrl, &mut env);
        env.set(&ctrl.config.ha_entities.grid_power.clone(), Some(-3000.0));
        ctrl.control_loop(&mut env, &mut no_sleep);
        assert!(ctrl.heating_active);

        env.set_str(&ctrl.config.ha_entities.heatingrod_switch.clone(), Some("off"));
        ctrl.control_loop(&mut env, &mut no_sleep);
        assert!(!ctrl.heating_active);
        assert_eq!(ctrl.dac.current_voltage(), 0.0);
    }

    #[test]
    fn switch_on_allows_heating() {
        let (mut ctrl, mut env) = make_controller();
        set_calibration(&mut ctrl);
        set_base_sensors(&ctrl, &mut env);
        env.set_str(&ctrl.config.ha_entities.heatingrod_switch.clone(), Some("on"));
        env.set(&ctrl.config.ha_entities.grid_power.clone(), Some(-3000.0));
        ctrl.control_loop(&mut env, &mut no_sleep);
        assert!(ctrl.heating_active);
    }

    #[test]
    fn no_switch_state_allows_heating() {
        let (mut ctrl, mut env) = make_controller();
        set_calibration(&mut ctrl);
        set_base_sensors(&ctrl, &mut env);
        env.set(&ctrl.config.ha_entities.grid_power.clone(), Some(-3000.0));
        ctrl.control_loop(&mut env, &mut no_sleep);
        assert!(ctrl.heating_active);
    }

    // ---- TestControllerLogic ----

    #[test]
    fn no_grid_data_no_heating() {
        let (mut ctrl, mut env) = make_controller();
        ctrl.control_loop(&mut env, &mut no_sleep);
        assert_eq!(ctrl.dac.current_voltage(), 0.0);
        assert!(!ctrl.heating_active);
    }

    #[test]
    fn grid_import_no_heating() {
        let (mut ctrl, mut env) = make_controller();
        env.set(&ctrl.config.ha_entities.grid_power.clone(), Some(500.0));
        ctrl.control_loop(&mut env, &mut no_sleep);
        assert_eq!(ctrl.dac.current_voltage(), 0.0);
        assert!(!ctrl.heating_active);
    }

    #[test]
    fn surplus_below_start_no_heating() {
        let (mut ctrl, mut env) = make_controller();
        env.set(&ctrl.config.ha_entities.grid_power.clone(), Some(-700.0));
        ctrl.control_loop(&mut env, &mut no_sleep);
        assert_eq!(ctrl.dac.current_voltage(), 0.0);
        assert!(!ctrl.heating_active);
    }

    #[test]
    fn surplus_above_start_heats() {
        let (mut ctrl, mut env) = make_controller();
        set_calibration(&mut ctrl);
        set_base_sensors(&ctrl, &mut env);
        env.set(&ctrl.config.ha_entities.grid_power.clone(), Some(-3000.0));
        ctrl.control_loop(&mut env, &mut no_sleep);
        assert!(ctrl.dac.current_voltage() > 0.0);
        assert!(ctrl.heating_active);
    }

    #[test]
    fn surplus_drop_stops_heating() {
        let (mut ctrl, mut env) = make_controller();
        set_calibration(&mut ctrl);
        set_base_sensors(&ctrl, &mut env);
        env.set(&ctrl.config.ha_entities.grid_power.clone(), Some(-3000.0));
        ctrl.control_loop(&mut env, &mut no_sleep);
        assert!(ctrl.heating_active);

        env.set(&ctrl.config.ha_entities.grid_power.clone(), Some(-400.0));
        ctrl.control_loop(&mut env, &mut no_sleep);
        assert!(!ctrl.heating_active);
        assert_eq!(ctrl.dac.current_voltage(), 0.0);
    }

    #[test]
    fn tank_temp_limit_stops_heating() {
        let (mut ctrl, mut env) = make_controller();
        set_calibration(&mut ctrl);
        env.set(&ctrl.config.ha_entities.grid_power.clone(), Some(-5000.0));
        env.set(&ctrl.config.ha_entities.heatingrod_power.clone(), Some(0.0));
        env.set(&ctrl.config.ha_entities.tank_top_temperature.clone(), Some(82.0));
        ctrl.control_loop(&mut env, &mut no_sleep);
        assert!(!ctrl.heating_active);
        assert_eq!(ctrl.dac.current_voltage(), 0.0);
    }

    #[test]
    fn stale_grid_triggers_shutdown() {
        let (mut ctrl, mut env) = make_controller();
        set_calibration(&mut ctrl);
        set_base_sensors(&ctrl, &mut env);
        env.set(&ctrl.config.ha_entities.grid_power.clone(), Some(-3000.0));
        ctrl.control_loop(&mut env, &mut no_sleep);
        assert!(ctrl.heating_active);

        env.set_aged(&ctrl.config.ha_entities.grid_power.clone(), Some(-3000.0), 120.0);
        ctrl.control_loop(&mut env, &mut no_sleep);
        assert!(!ctrl.heating_active);
        assert_eq!(ctrl.dac.current_voltage(), 0.0);
    }

    // ---- TestControllerFeedback ----

    #[test]
    fn ds100_feedback_adjusts_voltage() {
        let (mut ctrl, mut env) = make_controller();
        set_calibration(&mut ctrl);
        set_base_sensors(&ctrl, &mut env);
        env.set(&ctrl.config.ha_entities.grid_power.clone(), Some(-4000.0));
        ctrl.control_loop(&mut env, &mut no_sleep);
        let first_voltage = ctrl.dac.current_voltage();
        assert!(first_voltage > 0.0);

        // Seed EWMA and clear settling to allow a correction.
        ctrl.power_ewma = 1500.0;
        ctrl.settling = false;
        ctrl.on_ds100_power(&mut env, 1500.0);
        let second_voltage = ctrl.dac.current_voltage();
        assert_ne!(second_voltage, first_voltage);
    }

    #[test]
    fn no_calibration_uses_conservative() {
        let (mut ctrl, mut env) = make_controller();
        set_base_sensors(&ctrl, &mut env);
        env.set(&ctrl.config.ha_entities.grid_power.clone(), Some(-5000.0));
        // DS100 stale → calibration won't trigger, falls through to the
        // conservative estimate.
        env.set_aged(&ctrl.config.ha_entities.heatingrod_power.clone(), Some(0.0), 120.0);
        ctrl.control_loop(&mut env, &mut no_sleep);
        assert!(ctrl.heating_active);
        assert!(ctrl.dac.current_voltage() > 0.0);
        assert!(ctrl.dac.current_voltage() < 5.0);
    }

    // ---- TestInternalCutoff ----

    #[test]
    fn cutoff_detected_after_duration() {
        let (mut ctrl, mut env) = make_controller();
        set_calibration(&mut ctrl);
        ctrl.config.control.internal_cutoff_duration = 0.0; // immediate for test
        set_base_sensors(&ctrl, &mut env);

        env.set(&ctrl.config.ha_entities.grid_power.clone(), Some(-5000.0));
        ctrl.control_loop(&mut env, &mut no_sleep);
        assert!(ctrl.heating_active);
        assert!(ctrl.dac.current_voltage() > 1.0);

        // DS100 shows almost nothing while the DAC drives → cutoff
        env.set(&ctrl.config.ha_entities.heatingrod_power.clone(), Some(50.0));
        ctrl.internal_cutoff_since = env.clock - 1.0; // pretend it started 1s ago
        ctrl.control_loop(&mut env, &mut no_sleep);
        assert!(ctrl.internal_cutoff);
        assert!(!ctrl.heating_active);
        assert_eq!(ctrl.dac.current_voltage(), 0.0);
    }

    #[test]
    fn no_cutoff_when_power_flows() {
        let (mut ctrl, mut env) = make_controller();
        set_calibration(&mut ctrl);
        set_base_sensors(&ctrl, &mut env);
        env.set(&ctrl.config.ha_entities.grid_power.clone(), Some(-5000.0));
        ctrl.control_loop(&mut env, &mut no_sleep);
        assert!(ctrl.heating_active);

        env.set(&ctrl.config.ha_entities.heatingrod_power.clone(), Some(3000.0));
        env.set(&ctrl.config.ha_entities.grid_power.clone(), Some(-5000.0));
        ctrl.last_adjustment_ts = 0.0;
        ctrl.control_loop(&mut env, &mut no_sleep);
        assert!(ctrl.heating_active);
        assert!(!ctrl.internal_cutoff);
    }

    #[test]
    fn cutoff_recovery_on_temp_drop() {
        let (mut ctrl, mut env) = make_controller();
        set_calibration(&mut ctrl);
        ctrl.internal_cutoff = true;
        set_base_sensors(&ctrl, &mut env);

        // Temp still high → stay in cutoff
        env.set(&ctrl.config.ha_entities.grid_power.clone(), Some(-5000.0));
        env.set(&ctrl.config.ha_entities.heatingrod_temperature.clone(), Some(63.0));
        ctrl.control_loop(&mut env, &mut no_sleep);
        assert!(ctrl.internal_cutoff);

        // Temp drops below recovery threshold → resume
        env.set(&ctrl.config.ha_entities.heatingrod_temperature.clone(), Some(55.0));
        ctrl.control_loop(&mut env, &mut no_sleep);
        assert!(!ctrl.internal_cutoff);

        // Next loop should start heating again
        env.set(&ctrl.config.ha_entities.heatingrod_power.clone(), Some(0.0));
        ctrl.control_loop(&mut env, &mut no_sleep);
        assert!(ctrl.heating_active);
    }

    #[test]
    fn cutoff_no_recovery_without_temp_sensor() {
        let (mut ctrl, mut env) = make_controller();
        set_calibration(&mut ctrl);
        ctrl.internal_cutoff = true;
        set_base_sensors(&ctrl, &mut env);
        env.set(&ctrl.config.ha_entities.grid_power.clone(), Some(-5000.0));
        // No temp sensor data → stay in cutoff
        ctrl.control_loop(&mut env, &mut no_sleep);
        assert!(ctrl.internal_cutoff);
        assert!(!ctrl.heating_active);
    }

    // ---- TestHysteresis ----

    #[test]
    fn no_start_between_limits() {
        let (mut ctrl, mut env) = make_controller();
        env.set(&ctrl.config.ha_entities.grid_power.clone(), Some(-700.0));
        ctrl.control_loop(&mut env, &mut no_sleep);
        assert!(!ctrl.heating_active);
    }

    #[test]
    fn stays_on_between_limits() {
        let (mut ctrl, mut env) = make_controller();
        set_calibration(&mut ctrl);
        set_base_sensors(&ctrl, &mut env);
        env.set(&ctrl.config.ha_entities.grid_power.clone(), Some(-3000.0));
        ctrl.control_loop(&mut env, &mut no_sleep);
        assert!(ctrl.heating_active);

        env.set(&ctrl.config.ha_entities.grid_power.clone(), Some(-700.0));
        env.set(&ctrl.config.ha_entities.heatingrod_power.clone(), Some(2000.0));
        ctrl.control_loop(&mut env, &mut no_sleep);
        assert!(ctrl.heating_active);
    }

    // ---- TestSensorFault ----

    #[test]
    fn tank_temp_null_continues_heating() {
        let (mut ctrl, mut env) = make_controller();
        set_calibration(&mut ctrl);
        set_base_sensors(&ctrl, &mut env);
        env.set(&ctrl.config.ha_entities.grid_power.clone(), Some(-3000.0));
        ctrl.control_loop(&mut env, &mut no_sleep);
        assert!(ctrl.heating_active);

        // Tank temp goes unavailable — continue (analog cutoff protects)
        env.set(&ctrl.config.ha_entities.tank_top_temperature.clone(), None);
        ctrl.control_loop(&mut env, &mut no_sleep);
        assert!(ctrl.heating_active);
    }

    #[test]
    fn tank_temp_null_allows_start() {
        let (mut ctrl, mut env) = make_controller();
        set_calibration(&mut ctrl);
        env.set(&ctrl.config.ha_entities.grid_power.clone(), Some(-3000.0));
        env.set(&ctrl.config.ha_entities.heatingrod_power.clone(), Some(0.0));
        // tank_top_temperature not set → None
        ctrl.control_loop(&mut env, &mut no_sleep);
        assert!(ctrl.heating_active);
    }

    #[test]
    fn rod_power_null_stops_heating() {
        let (mut ctrl, mut env) = make_controller();
        set_calibration(&mut ctrl);
        set_base_sensors(&ctrl, &mut env);
        env.set(&ctrl.config.ha_entities.grid_power.clone(), Some(-3000.0));
        ctrl.control_loop(&mut env, &mut no_sleep);
        assert!(ctrl.heating_active);

        // DS100 goes unavailable → shut down
        env.set(&ctrl.config.ha_entities.heatingrod_power.clone(), None);
        ctrl.control_loop(&mut env, &mut no_sleep);
        assert!(!ctrl.heating_active);
        assert_eq!(ctrl.dac.current_voltage(), 0.0);
    }

    // PathBuf import used for potential state-file tests later.
    #[allow(dead_code)]
    fn unused(_: PathBuf) {}
}
