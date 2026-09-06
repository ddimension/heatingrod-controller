//! 1-Wire temperature poller via kernel w1 (sysfs) — `ds2490`/`w1_therm`
//! kernel drivers, reads `/sys/bus/w1/devices/28-xxxxxxxxxxxx/w1_slave`.
//! The libowcapi-FFI transport was removed after the kernel-w1 test run
//! (2026-09-04, operator decision).
//!
//! Every `open()` triggers a fresh conversion (~750ms) — blocking reads on
//! the dedicated 1-Wire thread, same architecture as the former libowcapi
//! path (which blocked in C on this thread; Python used asyncio.to_thread
//! for the same reason). Poller semantics unchanged from Python
//! `onewire.py`: backoff after 5 consecutive errors, 85°C power-on filter,
//! range check −55…125°C, push-on-change, per-cycle freshness for the
//! controller snapshot. New vs. Python: up to 3 immediate re-reads on CRC
//! NO — the bus lines are long, expect CRC retries.
//!
//! Kernel dir names (`28-0316720f11ff` — lowercase dash, serial LSB-first)
//! are canonicalized to the config format (`28.FF110F721603` — uppercase
//! dot, MSB order) so HA entity identity (object_id, MD5 key) is unchanged
//! from the libowcapi era.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

const DEFAULT_W1_ROOT: &str = "/sys/bus/w1/devices";
const BACKOFF_THRESHOLD: u32 = 5; // consecutive errors before backoff
const BACKOFF_CYCLES: u64 = 30; // retry only every N cycles when backed off
/// Immediate re-reads on CRC NO / read failure within one poll (long bus
/// lines → CRC retries are expected, not exceptional).
const CRC_RETRIES: u32 = 3;

/// Injectable `w1_slave` file read (tests); None = real fs read.
pub type FileRead = Box<dyn FnMut(&Path) -> Result<String, String> + Send>;

/// Reverse the 6 address bytes: DS18B20 ROMs are transmitted LSB-first,
/// so the kernel presents the little-endian form (`28-0316720f11ff`)
/// while libowcapi/config use MSB order (`28.FF110F721603`).
fn reverse_hex_bytes(hex: &str) -> String {
    hex.as_bytes()
        .chunks(2)
        .map(|b| std::str::from_utf8(b).expect("ascii hex"))
        .rev()
        .collect()
}

/// Kernel dir name → config/HA address format (MSB order).
/// `28-0316720f11ff` → `28.FF110F721603`; anything else → None.
fn canonical_addr(dir_name: &str) -> Option<String> {
    let hex = dir_name.strip_prefix("28-")?;
    (hex.len() == 12 && hex.chars().all(|c| c.is_ascii_hexdigit()))
        .then(|| format!("28.{}", reverse_hex_bytes(hex).to_ascii_uppercase()))
}

/// Config/HA address → kernel dir name (LSB order).
/// `28.FF110F721603` → `28-0316720f11ff`.
fn sysfs_dir(addr: &str) -> String {
    match addr.strip_prefix("28.") {
        Some(hex) => format!("28-{}", reverse_hex_bytes(hex).to_ascii_lowercase()),
        None => addr.to_ascii_lowercase(),
    }
}

/// Parse one `w1_slave` read: `crc=xx YES` line + `t=23625` millidegrees.
/// A `NO` CRC verdict is an error → caller re-reads.
fn parse_w1_slave(content: &str) -> Result<f64, String> {
    // w1_slave layout: `<9 scratchpad bytes> : crc=xx YES|NO` on line 1,
    // the same scratchpad plus ` t=23625` (millidegrees) on line 2.
    let mut t_line: Option<&str> = None;
    let mut crc_ok = false;
    for line in content.lines() {
        if let Some((_, rest)) = line.split_once("t=") {
            t_line = Some(rest.trim());
        } else if line.trim_end().ends_with("YES") {
            crc_ok = true;
        }
    }
    let t = t_line.ok_or_else(|| "no t= value".to_string())?;
    let millis: i64 = t
        .parse()
        .map_err(|e| format!("bad t= value {t:?}: {e}"))?;
    if !crc_ok {
        return Err("CRC NO".to_string());
    }
    Ok(millis as f64 / 1000.0)
}

pub struct OneWireSensor {
    pub address: String,
    pub name: String,
    pub temperature: f64,
    pub last_updated: f64,
    pub read_errors: u32,
    pub consecutive_errors: u32,
    pub missing: bool,
}

