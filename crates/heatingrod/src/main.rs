//! Heatingrod controller v3 (Rust port) — entry point + wiring.
//!
//! Architecture: the controller decision logic (controller.rs) runs in a
//! dedicated OS thread at a 100ms cadence (Python asyncio parity); the async
//! tokio runtime hosts the ESPHome API server and the ESPHome clients
//! (powerlogger, kaskade). Role sources share state via Arc<Mutex<...>> and
//! fall back to HA entities exactly like the Python getters.

mod bridge;
mod calibration;
mod clients;
mod config;
mod controller;
mod hardware;
mod logging;
mod state;

use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use esphome_api_server::{Command, HaStateEvent, SensorValue};
use tokio::sync::mpsc;

use bridge::Bridge;
use calibration::CalibrationManager;
use clients::{KaskadeClient, KaskadeShared, PlShared, PowerloggerClient};
use config::Config;
use controller::Controller;
use hardware::{
    system_now, ControllerIo, Dac, DS100Poller, DS100Reading, LinuxI2cBus, LinuxSerialPort,
    MockDac, OneWirePoller, RealDac, SensorRole, SlcanTransport, SorelDecoder,
};
use hardware::slcan;

const STATUS_INTERVAL: f64 = 60.0;
const FULL_PUSH_INTERVAL: f64 = 30.0;

/// Operator decision (04.09.2026), deviation from Python: the controller
/// works on its DIRECT connections (powerlogger/DS100/1-Wire), so a HA
/// subscriber loss does NOT trigger an emergency shutdown — only a warning.
/// Python shut down here because its sensors came THROUGH HA. Stale direct
/// data still trips the grid check; the HA-only manual switch and tank
/// limit are simply absent while HA is gone.
///
/// Returns None = continue, Some(reason) = emergency shutdown.
fn watchdog_ha_policy(heating_active: bool, subscribers: usize) -> Option<&'static str> {
    let _ = (heating_active, subscribers);
    None
}

// ---- shared role sources (Arc<Mutex>, read by the controller thread) ----

#[derive(Debug, Default)]
pub struct DsShared {
    pub last: Option<f64>,
    pub ts: f64,
    /// Telemetry for the STATUS line (Python parity).
    pub voltage: Option<f64>,
    pub freq: Option<f64>,
    pub msgs: u64,
    pub errs: u64,
    /// Energy counter (kWh) for the CO2-saved calculation (Python
    /// `r.energy_total_active`).
    pub energy: Option<f64>,
}

impl DsShared {
    fn get(&self, max_age: f64, now: f64) -> Option<f64> {
        let p = self.last?;
        if self.ts == 0.0 || now - self.ts > max_age {
            return None;
        }
        Some(p)
    }
}

#[derive(Debug, Default)]
pub struct OwShared {
    /// name → (temperature, timestamp)
    pub sensors: std::collections::HashMap<String, (f64, f64)>,
    /// Poller telemetry for the STATUS line (Python parity).
    pub total: usize,
    pub errors: u64,
    pub cycle: u64,
    pub cycle_ms: f64,
}

/// CAN bus session stats for the STATUS line (Python parity: cbus=pkts,errs).
pub struct CanShared {
    pub connected: bool,
    pub packets: u64,
    pub errors: u64,
}

impl OwShared {
    fn get(&self, name: &str, max_age: f64, now: f64) -> Option<f64> {
        let (temp, ts) = self.sensors.get(name)?;
        if now - ts > max_age {
            return None;
        }
        Some(*temp)
    }
}

/// ControllerIo backed by the live bridge + role sources with the Python
/// fallback chains (hardware first, HA entity second).
struct BridgeIo {
    bridge: Arc<Mutex<Bridge>>,
    config: Config,
    pl: Arc<Mutex<PlShared>>,
    ds: Arc<Mutex<DsShared>>,
    ow: Arc<Mutex<OwShared>>,
}

impl BridgeIo {
    fn ha_numeric(&self, entity_id: &str, max_age: f64) -> Option<f64> {
        self.bridge
            .lock()
            .expect("bridge lock")
            .server
            .get_ha_state(entity_id, Duration::from_secs_f64(max_age))
            .and_then(|s| s.parse::<f64>().ok())
    }
}

impl ControllerIo for BridgeIo {
    fn now(&self) -> f64 {
        system_now()
    }

    fn ha_state(&self, entity_id: &str, max_age: f64) -> Option<String> {
        self.bridge
            .lock()
            .expect("bridge lock")
            .server
            .get_ha_state(entity_id, Duration::from_secs_f64(max_age))
    }

    fn role_state(&self, role: SensorRole, entity_id: &str, max_age: f64) -> Option<f64> {
        let now = self.now();
        let ha_fallback = self.ha_numeric(entity_id, max_age);
        match role {
            SensorRole::Grid => self
                .pl
                .lock()
                .expect("pl lock")
                .get(max_age, now)
                .or(ha_fallback),
            SensorRole::RodPower => self
                .ds
                .lock()
                .expect("ds lock")
                .get(max_age, now)
                .or(ha_fallback),
            SensorRole::RodTemp => self
                .ow
                .lock()
                .expect("ow lock")
                .get("1-Wire Heizstab", max_age, now)
                .or(ha_fallback),
            SensorRole::TankTemp => self
                .ow
                .lock()
                .expect("ow lock")
                .get("1-Wire Speicher oben", max_age, now)
                .or(ha_fallback),
        }
    }

    fn ha_subscribers(&self) -> usize {
        self.bridge.lock().expect("bridge lock").server.subscriber_count()
    }

    fn push_sensor(&mut self, object_id: &str, value: f64) {
        self.bridge
            .lock()
            .expect("bridge lock")
            .update_sensor_by_object_id(object_id, SensorValue::Number(value));
    }
}

