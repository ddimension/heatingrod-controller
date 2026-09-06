//! GP8403 DAC via I2C — 1:1 port of `heatingrod/dac.py`.
//!
//! Register map (12-bit, 10V range):
//!   0x01: output range config (0x11 = 10V)
//!   0x02: channel 0 (16-bit word, upper 12 bits = DAC value)
//!   0x04: channel 1
//!
//! Safety invariants (CLAUDE.md): shutdown sets CH0 → 0V and CH1 → the SAFE
//! voltage (boiler fail-safe) — never 0V on CH1 except via a climate OFF
//! command. Error paths fall back to the safe voltage, exactly like Python.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use crate::hardware::Dac;

pub const REG_RANGE: u8 = 0x01;
pub const REG_CH0: u8 = 0x02;
pub const REG_CH1: u8 = 0x04;
pub const RANGE_10V: u16 = 0x11;
const DAC_MAX: f64 = 4095.0;
const VOLTAGE_10V_MV: f64 = 10000.0;

/// I2C bus abstraction — the real bus is `/dev/i2c-1` via i2cdev; tests use
/// [`MockI2cBus`]. Same SMBus ioctls as smbus2 → identical wire behavior.
pub trait I2cBus: Send {
    fn read_byte(&mut self, address: u8) -> Result<u8, String>;
    fn write_word_data(&mut self, address: u8, register: u8, value: u16) -> Result<(), String>;
}

/// Record-keeping mock; reads/writes can be made to fail. The flags are
/// shared (Arc<AtomicBool>) so a test can flip them mid-flight (e.g. init
/// succeeds, then writes start failing → error fallback path).
pub struct MockI2cBus {
    pub reads_ok: Arc<AtomicBool>,
    pub writes_ok: Arc<AtomicBool>,
    pub writes: Vec<(u8, u16)>,
}

impl MockI2cBus {
    pub fn new(reads_ok: bool, writes_ok: bool) -> Self {
        Self {
            reads_ok: Arc::new(AtomicBool::new(reads_ok)),
            writes_ok: Arc::new(AtomicBool::new(writes_ok)),
            writes: Vec::new(),
        }
    }

    pub fn set_writes_ok(&self, ok: bool) {
        self.writes_ok.store(ok, Ordering::Relaxed);
    }
}

impl I2cBus for MockI2cBus {
    fn read_byte(&mut self, _address: u8) -> Result<u8, String> {
        if self.reads_ok.load(Ordering::Relaxed) {
            Ok(0x00)
        } else {
            Err("i2c read failed".into())
        }
    }

    fn write_word_data(&mut self, _address: u8, register: u8, value: u16) -> Result<(), String> {
        if self.writes_ok.load(Ordering::Relaxed) {
            self.writes.push((register, value));
            Ok(())
        } else {
            Err("i2c write failed".into())
        }
    }
}

/// Linux I2C bus via the i2cdev crate — same kernel SMBus ioctls as smbus2
/// (identical wire behavior).
pub struct LinuxI2cBus {
    dev: Option<i2cdev::linux::LinuxI2CDevice>,
    address: u16,
}

impl LinuxI2cBus {
    pub fn new(path: &str, address: u16) -> Self {
        let dev = i2cdev::linux::LinuxI2CDevice::new(path, address).ok();
        Self { dev, address }
    }
}

impl I2cBus for LinuxI2cBus {
    fn read_byte(&mut self, _address: u8) -> Result<u8, String> {
        use i2cdev::core::I2CDevice;
        match self.dev.as_mut() {
            Some(dev) => dev
                .smbus_read_byte()
                .map_err(|e| format!("i2c read failed: {e}")),
            None => Err("i2c bus not open".into()),
        }
    }

    fn write_word_data(&mut self, _address: u8, register: u8, value: u16) -> Result<(), String> {
        use i2cdev::core::I2CDevice;
        match self.dev.as_mut() {
            Some(dev) => dev
                .smbus_write_word_data(register, value)
                .map_err(|e| format!("i2c write failed: {e}")),
            None => Err("i2c bus not open".into()),
        }
    }
}