impl OneWireSensor {
    fn new(address: String, name: String) -> Self {
        Self {
            address,
            name,
            temperature: 0.0,
            last_updated: 0.0,
            read_errors: 0,
            consecutive_errors: 0,
            missing: true,
        }
    }
}

pub struct OneWirePoller {
    pub poll_interval: f64,
    pub change_threshold: f64,
    pub reconnect_delay: f64,
    pub sensor_names: HashMap<String, String>,
    pub sensors: HashMap<String, OneWireSensor>,
    /// Sysfs device root; tests point this at a fake tree.
    pub w1_root: PathBuf,
    initialized: bool,
    pub read_count: u64,
    pub error_count: u64,
    pub cycle_count: u64,
    pub last_cycle_ms: f64,
    /// Injectable raw read (tests); None = real fs read.
    file_read: Option<FileRead>,
}

impl Default for OneWirePoller {
    fn default() -> Self {
        Self::new(10.0, 0.1, 5.0, HashMap::new())
    }
}

impl OneWirePoller {
    pub fn new(
        poll_interval: f64,
        change_threshold: f64,
        reconnect_delay: f64,
        sensor_names: HashMap<String, String>,
    ) -> Self {
        Self {
            poll_interval,
            change_threshold,
            reconnect_delay,
            sensor_names,
            sensors: HashMap::new(),
            w1_root: PathBuf::from(DEFAULT_W1_ROOT),
            initialized: false,
            read_count: 0,
            error_count: 0,
            cycle_count: 0,
            last_cycle_ms: 0.0,
            file_read: None,
        }
    }

    pub fn connected(&self) -> bool {
        self.initialized
    }

    /// Directory scan: register all `28-*` dirs (canonicalized). Returns
    /// the canonical addresses present on the bus.
    fn scan_sensors(&mut self) -> Vec<String> {
        let mut found = Vec::new();
        let Ok(entries) = std::fs::read_dir(&self.w1_root) else {
            tracing::error!("1-Wire: cannot read {}", self.w1_root.display());
            return found;
        };
        for entry in entries.flatten() {
            let dir = entry.file_name().to_string_lossy().into_owned();
            if let Some(addr) = canonical_addr(&dir) {
                found.push(addr.clone());
                if !self.sensors.contains_key(&addr) {
                    let name = self
                        .sensor_names
                        .get(&addr)
                        .cloned()
                        .unwrap_or_else(|| addr.clone());
                    tracing::info!("1-Wire: registered sensor {addr} ({name})");
                    self.sensors
                        .insert(addr.clone(), OneWireSensor::new(addr, name));
                }
            }
        }
        found
    }

    /// Python `_connect` equivalent: no bus-scan sleep — the kernel has
    /// already enumerated the bus into sysfs. An empty scan is still a
    /// FAILURE (Python parity: sensors may appear later; the reconnect
    /// loop retries).
    pub fn connect(&mut self) -> bool {
        let found = self.scan_sensors();
        if found.is_empty() {
            tracing::warn!(
                "1-Wire: empty bus scan (no sensors under {}) — reconnecting",
                self.w1_root.display()
            );
            return false;
        }
        let named = found
            .iter()
            .filter(|a| self.sensor_names.contains_key(*a))
            .count();
        tracing::info!(
            "1-Wire kernel w1: {} DS18B20 sensors ({named} named, {} unknown)",
            found.len(),
            found.len() - named
        );
        self.initialized = true;
        true
    }

    /// Python `_disconnect` — nothing to release, the kernel owns the bus.
    pub fn disconnect(&mut self) {
        self.initialized = false;
    }

    /// One `w1_slave` read with CRC retries. Each `open()` triggers a fresh
    /// conversion (~750ms), so a CRC NO re-read is a full retry. Long bus
    /// lines make CRC errors likely → up to [`CRC_RETRIES`] attempts.
    fn read_slave(&mut self, addr: &str) -> Result<f64, String> {
        let path = self.w1_root.join(sysfs_dir(addr)).join("w1_slave");
        let mut last_err = String::new();
        for attempt in 0..CRC_RETRIES {
            let content = match &mut self.file_read {
                Some(read) => read(&path),
                None => std::fs::read_to_string(&path).map_err(|e| e.to_string()),
            };
            match content.and_then(|c| parse_w1_slave(&c)) {
                Ok(temp) => {
                    if attempt > 0 {
                        tracing::debug!("1-Wire {addr}: CRC retry #{attempt} ok");
                    }
                    return Ok(temp);
                }
                Err(e) => last_err = e,
            }
        }
        Err(last_err)
    }