/// Map a DS100 reading to the config field names (Python getattr dispatch).
fn reading_fields(r: &DS100Reading) -> Vec<(&'static str, f64)> {
    vec![
        ("power_combined", r.power_combined),
        ("power_l1", r.power_l1),
        ("power_l2", r.power_l2),
        ("power_l3", r.power_l3),
        ("apparent_combined", r.apparent_combined),
        ("apparent_l1", r.apparent_l1),
        ("apparent_l2", r.apparent_l2),
        ("apparent_l3", r.apparent_l3),
        ("reactive_combined", r.reactive_combined),
        ("reactive_l1", r.reactive_l1),
        ("reactive_l2", r.reactive_l2),
        ("reactive_l3", r.reactive_l3),
        ("voltage_l1", r.voltage_l1),
        ("voltage_l2", r.voltage_l2),
        ("voltage_l3", r.voltage_l3),
        ("voltage_ln_avg", r.voltage_ln_avg),
        ("current_l1", r.current_l1),
        ("current_l2", r.current_l2),
        ("current_l3", r.current_l3),
        ("current_combined", r.current_combined),
        ("frequency", r.frequency),
        ("power_factor", r.power_factor),
        ("energy_total_active", r.energy_total_active),
        ("energy_forward_active", r.energy_forward_active),
        ("energy_reverse_active", r.energy_reverse_active),
        ("energy_total_reactive", r.energy_total_reactive),
        ("energy_forward_reactive", r.energy_forward_reactive),
        ("energy_reverse_reactive", r.energy_reverse_reactive),
        ("demand_forward", r.demand_forward),
        ("demand_total", r.demand_total),
        ("demand_max_forward", r.demand_max_forward),
        ("demand_max_total", r.demand_max_total),
    ]
}

/// DS100 polling thread — mirrors the Python asyncio poller (tiered
/// 100ms/30s/5min) with the Linux serial transport.
fn run_ds100_thread(
    config: Config,
    ds: Arc<Mutex<DsShared>>,
    bridge: Arc<Mutex<Bridge>>,
    stop: Arc<AtomicBool>,
) {
    let transport = LinuxSerialPort::new(&config.ds100.serial_port, config.ds100.timeout);
    let mut poller = DS100Poller::new(
        &config.ds100.serial_port,
        config.ds100.unit_id,
        config.ds100.target_baud,
        config.ds100.timeout,
        config.ds100.poll_interval_fast,
        config.ds100.poll_interval_full,
        config.ds100.poll_interval_demand,
        config.ds100.reconnect_delay,
        Box::new(transport),
    );
    let mut sleep = |secs: f64| std::thread::sleep(Duration::from_secs_f64(secs));
    while !stop.load(Ordering::Relaxed) {
        if !std::path::Path::new(&config.ds100.serial_port).exists() {
            tracing::warn!(
                "DS100 serial port {} not found, waiting...",
                config.ds100.serial_port
            );
            sleep(config.ds100.reconnect_delay);
            continue;
        }
        if !poller.connect_with_baud_detection(&mut sleep) {
            sleep(config.ds100.reconnect_delay);
            continue;
        }
        if let Some(info) = poller.read_device_info() {
            tracing::info!(
                "DS100 device: SN={} SW={} HW={} baud={}",
                info.serial_number,
                info.sw_version_str(),
                info.hw_version_str(),
                info.baud_rate()
            );
        }
        let mut last_full = 0.0;
        let mut last_demand = 0.0;
        while !stop.load(Ordering::Relaxed) && poller.is_open() {
            let now = system_now();
            poller.poll_fast(now, &mut |p| {
                let mut sh = ds.lock().expect("ds lock");
                sh.last = Some(p);
                sh.ts = now;
            });
            {
                // Telemetry for the STATUS line (outside the closure:
                // poll_fast holds the &mut borrow).
                let mut sh = ds.lock().expect("ds lock");
                if let Some(r) = &poller.last_reading {
                    sh.voltage = Some(r.voltage_ln_avg);
                    sh.freq = Some(r.frequency);
                    if r.energy_total_active > 0.0 {
                        sh.energy = Some(r.energy_total_active);
                    }
                }
                sh.msgs = poller.read_count;
                sh.errs = poller.error_count;
            }
            if now - last_full >= config.ds100.poll_interval_full {
                last_full = now;
                if let Some(reading) = poller.poll_full(now) {
                    let bridge = bridge.lock().expect("bridge lock");
                    for (field, value) in reading_fields(&reading) {
                        bridge.update_ds100_field(field, value);
                    }
                }
            }
            if now - last_demand >= config.ds100.poll_interval_demand {
                last_demand = now;
                let _ = poller.poll_demand(now);
            }
            sleep(config.ds100.poll_interval_fast);
        }
        tracing::info!("DS100 reconnecting in {:.0}s...", config.ds100.reconnect_delay);
    }
}

/// 1-Wire polling thread via kernel w1 (sysfs, ds2490/w1_therm drivers).
fn run_onewire_thread(
    config: Config,
    bridge: Arc<Mutex<Bridge>>,
    ow: Arc<Mutex<OwShared>>,
    stop: Arc<AtomicBool>,
) {
    let ow_names: std::collections::HashMap<String, String> = config
        .sensor
        .iter()
        .filter(|sd| sd.platform == "onewire" && !sd.address.is_empty())
        .map(|sd| (sd.address.clone(), sd.name.clone()))
        .collect();
    let mut poller = OneWirePoller::new(
        config.onewire.poll_interval as f64,
        config.onewire.change_threshold,
        config.onewire.reconnect_delay,
        ow_names,
    );
    let sleep = |secs: f64| std::thread::sleep(Duration::from_secs_f64(secs));
    while !stop.load(Ordering::Relaxed) {
        if !poller.connect() {
            sleep(config.onewire.reconnect_delay);
            continue;
        }
        while poller.connected() && !stop.load(Ordering::Relaxed) {
            let now = system_now();
            let bridge = bridge.clone();
            let ow = ow.clone();
            let _ = poller.poll_cycle(
                &mut |addr, _name, temp| {
                    // ESPHome push stays change-filtered (poll_cycle only
                    // calls back on Δ ≥ threshold, Python parity).
                    bridge.lock().expect("bridge lock").on_onewire_update(addr, temp);
                },
                now,
            );
            {
                // Freshness cache + telemetry: snapshot refreshes the
                // timestamp of EVERY sensor each cycle — a stable tank
                // temperature must not age into n/a (04.09.2026 bug).
                let mut sh = ow.lock().expect("ow lock");
                sh.sensors = poller.snapshot();
                sh.total = poller.sensors.len();
                sh.errors = poller.error_count;
                sh.cycle = poller.cycle_count;
                sh.cycle_ms = poller.last_cycle_ms;
            }
            sleep(config.onewire.poll_interval as f64);
        }
        poller.disconnect();
        if !stop.load(Ordering::Relaxed) {
            tracing::info!("1-Wire reconnecting in {:.0}s...", config.onewire.reconnect_delay);
        }
    }
}