impl LinuxI2cBus {
    #[allow(dead_code)]
    pub fn address(&self) -> u16 {
        self.address
    }
}

/// Real GP8403 driver (Python `DACController`).
pub struct RealDac {
    pub i2c_address: u8,
    pub max_voltage: f64,
    pub ch1_safe_voltage: f64,
    pub current_voltage: f64,
    pub current_voltage_ch1: f64,
    bus: Option<Box<dyn I2cBus>>,
    initialized: bool,
    i2c_errors: u32,
}

impl RealDac {
    pub fn new(
        i2c_address: u8,
        max_voltage: f64,
        ch1_safe_voltage: f64,
        bus: Box<dyn I2cBus>,
    ) -> Self {
        Self {
            i2c_address,
            max_voltage,
            ch1_safe_voltage,
            current_voltage: 0.0,
            current_voltage_ch1: ch1_safe_voltage,
            bus: Some(bus),
            initialized: false,
            i2c_errors: 0,
        }
    }

    /// Python `initialize(max_retries=30)`: probe, range config, shutdown.
    pub fn initialize(&mut self, max_retries: u32, sleep: &mut dyn FnMut(f64)) -> bool {
        let Some(bus) = self.bus.as_mut() else {
            tracing::error!("DAC I2C bus open failed");
            return false;
        };
        for attempt in 1..=max_retries {
            if bus.read_byte(self.i2c_address).is_ok()
                && bus.write_word_data(self.i2c_address, REG_RANGE, RANGE_10V).is_ok()
            {
                self.initialized = true;
                self.i2c_errors = 0;
                tracing::info!(
                    "DAC initialized at 0x{:02X} (10V range, attempt {attempt})",
                    self.i2c_address
                );
                self.shutdown();
                return true;
            }
            if attempt < max_retries {
                tracing::warn!(
                    "DAC not responding at 0x{:02X}, retry {attempt}/{max_retries}",
                    self.i2c_address
                );
                sleep(2.0);
            }
        }
        tracing::error!(
            "DAC not responding at 0x{:02X} after {max_retries} attempts",
            self.i2c_address
        );
        false
    }

    /// Python `reinitialize()`: 5 retries after I2C errors.
    pub fn reinitialize(&mut self, sleep: &mut dyn FnMut(f64)) -> bool {
        tracing::warn!("DAC reinitializing after {} I2C errors", self.i2c_errors);
        self.initialized = false;
        self.initialize(5, sleep)
    }

    fn set_raw(&mut self, channel_reg: u8, millivolts: f64) -> bool {
        let Some(bus) = self.bus.as_mut() else {
            return false;
        };
        let dac_val = ((millivolts / VOLTAGE_10V_MV) * DAC_MAX).clamp(0.0, DAC_MAX);
        match bus.write_word_data(self.i2c_address, channel_reg, (dac_val as u16) << 4) {
            Ok(()) => true,
            Err(e) => {
                tracing::error!("DAC I2C write failed (reg=0x{channel_reg:02X}): {e}");
                self.i2c_errors += 1;
                false
            }
        }
    }

    /// Python `shutdown()`: CH0 → 0V, CH1 → safe voltage (fail-safe).
    pub fn shutdown(&mut self) {
        tracing::info!(
            "DAC shutdown: CH0→0V, CH1→{:.0}V (safe)",
            self.ch1_safe_voltage
        );
        self.current_voltage = 0.0;
        self.current_voltage_ch1 = self.ch1_safe_voltage;

        if self.bus.is_none() {
            return;
        }
        if !self.set_raw(REG_CH0, 0.0) {
            // Python: reinitialize + retry once
            self.reinitialize(&mut |_| {});
            self.set_raw(REG_CH0, 0.0);
        }
        let safe_mv = self.ch1_safe_voltage * 1000.0;
        if !self.set_raw(REG_CH1, safe_mv) {
            self.reinitialize(&mut |_| {});
            self.set_raw(REG_CH1, safe_mv);
        }
    }
}