    /// Python `_read_all`: sequential reads with backoff, 85°C filter,
    /// range check.
    pub fn read_all(&mut self) -> HashMap<String, f64> {
        if !self.initialized {
            return HashMap::new();
        }
        let mut results = HashMap::new();
        // Collect addresses first (borrow checker: mutate sensors per addr).
        let addrs: Vec<String> = self.sensors.keys().cloned().collect();
        for addr in addrs {
            let backoff = {
                let s = self.sensors.get(&addr).unwrap();
                s.consecutive_errors >= BACKOFF_THRESHOLD
                    && self.cycle_count % BACKOFF_CYCLES != 0
            };
            if backoff {
                continue;
            }

            match self.read_slave(&addr) {
                Ok(temp) => {
                    if temp == 85.0 {
                        tracing::debug!("1-Wire {addr}: power-on default 85°C, skipping");
                    } else if (-55.0..=125.0).contains(&temp) {
                        results.insert(addr.clone(), temp);
                        {
                            let s = self.sensors.get_mut(&addr).unwrap();
                            if s.consecutive_errors >= BACKOFF_THRESHOLD {
                                tracing::info!(
                                    "1-Wire {addr} ({}): recovered after {} errors",
                                    s.name,
                                    s.consecutive_errors
                                );
                            }
                            s.consecutive_errors = 0;
                        }
                    } else {
                        tracing::warn!("1-Wire {addr}: out of range {temp:.2}°C");
                        // Python: out-of-range bumps consecutive_errors +
                        // error_count but NOT read_errors (read_errors gates
                        // the "read error #N" logs only).
                        self.register_soft_error(&addr);
                    }
                }
                Err(e) => {
                    self.register_error(&addr, 1);
                    let s = self.sensors.get(&addr).unwrap();
                    if s.consecutive_errors == BACKOFF_THRESHOLD {
                        tracing::warn!(
                            "1-Wire {addr} ({}): {} consecutive errors, backing off to every {BACKOFF_CYCLES} cycles",
                            s.name, s.consecutive_errors
                        );
                    } else if s.read_errors <= 3 {
                        tracing::warn!(
                            "1-Wire {addr} ({}): read error #{} ({e})",
                            s.name,
                            s.read_errors
                        );
                    }
                }
            }
        }
        results
    }

    fn register_error(&mut self, addr: &str, _kind: u8) {
        if let Some(s) = self.sensors.get_mut(addr) {
            s.read_errors += 1;
            s.consecutive_errors += 1;
        }
        self.error_count += 1;
    }

    /// Out-of-range value: counts toward backoff and total errors, but not
    /// toward the read-error log gate (Python onewire.py semantics).
    fn register_soft_error(&mut self, addr: &str) {
        if let Some(s) = self.sensors.get_mut(addr) {
            s.consecutive_errors += 1;
        }
        self.error_count += 1;
    }

    /// Python `_poll_loop` body for one cycle: rescan (kernel registers new
    /// devices dynamically — auto-discovery without reconnect), read,
    /// update, callbacks. Returns the cycle duration in ms.
    pub fn poll_cycle(
        &mut self,
        on_update: &mut dyn FnMut(&str, &str, f64),
        now: f64,
    ) -> Result<f64, String> {
        let t0 = std::time::Instant::now();
        self.scan_sensors();
        let temps = self.read_all();
        self.last_cycle_ms = t0.elapsed().as_secs_f64() * 1000.0;
        self.cycle_count += 1;

        for (addr, temp) in temps {
            let Some(sensor) = self.sensors.get_mut(&addr) else {
                continue;
            };
            let old_missing = sensor.missing;
            sensor.missing = false;
            sensor.last_updated = now;
            if old_missing || (temp - sensor.temperature).abs() >= self.change_threshold {
                sensor.temperature = temp;
                self.read_count += 1;
                let name = sensor.name.clone();
                on_update(&addr, &name, temp);
                tracing::debug!("1-Wire {addr} ({name}): {temp:.2}°C");
            }
        }
        Ok(self.last_cycle_ms)
    }

    /// name → (temperature, last_updated) for every non-missing sensor.
    ///
    /// Freshness cache for the controller: `last_updated` is refreshed on
    /// EVERY poll cycle, unlike the change-filtered ESPHome push. Stable
    /// temperatures (tank, thermal mass) must stay fresh even when they
    /// change by less than the push threshold.
    pub fn snapshot(&self) -> HashMap<String, (f64, f64)> {
        self.sensors
            .values()
            .filter(|s| !s.missing)
            .map(|s| (s.name.clone(), (s.temperature, s.last_updated)))
            .collect()
    }