/// CAN bus thread: slcan over USBtin → SCBI decode → bridge dispatch.
fn run_canbus_thread(
    config: Config,
    bridge: Arc<Mutex<Bridge>>,
    can: Arc<Mutex<CanShared>>,
    stop: Arc<AtomicBool>,
) {
    let mut decoder = SorelDecoder::new(config.canbus.unknown_buffer_size);
    for (k, v) in &config.canbus.sensor_names {
        decoder.sensor_names.insert(*k as u8, v.clone());
    }
    let mut transport = SlcanTransport::new(&config.canbus.serial_port, config.canbus.tty_baudrate);
    let mut sleep = |secs: f64| std::thread::sleep(Duration::from_secs_f64(secs));
    let mut last_usb_reset = 0.0;
    while !stop.load(Ordering::Relaxed) {
        if !std::path::Path::new(&config.canbus.serial_port).exists() {
            tracing::warn!(
                "CAN USB device {} not found, waiting...",
                config.canbus.serial_port
            );
            sleep(config.canbus.reconnect_delay);
            continue;
        }
        if let Err(e) = transport.open(config.canbus.bitrate) {
            tracing::warn!("CAN open failed: {e} (retrying)");
            sleep(config.canbus.reconnect_delay);
            continue;
        }
        // Wedge recovery: NAK on init acks = wedged USBtin firmware. The
        // de-authorize/re-authorize cycle resets its MCU (rmmod cdc_acm
        // alone does NOT). Rate-limited so a marginal adapter can't cause
        // a reset loop (04.09.2026 incident).
        // DISABLED at the operator's request (04.09.2026) — the automatic
        // reset stays available behind this flag, off until validated.
        const WEDGE_RESET_ENABLED: bool = false;
        if WEDGE_RESET_ENABLED && transport.saw_init_nak() {
            let now = system_now();
            if now - last_usb_reset > 300.0 {
                tracing::warn!("CAN: adapter wedge detected (NAK on init) — USB de-authorize reset");
                if let Err(r) = slcan::usb_deauthorize(&config.canbus.serial_port) {
                    tracing::warn!("CAN: USB reset failed: {r}");
                }
                last_usb_reset = now;
                sleep(1.0);
                continue; // reopen fresh after re-enumeration
            }
        }
        tracing::info!(
            "CAN bus connected on {} ({} bps)",
            config.canbus.serial_port,
            config.canbus.bitrate
        );
        can.lock().expect("can lock").connected = true;
        let mut warn_count: u64 = 0;
        while !stop.load(Ordering::Relaxed) {
            match transport.read_frame() {
                Ok((can_id, data)) => {
                    let msg = decoder.decode(can_id, &data, system_now());
                    if !msg.topic.is_empty()
                        && let Some(value) = msg.value
                    {
                        bridge.lock().expect("bridge lock").on_canbus_update(&msg.topic, value);
                    }
                    if let Some(alert) = decoder.air_detector.check() {
                        tracing::warn!(
                            "AIR IN PIPE detected: Warmwasser dropped {:.0}°C→{:.0}°C while pump PWM={}",
                            alert.from_temp,
                            alert.to_temp,
                            alert.pump_pwm
                        );
                    }
                    let mut sh = can.lock().expect("can lock");
                    sh.packets = decoder.stats.total;
                    sh.errors = decoder.stats.errors;
                }
                Err(e) => {
                    // EOF / io errors mean the device is gone (USB unplug) —
                    // Python treats (OSError, SerialException) as disconnect
                    // and reconnects with a device-existence check. Closing
                    // here breaks out of the read loop; the outer reconnect
                    // loop reopens once the tty reappears (04.09.2026 audit:
                    // without this the loop spins forever on a dead fd).
                    let fatal = e == "slcan eof"
                        || e.starts_with("slcan read: ")
                        || e == "slcan not open";
                    if fatal || !transport.is_open() {
                        tracing::warn!("CAN bus USB disconnect: {e}");
                        transport.close();
                        break;
                    }
                    // Read timeout = normal idle line. Everything else
                    // (parse failures) is worth seeing — rate-limited
                    // against a flood.
                    if e != "slcan read timeout" {
                        warn_count += 1;
                        if warn_count <= 5 || warn_count % 100 == 0 {
                            tracing::warn!("CAN read: {e} (count={warn_count})");
                        }
                    }
                }
            }
        }
        can.lock().expect("can lock").connected = false;
        transport.close();
        if !stop.load(Ordering::Relaxed) {
            tracing::info!("CAN bus reconnecting in {:.0}s...", config.canbus.reconnect_delay);
        }
    }
}

// ---- controller thread ----

struct ControllerParts {
    config: Config,
    dac: Box<dyn Dac>,
    calibration: CalibrationManager,
    bridge: Arc<Mutex<Bridge>>,
    pl: Arc<Mutex<PlShared>>,
    ds: Arc<Mutex<DsShared>>,
    ow: Arc<Mutex<OwShared>>,
    can: Arc<Mutex<CanShared>>,
    command_rx: Arc<Mutex<std::sync::mpsc::Receiver<Command>>>,
    state_file: PathBuf,
    ksk: Arc<Mutex<KaskadeShared>>,
    stop: Arc<AtomicBool>,
}