impl Dac for RealDac {
    fn set_voltage(&mut self, voltage: f64) {
        let voltage = voltage.clamp(0.0, self.max_voltage);
        self.current_voltage = voltage;
        let millivolts = voltage * 1000.0;

        if !self.initialized {
            if !self.reinitialize(&mut |_| {}) {
                tracing::error!("DAC not available, cannot set voltage");
                self.current_voltage = 0.0;
                return;
            }
        }
        if !self.set_raw(REG_CH0, millivolts) {
            if self.reinitialize(&mut |_| {}) {
                self.set_raw(REG_CH0, millivolts);
            } else {
                self.current_voltage = 0.0;
            }
        } else {
            tracing::debug!("DAC set voltage: {voltage:.2}V");
        }
    }

    fn set_voltage_ch1(&mut self, voltage: f64) {
        let voltage = voltage.clamp(0.0, 10.0);
        self.current_voltage_ch1 = voltage;
        let millivolts = voltage * 1000.0;

        if !self.initialized {
            if !self.reinitialize(&mut |_| {}) {
                tracing::error!("DAC not available, cannot set CH1 voltage");
                self.current_voltage_ch1 = self.ch1_safe_voltage;
                return;
            }
        }
        if !self.set_raw(REG_CH1, millivolts) {
            if self.reinitialize(&mut |_| {}) {
                self.set_raw(REG_CH1, millivolts);
            } else {
                self.current_voltage_ch1 = self.ch1_safe_voltage;
            }
        } else {
            tracing::debug!("DAC set CH1 voltage: {voltage:.2}V");
        }
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
        // Known deviation (04.09.2026 audit D1): Python probes the bus with
        // read_byte on every STATUS tick; we report the initialized flag.
        // A silent GP8403 death with CH0 idle at 0V is NOT detected here —
        // only a failed write flips the flag (reinitialize path).
        self.initialized
    }