    pub fn get_temperature(&self, address: &str, max_age: f64, now: f64) -> Option<f64> {
        let sensor = self.sensors.get(address)?;
        if sensor.missing {
            return None;
        }
        if now - sensor.last_updated > max_age {
            return None;
        }
        Some(sensor.temperature)
    }

    pub fn get_temperature_by_name(&self, name: &str, max_age: f64, now: f64) -> Option<f64> {
        self.sensors.values().find_map(|s| {
            if s.name == name {
                self.get_temperature(&s.address, max_age, now)
            } else {
                None
            }
        })
    }

    pub fn get_stats(&self) -> (usize, u64, u64, u64, f64) {
        (
            self.sensors.len(),
            self.read_count,
            self.error_count,
            self.cycle_count,
            self.last_cycle_ms,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicI32, Ordering};
    use std::sync::Arc;

    /// Fake sysfs tree in /tmp, auto-removed on drop. Unique per test —
    /// cargo runs tests in parallel threads within one process.
    struct TestDir(PathBuf);

    impl TestDir {
        fn new(tag: &str) -> Self {
            let root = std::env::temp_dir().join(format!(
                "heatingrod-w1-{}-{tag}",
                std::process::id()
            ));
            let _ = std::fs::remove_dir_all(&root);
            Self(root)
        }

        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TestDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn write_sensor(root: &Path, dir: &str, content: &str) {
        std::fs::create_dir_all(root.join(dir)).unwrap();
        std::fs::write(root.join(dir).join("w1_slave"), content).unwrap();
    }

    fn poller_at(root: &Path) -> OneWirePoller {
        let mut p = OneWirePoller::new(10.0, 0.1, 5.0, HashMap::new());
        p.w1_root = root.to_path_buf();
        p
    }

    /// Valid w1_slave content: 23.437°C, CRC OK.
    const GOOD: &str = "4f 01 4b 46 7f ff 0c 10 45 : crc=45 YES\n4f 01 4b 46 7f ff 0c 10 45 t=23437\n";
    /// CRC verdict NO → must be retried / counted as error.
    const CRC_NO: &str = "4f 01 4b 46 7f ff 0c 10 45 : crc=45 NO\n4f 01 4b 46 7f ff 0c 10 45 t=23437\n";

    #[test]
    fn canonical_addr_mapping() {
        // Real pair from the production bus (verified 2026-09-04).
        assert_eq!(
            canonical_addr("28-0316720f11ff"),
            Some("28.FF110F721603".to_string())
        );
        // Bus masters, non-28 devices, non-hex/short names are ignored.
        assert_eq!(canonical_addr("w1_bus_master1"), None);
        assert_eq!(canonical_addr("28-xyz"), None);
        assert_eq!(canonical_addr("28-ff110f"), None);
        assert_eq!(canonical_addr("28-0316720f11ff!"), None);
        // Non-28 families (e.g. the 81- ID chip on the bus) are ignored.
        assert_eq!(canonical_addr("81-000000325b45"), None);
    }

    #[test]
    fn sysfs_dir_mapping() {
        assert_eq!(sysfs_dir("28.FF110F721603"), "28-0316720f11ff");
        assert_eq!(sysfs_dir("28.FFE0DB701603"), "28-031670dbe0ff");
    }

    #[test]
    fn parse_w1_slave_ok() {
        assert_eq!(parse_w1_slave(GOOD), Ok(23.437));
        // Negative temps: t=-375 → -0.375°C.
        let neg = "00 00 00 00 00 00 00 00 00 : crc=00 YES\n00 00 00 00 00 00 00 00 00 t=-375\n";
        assert_eq!(parse_w1_slave(neg), Ok(-0.375));
    }

    #[test]
    fn parse_w1_slave_errors() {
        assert!(parse_w1_slave(CRC_NO).is_err());
        assert!(parse_w1_slave("garbage").is_err());
        // t= present but no YES verdict → malformed.
        assert!(parse_w1_slave("xx t=23437\n").is_err());
    }

    #[test]
    fn connect_scans_and_names_sensors() {
        let dir = TestDir::new("scan");
        write_sensor(dir.path(), "28-0316720f11ff", GOOD);
        write_sensor(dir.path(), "28-abcdabcdabcd", GOOD);
        let mut names = HashMap::new();
        names.insert("28.FF110F721603".into(), "Speicher oben".into());
        let mut p = OneWirePoller::new(10.0, 0.1, 5.0, names);
        p.w1_root = dir.path().to_path_buf();

        assert!(p.connect());
        assert!(p.connected());
        assert_eq!(p.sensors.len(), 2);
        assert_eq!(p.sensors["28.FF110F721603"].name, "Speicher oben");
        // Unknown sensor falls back to its canonical address as name
        // (ab cd ab cd ab cd LSB-first → cd ab cd ab cd ab MSB-first).
        assert_eq!(p.sensors["28.CDABCDABCDAB"].name, "28.CDABCDABCDAB");
    }

    #[test]
    fn connect_empty_bus_is_failure() {
        let dir = TestDir::new("empty");
        // Only a bus master, no sensors — Python parity: retry via reconnect.
        std::fs::create_dir_all(dir.path().join("w1_bus_master1")).unwrap();
        let mut p = poller_at(dir.path());
        assert!(!p.connect());
        assert!(!p.connected());
    }

    #[test]
    fn connect_missing_root_is_failure() {
        let mut p = poller_at(&std::env::temp_dir().join("heatingrod-w1-nonexistent"));
        assert!(!p.connect());
    }

    #[test]
    fn power_on_85_is_skipped() {
        // 85°C = DS18B20 power-on default, filtered out — no push, no error.
        let dir = TestDir::new("85c");
        write_sensor(dir.path(), "28-0316720f11ff", "crc=00 YES\nt=85000\n");
        let mut p = poller_at(dir.path());
        assert!(p.connect());
        let mut calls = 0;
        p.poll_cycle(&mut |_, _, _| calls += 1, 1000.0).unwrap();
        assert_eq!(calls, 0);
        assert_eq!(p.error_count, 0);
    }

    #[test]
    fn out_of_range_counts_error() {
        let dir = TestDir::new("oor");
        write_sensor(dir.path(), "28-0316720f11ff", "crc=00 YES\nt=200000\n");
        let mut p = poller_at(dir.path());
        assert!(p.connect());
        p.poll_cycle(&mut |_, _, _| {}, 1000.0).unwrap();
        assert_eq!(p.error_count, 1);
        assert_eq!(p.sensors["28.FF110F721603"].consecutive_errors, 1);
        // read_errors untouched (soft error — Python semantics).
        assert_eq!(p.sensors["28.FF110F721603"].read_errors, 0);
    }

    #[test]
    fn backoff_after_five_errors() {
        // Unparseable content → every read fails.
        let dir = TestDir::new("backoff");
        write_sensor(dir.path(), "28-0316720f11ff", "garbage\n");
        let mut p = poller_at(dir.path());
        assert!(p.connect());
        // poll_cycle increments cycle_count (read_all alone does not — Python
        // uses the cycle counter from the poll loop).
        for i in 0..6 {
            p.poll_cycle(&mut |_, _, _| {}, 1000.0 + i as f64).unwrap();
        }
        assert_eq!(p.sensors["28.FF110F721603"].consecutive_errors, 5);
        // Backoff active: cycle 6 % 30 != 0 → skipped (no additional errors)
        let errs_before = p.error_count;
        p.poll_cycle(&mut |_, _, _| {}, 1010.0).unwrap();
        assert_eq!(p.error_count, errs_before);
    }

    #[test]
    fn push_on_change_semantics() {
        let dir = TestDir::new("push");
        write_sensor(dir.path(), "28-0316720f11ff", "crc=00 YES\nt=20000\n");
        let mut p = poller_at(dir.path());
        assert!(p.connect());

        let mut calls: Vec<f64> = Vec::new();
        let mut now = 1000.0;
        p.poll_cycle(&mut |_a, _n, t| calls.push(t), now).unwrap();
        now += 10.0;
        p.poll_cycle(&mut |_a, _n, t| calls.push(t), now).unwrap(); // unchanged
        // Rewrite the file: +0.2°C → next cycle pushes.
        write_sensor(dir.path(), "28-0316720f11ff", "crc=00 YES\nt=20200\n");
        now += 10.0;
        p.poll_cycle(&mut |_a, _n, t| calls.push(t), now).unwrap();

        // first cycle: old_missing → push; second: unchanged → no push;
        // third: Δ0.2 ≥ threshold → push
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[0], 20.0);
        assert_eq!(calls[1], 20.2);
        assert_eq!(p.get_temperature("28.FF110F721603", 30.0, now), Some(20.2));
    }

    #[test]
    fn crc_no_retries_then_succeeds() {
        // Long bus lines → CRC NO is expected. Two bad reads, then a good
        // one: the poller must retry within the cycle and succeed.
        let dir = TestDir::new("crc-retry");
        write_sensor(dir.path(), "28-0316720f11ff", GOOD);
        let calls = Arc::new(AtomicI32::new(0));
        let inner = calls.clone();
        let read = Box::new(move |_p: &Path| {
            let n = inner.fetch_add(1, Ordering::Relaxed) + 1;
            if n <= 2 {
                Ok(CRC_NO.to_string())
            } else {
                Ok(GOOD.to_string())
            }
        });
        let mut p = OneWirePoller::new(10.0, 0.1, 5.0, HashMap::new());
        p.w1_root = dir.path().to_path_buf();
        p.file_read = Some(read);
        assert!(p.connect());

        let mut pushes = 0;
        p.poll_cycle(&mut |_, _, _| pushes += 1, 1000.0).unwrap();
        assert_eq!(calls.load(Ordering::Relaxed), 3); // 2 CRC NO + 1 success
        assert_eq!(pushes, 1);
        assert_eq!(p.error_count, 0);
        assert_eq!(p.sensors["28.FF110F721603"].consecutive_errors, 0);
    }

    #[test]
    fn crc_no_exhausts_retries_counts_error() {
        let dir = TestDir::new("crc-fail");
        write_sensor(dir.path(), "28-0316720f11ff", CRC_NO);
        let calls = Arc::new(AtomicI32::new(0));
        let inner = calls.clone();
        let read = Box::new(move |_p: &Path| {
            inner.fetch_add(1, Ordering::Relaxed);
            Ok(CRC_NO.to_string())
        });
        let mut p = OneWirePoller::new(10.0, 0.1, 5.0, HashMap::new());
        p.w1_root = dir.path().to_path_buf();
        p.file_read = Some(read);
        assert!(p.connect());

        p.poll_cycle(&mut |_, _, _| {}, 1000.0).unwrap();
        assert_eq!(calls.load(Ordering::Relaxed), CRC_RETRIES as i32);
        // One error per cycle, not per attempt.
        assert_eq!(p.error_count, 1);
        assert_eq!(p.sensors["28.FF110F721603"].consecutive_errors, 1);
        assert_eq!(p.sensors["28.FF110F721603"].read_errors, 1);
    }

    #[test]
    fn missing_file_counts_as_read_error() {
        let dir = TestDir::new("missing");
        write_sensor(dir.path(), "28-0316720f11ff", GOOD);
        let mut p = poller_at(dir.path());
        assert!(p.connect());
        // Sensor dir disappears (adapter unplugged / device removed).
        std::fs::remove_dir_all(dir.path().join("28-0316720f11ff")).unwrap();
        p.poll_cycle(&mut |_, _, _| {}, 1000.0).unwrap();
        assert_eq!(p.error_count, 1);
        assert_eq!(p.sensors["28.FF110F721603"].read_errors, 1);
    }

    #[test]
    fn new_sensor_autodiscovers_without_reconnect() {
        // Kernel registers new devices dynamically — the per-cycle rescan
        // must pick them up (Python needed a reconnect for this).
        let dir = TestDir::new("autodisc");
        write_sensor(dir.path(), "28-0316720f11ff", GOOD);
        let mut p = poller_at(dir.path());
        assert!(p.connect());
        assert_eq!(p.sensors.len(), 1);
        write_sensor(dir.path(), "28-031670dbe0ff", GOOD);
        p.poll_cycle(&mut |_, _, _| {}, 1000.0).unwrap();
        assert_eq!(p.sensors.len(), 2);
        assert!(p.sensors.contains_key("28.FFE0DB701603"));
    }

    #[test]
    fn stale_temperature_returns_none() {
        let dir = TestDir::new("stale");
        write_sensor(dir.path(), "28-0316720f11ff", GOOD);
        let mut p = poller_at(dir.path());
        assert!(p.connect());
        p.poll_cycle(&mut |_, _, _| {}, 1000.0).unwrap();
        assert_eq!(p.get_temperature("28.FF110F721603", 30.0, 1000.0), Some(23.437));
        assert_eq!(p.get_temperature("28.FF110F721603", 30.0, 1100.0), None);
    }

    #[test]
    fn missing_sensor_returns_none() {
        let p = poller_at(&PathBuf::from(DEFAULT_W1_ROOT));
        assert_eq!(p.get_temperature("28.BB", 30.0, 0.0), None);
    }
}