/// Python `_on_climate_command` + `_on_dac_change` (mock DAC in shadow).
fn handle_climate_command(
    controller: &mut Controller,
    bridge: &Arc<Mutex<Bridge>>,
    key: u32,
    mode: Option<i32>,
    target_temperature: Option<f32>,
    state_file: &PathBuf,
    ksk: &Arc<Mutex<KaskadeShared>>,
) {
    let binding = {
        let bridge = bridge.lock().expect("bridge lock");
        bridge.climate_binding(key).cloned()
    };
    let Some(binding) = binding else {
        tracing::warn!("Climate command for unknown key {key}");
        return;
    };
    let stored = binding.target_temperature;

    let new_ch1: Option<f64> = if mode == Some(0) {
        tracing::info!("Climate OFF: {} -> CH1=0V", binding.object_id);
        Some(0.0)
    } else if let Some(t) = target_temperature {
        let voltage = (t as f64).clamp(0.0, 100.0) / 10.0;
        tracing::info!(
            "Climate HEAT: {} -> {t:.1}°C, CH1={voltage:.2}V",
            binding.object_id
        );
        Some(voltage)
    } else if mode.is_some() {
        let voltage = stored / 10.0;
        tracing::info!(
            "Climate HEAT (mode only): {} -> stored {stored:.1}°C, CH1={voltage:.2}V",
            binding.object_id
        );
        Some(voltage)
    } else {
        None
    };

    if let Some(ch1) = new_ch1 {
        controller.dac.set_voltage_ch1(ch1);
        state::save_ch1_voltage(state_file, ch1);
        // Python `_on_dac_change`: CH1==0 → Brennersperre ON (boiler blocked).
        let ksk_desired = ch1 < 0.05;
        {
            let mut sh = ksk.lock().expect("ksk lock");
            sh.desired = Some(ksk_desired);
            // Bump the change sequence so a connected session sends the
            // command immediately instead of waiting for the 10s tick.
            sh.desired_seq += 1;
        }
        tracing::info!(
            "kaskade: desired={} ({}) (CH1={ch1:.2}V)",
            if ksk_desired { "ON" } else { "OFF" },
            if ksk_desired {
                "Brennersperre aktiv"
            } else {
                "Brennersperre inactive"
            }
        );
        let echo_mode = if ch1 > 0.0 { 3 } else { 0 };
        let echo_action = if ch1 > 0.0 { 4 } else { 0 };
        bridge
            .lock()
            .expect("bridge lock")
            .update_climate_state(key, echo_mode, ch1 * 10.0, None, Some(echo_action));
    }
}