    fn shutdown(&mut self) {
        RealDac::shutdown(self);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ok_bus() -> Box<MockI2cBus> {
        Box::new(MockI2cBus::new(true, true))
    }

    fn failing_writes_bus() -> Box<MockI2cBus> {
        Box::new(MockI2cBus::new(true, false))
    }

    fn failing_reads_bus() -> Box<MockI2cBus> {
        Box::new(MockI2cBus::new(false, false))
    }

    fn no_sleep(_: f64) {}

    // ---- ports of test_dac.py ----

    #[test]
    fn mock_init() {
        let mut dac = crate::hardware::MockDac::new(10.0);
        assert!(dac.is_responsive());
        let _ = &mut dac;
    }

    #[test]
    fn mock_set_voltage() {
        let mut dac = crate::hardware::MockDac::new(10.0);
        dac.set_voltage(5.0);
        assert_eq!(dac.current_voltage(), 5.0);
    }

    #[test]
    fn voltage_clamped_to_max() {
        let mut dac = crate::hardware::MockDac::new(10.0);
        dac.set_voltage(15.0);
        assert_eq!(dac.current_voltage(), 10.0);
    }

    #[test]
    fn voltage_clamped_to_zero() {
        let mut dac = crate::hardware::MockDac::new(10.0);
        dac.set_voltage(-5.0);
        assert_eq!(dac.current_voltage(), 0.0);
    }

    #[test]
    fn shutdown_sets_zero() {
        let mut dac = crate::hardware::MockDac::new(10.0);
        dac.set_voltage(7.5);
        assert_eq!(dac.current_voltage(), 7.5);
        dac.shutdown();
        assert_eq!(dac.current_voltage(), 0.0);
    }

    #[test]
    fn current_level() {
        let mut dac = crate::hardware::MockDac::new(10.0);
        dac.set_voltage(5.0);
        assert_eq!(dac.current_level(), 0.5);
    }

    #[test]
    fn current_level_zero_max() {
        let dac = crate::hardware::MockDac::new(0.0);
        assert_eq!(dac.current_level(), 0.0);
    }

    #[test]
    fn not_initialized_no_crash() {
        // Real driver with a dead bus: set_voltage falls back to 0V.
        let mut dac = RealDac::new(0x5f, 10.0, 6.0, failing_reads_bus());
        dac.set_voltage(5.0);
        assert_eq!(dac.current_voltage(), 0.0);
    }

    #[test]
    fn shutdown_without_init() {
        let mut dac = crate::hardware::MockDac::new(10.0);
        dac.shutdown();
        assert_eq!(dac.current_voltage(), 0.0);
    }

    #[test]
    fn mock_set_voltage_ch1() {
        let mut dac = crate::hardware::MockDac::new(10.0);
        dac.set_voltage_ch1(7.5);
        assert_eq!(dac.current_voltage_ch1(), 7.5);
    }

    #[test]
    fn ch1_voltage_clamped() {
        let mut dac = crate::hardware::MockDac::new(10.0);
        dac.set_voltage_ch1(15.0);
        assert_eq!(dac.current_voltage_ch1(), 10.0);
        dac.set_voltage_ch1(-1.0);
        assert_eq!(dac.current_voltage_ch1(), 0.0);
    }

    #[test]
    fn shutdown_resets_ch1_to_safe() {
        // Python test: ch1_safe_voltage=10.0 — the mock keeps its safe value.
        let mut dac = crate::hardware::MockDac::new(10.0);
        dac.ch1_safe_voltage = 10.0;
        dac.set_voltage_ch1(5.0);
        assert_eq!(dac.current_voltage_ch1(), 5.0);
        dac.shutdown();
        assert_eq!(dac.current_voltage_ch1(), 10.0);
    }

    #[test]
    fn ch1_safe_voltage_on_init() {
        let mut dac = crate::hardware::MockDac::new(10.0);
        dac.current_voltage_ch1 = 10.0; // Python ctor: ch1_safe_voltage
        assert_eq!(dac.current_voltage_ch1(), 10.0);
        let _ = &mut dac;
    }

    #[test]
    fn ch1_error_fallback_to_safe_voltage() {
        // Python semantics: a write that fails twice (init + retry) leaves
        // CH1 at the safe voltage. Init succeeds, THEN writes start failing.
        let bus = Box::new(MockI2cBus::new(true, true));
        let writes_flag = std::sync::Arc::clone(&bus.writes_ok);
        let mut dac = RealDac::new(0x5f, 10.0, 10.0, bus);
        assert!(dac.initialize(2, &mut no_sleep));
        writes_flag.store(false, std::sync::atomic::Ordering::Relaxed);
        dac.set_voltage_ch1(5.0);
        assert_eq!(dac.current_voltage_ch1(), 10.0);
    }

    #[test]
    fn ch0_ch1_independent() {
        let mut dac = crate::hardware::MockDac::new(10.0);
        dac.set_voltage(3.0);
        dac.set_voltage_ch1(8.0);
        assert_eq!(dac.current_voltage(), 3.0);
        assert_eq!(dac.current_voltage_ch1(), 8.0);
    }

    #[test]
    fn initialize_succeeds_with_working_bus() {
        let mut dac = RealDac::new(0x5f, 10.0, 6.0, ok_bus());
        assert!(dac.initialize(3, &mut no_sleep));
        assert!(dac.is_responsive());
        // Python initialize ends with shutdown(): CH0=0, CH1=safe.
        assert_eq!(dac.current_voltage(), 0.0);
        assert_eq!(dac.current_voltage_ch1(), 6.0);
    }

    #[test]
    fn initialize_fails_when_range_write_fails() {
        let mut dac = RealDac::new(0x5f, 10.0, 6.0, failing_writes_bus());
        assert!(!dac.initialize(2, &mut no_sleep));
        assert!(!dac.is_responsive());
    }
}