fn run_controller_thread(parts: ControllerParts) {
    let mut controller = Controller::new(
        parts.config.clone(),
        parts.dac,
        parts.calibration,
        system_now(),
    );
    controller.running = true;
    // Interruptible calibration: shutdown aborts the step sleep immediately
    // (Python cancels the async sleep) instead of holding the DAC for up to
    // 30s. The hook drains the climate command queue once per second so
    // scheduler commands apply between steps, not after the whole sweep.
    controller.stop_flag = Some(parts.stop.clone());
    {
        let rx = parts.command_rx.clone();
        let bridge = parts.bridge.clone();
        let state_file = parts.state_file.clone();
        let ksk = parts.ksk.clone();
        controller.calibration_hook = Some(Box::new(move |c: &mut Controller| {
            while let Ok(cmd) = rx.lock().expect("cmd rx").try_recv() {
                if let Command::Climate {
                    key,
                    mode,
                    target_temperature,
                } = cmd
                {
                    handle_climate_command(c, &bridge, key, mode, target_temperature, &state_file, &ksk);
                }
            }
        }));
    }

    let ch1_safe = parts.config.dac.ch1_safe_celsius / 10.0;
    let ch1 = state::load_ch1_voltage(&parts.state_file, ch1_safe);
    controller.dac.set_voltage_ch1(ch1);
    let ksk_desired = ch1 < 0.05;
    {
        let mut sh = parts.ksk.lock().expect("ksk lock");
        sh.desired = Some(ksk_desired);
        sh.desired_seq += 1;
    }
    tracing::info!(
        "kaskade: desired={} ({}) (restored CH1={ch1:.2}V)",
        if ksk_desired { "ON" } else { "OFF" },
        if ksk_desired {
            "Brennersperre aktiv"
        } else {
            "Brennersperre inactive"
        }
    );
    // Python start(): `_on_dac_change(current, ch1)` pushes the restored
    // CH1 state to the climate entities — mirror it so HA sees HEAT/80°C
    // instead of the entity defaults (OFF/60°C) until the next command.
    {
        let echo_mode = if ch1 > 0.0 { 3 } else { 0 };
        let echo_action = if ch1 > 0.0 { 4 } else { 0 };
        let mut bridge = parts.bridge.lock().expect("bridge lock");
        for key in bridge.climate_keys() {
            bridge.update_climate_state(key, echo_mode, ch1 * 10.0, None, Some(echo_action));
        }
    }

    let io = BridgeIo {
        bridge: parts.bridge.clone(),
        config: parts.config.clone(),
        pl: parts.pl.clone(),
        ds: parts.ds.clone(),
        ow: parts.ow.clone(),
    };
    let mut io = io;

    let cfg = parts.config.clone();
    // First watchdog tick after 15s (Python `await asyncio.sleep(15)` before
    // the first check) — avoids startup false alarms while clients connect.
    let mut last_watchdog = system_now() - cfg.control.watchdog_interval + 15.0;
    let mut last_full_push = 0.0;
    let mut last_status = 0.0;
    let mut last_climate_echo: Option<(i32, i32)> = None;

    while !parts.stop.load(Ordering::Relaxed) {
        let now = system_now();

        // Climate commands from HA (server → channel → here)
        while let Ok(cmd) = parts.command_rx.lock().expect("cmd rx").try_recv() {
            if let Command::Climate {
                key,
                mode,
                target_temperature,
            } = cmd
            {
                handle_climate_command(
                    &mut controller,
                    &parts.bridge,
                    key,
                    mode,
                    target_temperature,
                    &parts.state_file,
                    &parts.ksk,
                );
            }
        }

        // DS100 power cadence (~100ms, Python `_on_ds100_power` feed)
        if let Some(power) = parts.ds.lock().expect("ds lock").get(5.0, now) {
            controller.on_ds100_power(&mut io, power);
        }

        // Watchdog (10s)
        if now - last_watchdog >= cfg.control.watchdog_interval {
            last_watchdog = now;

            let max_age = cfg.control.sensor_max_age_seconds;

            // STATUS line (60s) — ALWAYS, regardless of grid freshness, HA
            // subscriber count or calibration state: the controller works on
            // its direct connections and must keep reporting.
            if now - last_status >= STATUS_INTERVAL {
                last_status = now;
                let state_str = if controller.heating_active {
                    "heating"
                } else if controller.internal_cutoff {
                    "cutoff"
                } else if controller.calibrating {
                    "calibrating"
                } else {
                    "idle"
                };
                let fmt = |v: Option<f64>, dec: usize| match v {
                    Some(x) => format!("{x:.dec$}"),
                    None => "n/a".into(),
                };
                let grid = io.role_state(
                    SensorRole::Grid,
                    &cfg.ha_entities.grid_power,
                    max_age,
                );
                let pv = io.ha_numeric(&cfg.ha_entities.pv_dc_power, max_age);
                let rod = io.role_state(
                    SensorRole::RodPower,
                    &cfg.ha_entities.heatingrod_power,
                    max_age,
                );
                let rod_temp = io.role_state(
                    SensorRole::RodTemp,
                    &cfg.ha_entities.heatingrod_temperature,
                    max_age,
                );
                let tank = io.role_state(
                    SensorRole::TankTemp,
                    &cfg.ha_entities.tank_top_temperature,
                    max_age,
                );

                // Direct-connection diagnostics (Python STATUS parity).
                let grid_src = if parts
                    .pl
                    .lock()
                    .expect("pl lock")
                    .get(max_age, io.now())
                    .is_some()
                {
                    "pl"
                } else {
                    "ha"
                };
                let delay = if controller.feedback_delay_count > 0 {
                    format!("{:.0}ms", controller.feedback_delay_ewma * 1000.0)
                } else {
                    "n/a".to_string()
                };
                let hapi = if io.ha_subscribers() > 0 { "ok" } else { "no" };
                let (pl_ok, pl_msgs, pl_intv) = {
                    let sh = parts.pl.lock().expect("pl lock");
                    (
                        if sh.connected { "ok" } else { "no" },
                        sh.updates,
                        sh.last_interval
                            .map(|i| format!("{i:.1}s"))
                            .unwrap_or_else(|| "n/a".to_string()),
                    )
                };
                let (ds_v, ds_hz, ds_msgs, ds_errs) = {
                    let sh = parts.ds.lock().expect("ds lock");
                    (
                        fmt(sh.voltage, 0),
                        fmt(sh.freq, 1),
                        sh.msgs,
                        sh.errs,
                    )
                };
                let (c_ok, c_pkts, c_errs) = {
                    let sh = parts.can.lock().expect("can lock");
                    (
                        if sh.connected { "ok" } else { "no" },
                        sh.packets,
                        sh.errors,
                    )
                };
                let (w_total, w_errs, w_cyc, w_ms) = {
                    let sh = parts.ow.lock().expect("ow lock");
                    (sh.total, sh.errors, sh.cycle, sh.cycle_ms)
                };
                let idac = if controller.dac.is_responsive() {
                    "ok"
                } else {
                    "no"
                };
                let ksk = match parts.ksk.lock().expect("ksk lock").actual {
                    Some(true) => "on",
                    Some(false) => "off",
                    None => "?",
                };
                tracing::info!(
                    "STATUS | {state_str} | grid={}W({grid_src}) pv={}W rod={}W/{}°C tank={}°C dac={:.1}V/{:.0}% delay={delay} | hapi={hapi} pl={pl_ok},msgs={pl_msgs},intv={pl_intv} ds100={ds_v}V,{ds_hz}Hz,mbus={ds_msgs},{ds_errs}e cbus={c_ok},pkts={c_pkts},errs={c_errs} 1wir={w_total}/{w_errs}e,cyc={w_cyc},{w_ms:.0}ms idac={idac} pc1={:.0}°C ksk={ksk}",
                    fmt(grid, 0),
                    fmt(pv, 0),
                    fmt(rod, 0),
                    fmt(rod_temp, 1),
                    fmt(tank, 1),
                    controller.dac.current_voltage(),
                    controller.dac.current_level() * 100.0,
                    controller.dac.current_voltage_ch1() * 10.0
                );
            }

            // Climate echo (Python `_on_dac_change`): action reflects the
            // heating state — HA automations rely on action == "heating".
            // Change-gated to avoid a wire frame every 10s.
            {
                let ch1 = controller.dac.current_voltage_ch1();
                let echo_mode = if ch1 > 0.0 { 3 } else { 0 };
                let echo_action = if controller.heating_active {
                    3
                } else if ch1 > 0.0 {
                    4
                } else {
                    0
                };
                if last_climate_echo != Some((echo_mode, echo_action)) {
                    last_climate_echo = Some((echo_mode, echo_action));
                    let mut bridge = parts.bridge.lock().expect("bridge lock");
                    for key in bridge.climate_keys() {
                        bridge.update_climate_state(
                            key,
                            echo_mode,
                            ch1 * 10.0,
                            None,
                            Some(echo_action),
                        );
                    }
                }
            }

            let pl_fresh = parts
                .pl
                .lock()
                .expect("pl lock")
                .get(max_age, io.now())
                .is_some();
            let ha_fresh = io.ha_numeric(&cfg.ha_entities.grid_power, max_age).is_some();
            // Grid source transition logs (Python parity: "HA relay →
            // Powerlogger direct (recovered)" etc.).
            let grid_src = if pl_fresh {
                "powerlogger direct"
            } else if ha_fresh {
                "HA relay"
            } else {
                "no data"
            };
            if grid_src != controller.grid_source {
                tracing::info!("Grid power: {grid_src} (was {})", controller.grid_source);
                controller.grid_source = grid_src.to_string();
            }
            let grid_val = io.role_state(SensorRole::Grid, &cfg.ha_entities.grid_power, max_age);
            let grid_fresh = grid_val.is_some();
            if !grid_fresh {
                // Diagnostics (also covers the Python-parity silent return):
                // which source is dead? Powerlogger direct vs HA fallback.
                tracing::warn!(
                    "WATCHDOG: grid stale (pl_direct={pl_fresh}, ha_fallback={ha_fresh})"
                );
                if controller.heating_active {
                    controller.emergency_shutdown("Grid power sensor stale");
                }
                continue;
            }
            // Operator decision: unlike Python (whose sensors came THROUGH
            // HA), the Rust controller has direct connections (powerlogger,
            // DS100, 1-Wire) — keep working and logging STATUS without HA.
            // Safety net: stale direct data still triggers the grid check
            // above; the manual switch/tank limits come from HA and are
            // simply absent while HA is gone.
            if io.ha_subscribers() == 0 {
                tracing::warn!("WATCHDOG: no HA subscribers — continuing on direct connections");
            }
            if controller.calibrating {
                continue;
            }

            // Full controller push (30s, Python watchdog `_update_esphome_sensors`)
            if now - last_full_push >= FULL_PUSH_INTERVAL {
                last_full_push = now;
                let grid = io.role_state(
                    SensorRole::Grid,
                    &cfg.ha_entities.grid_power,
                    max_age,
                );
                let pv = io.ha_numeric(&cfg.ha_entities.pv_dc_power, max_age);
                let state_str = if controller.heating_active {
                    "heating"
                } else if controller.internal_cutoff {
                    "cutoff"
                } else if controller.calibrating {
                    "calibrating"
                } else {
                    "idle"
                };
                let push = |field: &str, v: f64| {
                    io.bridge
                        .lock()
                        .expect("bridge lock")
                        .update_controller_field(field, v);
                };
                push("dac_level", controller.dac.current_level() * 100.0);
                push("dac_voltage", controller.dac.current_voltage());
                if let Some(g) = grid {
                    push("grid_power", g);
                }
                if let Some(p) = pv {
                    push("pv_power", p);
                }
                push("overdraw_energy", controller.overdraw_energy_wh);
                push("feedback_delay", controller.feedback_delay_ewma * 1000.0);
                push("uptime", now - controller.start_time);
                // Python `"overdraw"`: grid only while actually heating.
                let rod_now = io.role_state(
                    SensorRole::RodPower,
                    &cfg.ha_entities.heatingrod_power,
                    max_age,
                );
                let overdraw = match (grid, rod_now) {
                    (Some(g), Some(r)) if g > 0.0 && r > 0.0 && controller.heating_active => g,
                    _ => 0.0,
                };
                push("overdraw", overdraw);
                // CO2 saved: each PV-kWh into the rod saves 0.268 kg CO2
                // (oil not burned). Python: r.energy_total_active delta.
                let ds_energy = parts.ds.lock().expect("ds lock").energy;
                if let Some(energy) = ds_energy {
                    if controller.co2_start_energy.is_none() {
                        controller.co2_start_energy = Some(energy);
                    }
                    if let Some(start) = controller.co2_start_energy {
                        push("co2_saved", (energy - start) * 0.268);
                    }
                }
                io.bridge.lock().expect("bridge lock").update_sensor_by_object_id(
                    "ctrl_state",
                    SensorValue::Text(state_str.to_string()),
                );
            }

            controller.control_loop(&mut io, &mut |secs| {
                std::thread::sleep(Duration::from_secs_f64(secs));
            });
        }

        std::thread::sleep(Duration::from_millis(100));
    }

    // Graceful shutdown: CH0 → 0V, CH1 → safe (Python emergency + atexit).
    controller.emergency_shutdown("shutdown");
    let dac = &mut controller.dac;
    dac.shutdown();
    tracing::info!("Controller stopped");
}

// ---- async tasks ----

async fn powerlogger_task(
    config: Config,
    pl: Arc<Mutex<PlShared>>,
    stop: Arc<AtomicBool>,
) {
    let mut client = PowerloggerClient::new(
        &config.powerlogger.host,
        config.powerlogger.port,
        &config.powerlogger.noise_psk,
        &config.powerlogger.entity_object_id,
        config.powerlogger.reconnect_delay,
    );
    client.set_shared(pl);
    let mut tries: u32 = 0;
    while !stop.load(Ordering::Relaxed) {
        client.run_session().await;
        if stop.load(Ordering::Relaxed) {
            break;
        }
        // aioesphomeapi ReconnectLogic: exponential backoff ~1.8^tries,
        // capped at 60s — a dead ESP is not hammered 12×/minute.
        tries += 1;
        let delay = client.reconnect_delay * 1.8f64.powi(tries as i32).min(60.0);
        tokio::time::sleep(Duration::from_secs_f64(delay)).await;
    }
}

async fn kaskade_task(
    config: Config,
    shared: Arc<Mutex<KaskadeShared>>,
    stop: Arc<AtomicBool>,
) {
    let mut client = KaskadeClient::new(
        &config.kaskade.host,
        config.kaskade.port,
        &config.kaskade.noise_psk,
        &config.kaskade.switch_object_id,
        config.kaskade.reconnect_delay,
    );
    client.set_shared(shared);
    // Backoff: an ESP that rejects handshakes (e.g. saturated after many
    // restarts) must not be hammered every 5s — 30s gives it room to recover.
    let mut backoff = 5.0_f64;
    while !stop.load(Ordering::Relaxed) {
        client.run_session().await;
        if stop.load(Ordering::Relaxed) {
            break;
        }
        if client.connected {
            backoff = 5.0;
        }
        tokio::time::sleep(Duration::from_secs_f64(backoff)).await;
        backoff = (backoff * 2.0).min(60.0);
    }
}

async fn ha_state_task(
    bridge: Arc<Mutex<Bridge>>,
    mut rx: mpsc::Receiver<HaStateEvent>,
) {
    while let Some(event) = rx.recv().await {
        tracing::debug!("HA state: {} = {}", event.entity_id, event.state);
        bridge
            .lock()
            .expect("bridge lock")
            .on_ha_state(&event.entity_id, &event.state);
    }
}

async fn forward_commands(
    mut rx: mpsc::Receiver<Command>,
    tx: std::sync::mpsc::Sender<Command>,
) {
    while let Some(cmd) = rx.recv().await {
        let _ = tx.send(cmd);
    }
}

// ---- entry point ----

#[derive(Debug, Default)]
struct Args {
    config: Option<PathBuf>,
    mock_all: bool,
    verbose: u8,
    debug: bool,
    log_modules: Vec<String>,
}

fn parse_args() -> Result<Args, String> {
    let mut args = Args::default();
    let mut it = std::env::args().skip(1);
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--config" | "-c" => {
                args.config = Some(PathBuf::from(
                    it.next().ok_or("--config needs a path")?,
                ));
            }
            "--mock-all" => args.mock_all = true,
            "-v" => args.verbose += 1,
            "--debug" => args.debug = true,
            "--log-module" => {
                args.log_modules
                    .push(it.next().ok_or("--log-module needs a module name")?);
            }
            "-h" | "--help" => {
                println!(
                    "heatingrod v3\nusage: heatingrod --config CONFIG.yaml [--mock-all] [-v] [--debug] [--log-module MOD]"
                );
                std::process::exit(0);
            }
            other => return Err(format!("unknown argument: {other}")),
        }
    }
    Ok(args)
}

fn init_logging(args: &Args, forward: Option<Arc<esphome_api_server::ApiServer>>) {
    use std::io::IsTerminal;
    use tracing_subscriber::filter::Targets;
    use tracing_subscriber::prelude::*;

    // Python FIRST_PARTY_LOGGERS principle: `-v` raises only our own modules
    // to DEBUG; third-party crates stay at INFO (CLAUDE.md: journal flood).
    let mut targets = Targets::new().with_default(tracing::Level::INFO);
    if args.debug || args.verbose > 0 {
        targets = targets
            .with_target("heatingrod", tracing::Level::DEBUG)
            .with_target("esphome_api_server", tracing::Level::DEBUG);
    }
    if args.debug {
        // Python `-vv`: everything.
        targets = targets.with_default(tracing::Level::DEBUG);
    }
    for m in &args.log_modules {
        // Python --log-module names (ds100, controller, …) map into both
        // module trees.
        targets = targets.with_target(format!("heatingrod::{m}"), tracing::Level::DEBUG);
        targets =
            targets.with_target(format!("heatingrod::hardware::{m}"), tracing::Level::DEBUG);
    }

    let force_stderr = std::env::var("HEATINGROD_LOG_STDERR").is_ok();
    let use_stderr = std::io::stderr().is_terminal()
        || force_stderr
        || !std::path::Path::new("/dev/log").exists();

    let fmt_layer = tracing_subscriber::fmt::layer()
        .with_target(false)
        .with_filter(targets.clone());

    if use_stderr {
        match forward {
            Some(server) => {
                tracing_subscriber::registry()
                    .with(fmt_layer)
                    .with(logging::LogForwardLayer::new(server))
                    .init();
            }
            None => {
                tracing_subscriber::registry().with(fmt_layer).init();
            }
        }
    } else if let Ok(journald_layer) = logging::JournaldLayer::new("heatingrod") {
        // Native journald datagrams (no libsystemd link — works in static
        // musl builds too). Priority mapping is internal (DEBUG=7, INFO=6,
        // WARNING=4, ERROR=3 — same as the former tracing-journald path).
        match forward {
            Some(server) => {
                let _ = tracing_subscriber::registry()
                    .with(journald_layer)
                    .with(logging::LogForwardLayer::new(server))
                    .with(targets)
                    .try_init();
            }
            None => {
                let _ = tracing_subscriber::registry()
                    .with(journald_layer)
                    .with(targets)
                    .try_init();
            }
        }
    } else {
        tracing_subscriber::registry().with(fmt_layer).init();
    }
}

#[tokio::main]
async fn main() -> ExitCode {
    let args = match parse_args() {
        Ok(a) => a,
        Err(e) => {
            eprintln!("{e}");
            return ExitCode::from(2);
        }
    };

    let config_path = args
        .config
        .clone()
        .or_else(|| std::env::var("HEATINGROD_CONFIG").ok().map(PathBuf::from))
        .unwrap_or_else(|| PathBuf::from("config.yaml"));
    let config = match Config::load(&config_path) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("{e}");
            return ExitCode::from(1);
        }
    };

    let (command_tx, command_rx) = mpsc::channel(256);
    let (ha_event_tx, ha_event_rx) = mpsc::channel(256);
    let bridge = match Bridge::build(&config, command_tx, ha_event_tx) {
        Ok(b) => b,
        Err(e) => {
            eprintln!("bridge setup failed: {e}");
            return ExitCode::from(1);
        }
    };
    let bridge = Arc::new(Mutex::new(bridge));
    let server = bridge
        .lock()
        .expect("bridge lock")
        .server
        .clone();

    init_logging(&args, Some(server.clone()));
    tracing::info!(
        "heatingrod v3 starting (config {}, mock_all={})",
        config_path.display(),
        args.mock_all
    );

    // Controller DAC: mock (shadow) or real GP8403 via i2cdev (cutover).
    let dac: Box<dyn Dac> = if config.dac.mock || args.mock_all {
        Box::new(MockDac::new(config.dac.max_voltage))
    } else {
        let addr = u8::from_str_radix(config.dac.i2c_address.trim_start_matches("0x"), 16)
            .unwrap_or(0x5f);
        let bus = LinuxI2cBus::new("/dev/i2c-1", addr as u16);
        let mut real = RealDac::new(
            addr,
            config.dac.max_voltage,
            config.dac.ch1_safe_celsius / 10.0,
            Box::new(bus),
        );
        let mut sleep = |secs: f64| std::thread::sleep(Duration::from_secs_f64(secs));
        real.initialize(30, &mut sleep);
        Box::new(real)
    };

    let mut calibration = CalibrationManager::new(
        &config.calibration.file_path,
        config.calibration.max_age_days,
        config.calibration.deviation_threshold,
        config.calibration.ewma_alpha,
        config.dac.max_voltage,
    );
    calibration.load();

    let state_file = state::state_path(&config.calibration.file_path);

    let stop = Arc::new(AtomicBool::new(false));
    let pl = Arc::new(Mutex::new(PlShared::default()));
    let ds = Arc::new(Mutex::new(DsShared::default()));
    let ow = Arc::new(Mutex::new(OwShared::default()));
    let can = Arc::new(Mutex::new(CanShared {
        connected: false,
        packets: 0,
        errors: 0,
    }));
    let ksk = Arc::new(Mutex::new(KaskadeShared::default()));

    let (cmd_tx, cmd_rx) = std::sync::mpsc::channel();

    // Controller thread
    let parts = ControllerParts {
        config: config.clone(),
        dac,
        calibration,
        bridge: bridge.clone(),
        pl: pl.clone(),
        ds: ds.clone(),
        ow: ow.clone(),
        can: can.clone(),
        command_rx: Arc::new(Mutex::new(cmd_rx)),
        state_file: state_file.clone(),
        ksk: ksk.clone(),
        stop: stop.clone(),
    };
    let controller_handle = std::thread::Builder::new()
        .name("heatingrod-controller".into())
        .spawn(move || run_controller_thread(parts))
        .expect("spawn controller thread");

    // Async tasks
    let run_server = server.clone();
    let server_task = tokio::spawn(async move { run_server.run().await });

    let mut tasks: Vec<tokio::task::JoinHandle<()>> = Vec::new();
    tasks.push(tokio::spawn(forward_commands(command_rx, cmd_tx)));
    tasks.push(tokio::spawn(ha_state_task(bridge.clone(), ha_event_rx)));

    if !config.powerlogger.host.is_empty() {
        tracing::info!(
            "powerlogger client: {}:{}",
            config.powerlogger.host,
            config.powerlogger.port
        );
        tasks.push(tokio::spawn(powerlogger_task(
            config.clone(),
            pl.clone(),
            stop.clone(),
        )));
    }
    if config.kaskade.enabled && !config.kaskade.host.is_empty() {
        tracing::info!("kaskade client: {}:{}", config.kaskade.host, config.kaskade.port);
        tasks.push(tokio::spawn(kaskade_task(
            config.clone(),
            ksk.clone(),
            stop.clone(),
        )));
    }
    if config.ds100.enabled && !config.ds100.serial_port.is_empty() {
        tracing::info!("ds100 poller: {}", config.ds100.serial_port);
        std::thread::Builder::new()
            .name("heatingrod-ds100".into())
            .spawn({
                let cfg = config.clone();
                let ds = ds.clone();
                let bridge = bridge.clone();
                let stop = stop.clone();
                move || run_ds100_thread(cfg, ds, bridge, stop)
            })
            .expect("spawn ds100 thread");
    }
    if config.onewire.enabled {
        tracing::info!("1-Wire poller: kernel w1 (sysfs)");
        std::thread::Builder::new()
            .name("heatingrod-onewire".into())
            .spawn({
                let cfg = config.clone();
                let bridge = bridge.clone();
                let ow = ow.clone();
                let stop = stop.clone();
                move || run_onewire_thread(cfg, bridge, ow, stop)
            })
            .expect("spawn onewire thread");
    }
    if config.canbus.enabled && !config.canbus.serial_port.is_empty() {
        tracing::info!("CAN bus: {}", config.canbus.serial_port);
        std::thread::Builder::new()
            .name("heatingrod-canbus".into())
            .spawn({
                let cfg = config.clone();
                let bridge = bridge.clone();
                let can = can.clone();
                let stop = stop.clone();
                move || run_canbus_thread(cfg, bridge, can, stop)
            })
            .expect("spawn canbus thread");
    }

    // Signals: SIGTERM/SIGINT/SIGHUP → graceful shutdown (Python parity).
    let mut term = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        .expect("install SIGTERM handler");
    let mut int = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::interrupt())
        .expect("install SIGINT handler");
    let mut hup = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::hangup())
        .expect("install SIGHUP handler");

    tokio::select! {
        _ = term.recv() => tracing::warn!("SIGTERM received, shutting down"),
        _ = int.recv() => tracing::warn!("SIGINT received, shutting down"),
        _ = hup.recv() => tracing::warn!("SIGHUP received, shutting down"),
        res = server_task => {
            match res {
                Ok(Err(e)) => tracing::error!("server task failed: {e}"),
                Ok(Ok(())) => tracing::info!("server task ended"),
                Err(e) => tracing::error!("server task panicked: {e}"),
            }
        }
    }

    stop.store(true, Ordering::Relaxed);
    for t in tasks.drain(..) {
        t.abort();
    }
    // The controller thread may only end after a stop signal. Anything else
    // (panic, .expect() poison) leaves the DAC unmanaged — exit so systemd
    // restarts us and ExecStopPost dac-safe forces CH0=0V/CH1=6V. This
    // covers the dev profile too, where panic=abort does not exist.
    if controller_handle.is_finished() && !stop.load(Ordering::Relaxed) {
        tracing::error!("controller thread ended unexpectedly — exiting for systemd restart (dac-safe)");
        std::process::exit(1);
    }
    // Server-task failure path (no signal): stop the controller before
    // joining, otherwise join would block forever.
    stop.store(true, Ordering::Relaxed);
    let _ = controller_handle.join();
    ExitCode::SUCCESS
}

#[cfg(test)]
mod tests {
    use super::watchdog_ha_policy;

    /// Documents the deliberate deviation from Python (04.09.2026): with
    /// direct connections, losing HA subscribers must NOT stop heating —
    /// the controller keeps working and only logs a warning. Python's
    /// `emergency_shutdown("HA not connected via ESPHome API")` is the
    /// reference behavior this test pins against.
    #[test]
    fn ha_subscriber_loss_does_not_emergency() {
        assert_eq!(watchdog_ha_policy(true, 0), None);
        assert_eq!(watchdog_ha_policy(false, 0), None);
    }
}
