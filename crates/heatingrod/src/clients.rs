//! ESPHome Native API CLIENTS — ports of `kaskade.py` + `powerlogger.py`.
//!
//! Uses the vendored crate in INITIATOR mode (the heatingrod patch; upstream
//! `start()` is responder-only and deadlocks as a client). Client semantics
//! mirror aioesphomeapi: connect → send HelloRequest → list entities → resolve
//! the key by object_id (never hardcoded) → subscribe → track state pushes.

use esphome_api_server::proto_types::{HelloRequest, SwitchCommandRequest};
use esphome_native_api::esphomeapi::EspHomeApi;
use esphome_native_api::parser::ProtoMessage;
use esphome_native_api::{Error, HandshakeError};
use std::sync::{Arc, Mutex};
use std::os::fd::AsRawFd;
use tokio::net::TcpStream;

/// TCP keepalive (30s idle, 10s interval, 3 probes): a silently dead ESPHome
/// connection must not hold the role source hostage — without this, a dead
/// peer is only noticed when the session watchdog fires (and the controller
/// then treats grid as stale and stops logging STATUS, Python parity).
fn set_tcp_keepalive(stream: &TcpStream) {
    let fd = stream.as_raw_fd();
    unsafe {
        let one: libc::c_int = 1;
        let _ = libc::setsockopt(
            fd,
            libc::SOL_SOCKET,
            libc::SO_KEEPALIVE,
            &one as *const _ as *const libc::c_void,
            std::mem::size_of::<libc::c_int>() as libc::socklen_t,
        );
        let idle: libc::c_int = 30;
        let _ = libc::setsockopt(
            fd,
            libc::IPPROTO_TCP,
            libc::TCP_KEEPIDLE,
            &idle as *const _ as *const libc::c_void,
            std::mem::size_of::<libc::c_int>() as libc::socklen_t,
        );
        let intvl: libc::c_int = 10;
        let _ = libc::setsockopt(
            fd,
            libc::IPPROTO_TCP,
            libc::TCP_KEEPINTVL,
            &intvl as *const _ as *const libc::c_void,
            std::mem::size_of::<libc::c_int>() as libc::socklen_t,
        );
        let cnt: libc::c_int = 3;
        let _ = libc::setsockopt(
            fd,
            libc::IPPROTO_TCP,
            libc::TCP_KEEPCNT,
            &cnt as *const _ as *const libc::c_void,
            std::mem::size_of::<libc::c_int>() as libc::socklen_t,
        );
    }
}

/// Kaskade switch state label (Python parity: `switch command sent: OFF`).
fn switch_label(state: bool) -> &'static str {
    if state { "ON" } else { "OFF" }
}

/// What the switch state MEANS for the boiler (CLAUDE.md: ON = relay pulls,
/// KN/KL dead → boiler locked out; OFF = KN/KL live → boiler free).
fn switch_meaning(state: bool) -> &'static str {
    if state {
        "Brennersperre aktiv"
    } else {
        "Brennersperre inactive"
    }
}

/// ESPHome's name→object_id derivation (aioesphomeapi `object_id.py`):
/// sanitize(snake_case(name)). Required for API >= 1.14, where servers may
/// omit object_id on the wire and clients reconstruct it.
pub fn compute_object_id(name: &str) -> String {
    name.chars()
        .map(|c| {
            if c == ' ' {
                '_'
            } else if c.is_ascii_uppercase() {
                c.to_ascii_lowercase()
            } else {
                c
            }
        })
        .map(|c| {
            if c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

/// Active liveness probe: PingRequest (7) → PingResponse (8).
///
/// Complements the passive idle watchdog, which resets on ANY incoming
/// frame — a peer that keeps pushing stale data but has stopped reading
/// our frames would go unnoticed. The ping round-trip proves the peer
/// still processes what we send, and measures the session RTT.
const PING_INTERVAL: std::time::Duration = std::time::Duration::from_secs(15);
const PONG_DEADLINE: std::time::Duration = std::time::Duration::from_secs(10);

struct PingMonitor {
    interval: tokio::time::Interval,
    /// Armed for PONG_DEADLINE after each ping; disarmed (far future)
    /// between pings.
    deadline: std::pin::Pin<Box<tokio::time::Sleep>>,
    sent_at: Option<tokio::time::Instant>,
}

impl PingMonitor {
    fn new() -> Self {
        let mut interval = tokio::time::interval(PING_INTERVAL);
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        let deadline = Box::pin(tokio::time::sleep(std::time::Duration::from_secs(3600)));
        Self {
            interval,
            deadline,
            sent_at: None,
        }
    }

    /// Arm the pong deadline; call right after the PingRequest went out.
    fn ping_sent(&mut self) {
        self.sent_at = Some(tokio::time::Instant::now());
        self.deadline
            .as_mut()
            .reset(tokio::time::Instant::now() + PONG_DEADLINE);
    }

    /// Pong received: measure RTT, log, disarm until the next ping.
    fn pong(&mut self, label: &str) {
        if let Some(t0) = self.sent_at.take() {
            let rtt = t0.elapsed();
            if rtt > std::time::Duration::from_secs(2) {
                tracing::warn!("{label}: ping RTT high: {}ms", rtt.as_millis());
            } else {
                tracing::debug!("{label}: pong rtt={}ms", rtt.as_millis());
            }
        }
        self.deadline
            .as_mut()
            .reset(tokio::time::Instant::now() + std::time::Duration::from_secs(3600));
    }
}

/// Generic ESPHome API client connection (initiator mode + Hello).
pub struct EspHomeClient {
    host: String,
    port: u16,
    noise_psk: String,
    name: String,
}

impl EspHomeClient {
    pub fn new(host: &str, port: u16, noise_psk: &str, name: &str) -> Self {
        Self {
            host: host.to_string(),
            port,
            noise_psk: noise_psk.to_string(),
            name: name.to_string(),
        }
    }

    /// Connect (initiator handshake) and send the HelloRequest.
    pub async fn connect(&self) -> Result<esphome_native_api::Connection, Error> {
        let stream = TcpStream::connect((self.host.as_str(), self.port))
            .await
            .map_err(Error::Io)?;
        set_tcp_keepalive(&stream);
        let api = EspHomeApi::builder()
            .name(self.name.clone())
            .api_version_major(1)
            .api_version_minor(14)
            .server_info("heatingrod-rust client".to_string())
            .encryption_key(self.noise_psk.clone())
            .initiator(true) // heatingrod patch — client handshake
            .build()?;
        let connection = api.start(stream).await?;
        // aioesphomeapi sends Hello first; the server replies HelloResponse.
        let _ = connection
            .sender()
            .send(ProtoMessage::HelloRequest(HelloRequest {
                api_version_major: 1,
                api_version_minor: 14,
                client_info: format!("heatingrod-rust {}", env!("CARGO_PKG_VERSION")),
            }))
            .await;
        Ok(connection)
    }

    pub fn is_wrong_key(err: &Error) -> bool {
        matches!(err, Error::Handshake(HandshakeError::MacFailure))
    }
}

/// Kaskade relay client (Brennersperre) — port of `kaskade.py`.
///
/// Switch semantics: ON = Brennersperre active = boiler BLOCKED.
/// Shared powerlogger state between the async client session and the
/// controller thread — updated LIVE on every push (not only at session end),
/// so the grid role source never has stale windows.
#[derive(Debug, Default)]
pub struct PlShared {
    pub last: Option<f64>,
    pub ts: f64,
    pub connected: bool,
    pub updates: u64,
    /// Seconds between the last two sensor updates (SML telegram cadence).
    pub last_interval: Option<f64>,
}

impl PlShared {
    pub fn get(&self, max_age: f64, now: f64) -> Option<f64> {
        let p = self.last?;
        if self.ts == 0.0 || now - self.ts > max_age {
            return None;
        }
        Some(p)
    }
}

/// Shared kaskade state between the async client session and the controller
/// thread (desired state set by the CH1 logic, actual tracked by the client).
#[derive(Debug, Default)]
pub struct KaskadeShared {
    pub desired: Option<bool>,
    pub actual: Option<bool>,
    pub connected: bool,
    /// Bumped on every desired change — a connected session polls this and
    /// sends immediately (Python `set_switch` sends at once; the 10s
    /// consistency tick alone would delay the Brennersperre by up to 10s).
    pub desired_seq: u64,
}

pub struct KaskadeClient {
    pub client: EspHomeClient,
    pub switch_object_id: String,
    pub reconnect_delay: f64,
    pub desired_state: Option<bool>,
    pub actual_state: Option<bool>,
    pub connected: bool,
    pub switch_key: Option<u32>,
    pub switch_device_id: u32,
    pub shared: Option<Arc<Mutex<KaskadeShared>>>,
    updates: u64,
    reconnect_backoff: f64,
}

impl KaskadeClient {
    pub fn new(
        host: &str,
        port: u16,
        noise_psk: &str,
        switch_object_id: &str,
        reconnect_delay: f64,
    ) -> Self {
        Self {
            client: EspHomeClient::new(host, port, noise_psk, "heatingrod-kaskade"),
            switch_object_id: switch_object_id.to_string(),
            reconnect_delay,
            desired_state: None,
            actual_state: None,
            connected: false,
            switch_key: None,
            switch_device_id: 0,
            shared: None,
            updates: 0,
            reconnect_backoff: reconnect_delay,
        }
    }

    /// Attach the shared state bridge (controller thread ↔ session).
    pub fn set_shared(&mut self, shared: Arc<Mutex<KaskadeShared>>) {
        self.shared = Some(shared);
    }

    /// One connection session: resolve key by object_id, subscribe, track
    /// actual state, apply a pending desired state. Returns when the
    /// connection ends (the caller reconnects after `reconnect_delay`).
    pub async fn run_session(&mut self) {
        self.connected = false;
        let connection = match self.client.connect().await {
            Ok(c) => c,
            Err(e) => {
                if EspHomeClient::is_wrong_key(&e) {
                    tracing::error!("kaskade: wrong encryption key: {e}");
                } else {
                    tracing::warn!("kaskade: connect failed: {e}");
                }
                return;
            }
        };
        tracing::info!("kaskade: connected to {}:{}", self.client.host, self.client.port);

        let tx = connection.sender();
        let mut rx = connection.receiver();
        let _ = tx.send(ProtoMessage::ListEntitiesRequest(
            esphome_api_server::proto_types::ListEntitiesRequest {},
        )).await;

        let mut listed = false;
        let mut consistency = tokio::time::interval(tokio::time::Duration::from_secs(10));
        consistency.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        // Fast desired-state poll (Python `set_switch` sends immediately):
        // a bumped desired_seq is sent within ~200ms instead of waiting for
        // the 10s consistency tick — matters for the Brennersperre.
        let mut desired_poll = tokio::time::interval(tokio::time::Duration::from_millis(200));
        desired_poll.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        let mut last_sent_seq: u64 = 0;
        let mut watchdog = Box::pin(tokio::time::sleep(tokio::time::Duration::from_secs(60)));
        let mut ping = PingMonitor::new();
        loop {
            tokio::select! {
                _ = &mut watchdog => {
                    tracing::warn!("kaskade: no traffic for 60s, forcing reconnect");
                    break;
                }
                _ = ping.interval.tick() => {
                    let _ = tx
                        .send(ProtoMessage::PingRequest(
                            esphome_api_server::proto_types::PingRequest {},
                        ))
                        .await;
                    ping.ping_sent();
                }
                _ = &mut ping.deadline => {
                    tracing::warn!(
                        "kaskade: no PingResponse for {}s, forcing reconnect",
                        PONG_DEADLINE.as_secs()
                    );
                    break;
                }
                _ = desired_poll.tick() => {
                    if let (Some(shared), Some(key)) = (&self.shared, self.switch_key) {
                        let (desired, seq) = {
                            let sh = shared.lock().expect("ksk lock");
                            (sh.desired, sh.desired_seq)
                        };
                        if let Some(desired) = desired
                            && seq != last_sent_seq
                        {
                            last_sent_seq = seq;
                            let _ = tx.send(ProtoMessage::SwitchCommandRequest(
                                SwitchCommandRequest {
                                    key,
                                    state: desired,
                                    device_id: self.switch_device_id,
                                },
                            )).await;
                            tracing::info!(
                                "kaskade: switch command sent: {} ({})",
                                switch_label(desired),
                                switch_meaning(desired)
                            );
                        }
                    }
                }
                _ = consistency.tick() => {
                    // Python `check_consistency`: re-send on desired/actual mismatch.
                    if let (Some(shared), Some(key)) = (&self.shared, self.switch_key) {
                        let (desired, actual) = {
                            let sh = shared.lock().expect("ksk lock");
                            (sh.desired, sh.actual)
                        };
                        if let Some(desired) = desired
                            && actual != Some(desired)
                        {
                            tracing::warn!(
                                "kaskade: mismatch actual={:?} desired={} — correcting",
                                actual,
                                switch_label(desired)
                            );
                            let _ = tx.send(ProtoMessage::SwitchCommandRequest(
                                SwitchCommandRequest {
                                    key,
                                    state: desired,
                                    device_id: self.switch_device_id,
                                },
                            )).await;
                        }
                    }
                }
                msg = rx.recv() => {
                    watchdog.as_mut().reset(tokio::time::Instant::now() + tokio::time::Duration::from_secs(60));
                    match msg {
                    Ok(ProtoMessage::ListEntitiesSwitchResponse(s)) => {
                        // API >= 1.14: object_id may be omitted on the wire —
                        // reconstruct it from the name (ESPHome algorithm).
                        let oid = if s.object_id.is_empty() {
                            compute_object_id(&s.name)
                        } else {
                            s.object_id.clone()
                        };
                        if oid == self.switch_object_id {
                            tracing::info!(
                                "kaskade: switch '{}' key={} device_id={}",
                                oid, s.key, s.device_id
                            );
                            self.switch_key = Some(s.key);
                            self.switch_device_id = s.device_id;
                        }
                    }
                    Ok(ProtoMessage::ListEntitiesDoneResponse(_)) => {
                        listed = true;
                        // Python: log.error when the configured object_id was
                        // not found — a silent session would leave the
                        // Brennersperre unmanaged without any trace.
                        if self.switch_key.is_none() {
                            tracing::error!(
                                "kaskade: switch object_id='{}' not found on device",
                                self.switch_object_id
                            );
                        }
                        // Subscribe to states after the entity list is complete.
                        let _ = tx.send(ProtoMessage::SubscribeStatesRequest(
                            esphome_api_server::proto_types::SubscribeStatesRequest {},
                        )).await;
                        // Apply a pending desired state (reconnect memory) —
                        // from the shared bridge if attached, else local.
                        let desired = self
                            .shared
                            .as_ref()
                            .and_then(|s| s.lock().expect("ksk lock").desired)
                            .or(self.desired_state);
                        if let (Some(key), Some(desired)) = (self.switch_key, desired) {
                            let _ = tx.send(ProtoMessage::SwitchCommandRequest(
                                SwitchCommandRequest {
                                    key,
                                    state: desired,
                                    device_id: self.switch_device_id,
                                },
                            )).await;
                            tracing::info!(
                                "kaskade: switch command sent: {} ({})",
                                switch_label(desired),
                                switch_meaning(desired)
                            );
                        }
                    }
                    Ok(ProtoMessage::SwitchStateResponse(s)) => {
                        if Some(s.key) == self.switch_key {
                            self.updates += 1;
                            // Python logs every state CHANGE on INFO.
                            if self.actual_state != Some(s.state) {
                                tracing::info!(
                                    "kaskade: switch state changed: {:?} → {}",
                                    self.actual_state.map(|b| if b { "ON" } else { "OFF" }),
                                    if s.state { "ON" } else { "OFF" }
                                );
                            }
                            self.actual_state = Some(s.state);
                            self.connected = true;
                            if let Some(shared) = &self.shared {
                                let mut sh = shared.lock().expect("ksk lock");
                                sh.actual = Some(s.state);
                                sh.connected = true;
                            }
                        }
                    }
                    Ok(ProtoMessage::HelloResponse(_)) => {}
                    Ok(ProtoMessage::GetTimeRequest(_)) => {
                        // Real ESPHome servers wait for the time reply before
                        // processing further requests (aioesphomeapi does this).
                        let now = std::time::SystemTime::now()
                            .duration_since(std::time::UNIX_EPOCH)
                            .map(|d| d.as_secs() as u32)
                            .unwrap_or(0);
                        let _ = tx
                            .send(ProtoMessage::GetTimeResponse(
                                esphome_api_server::proto_types::GetTimeResponse {
                                    epoch_seconds: now,
                                    timezone: String::new(),
                                    parsed_timezone: None,
                                },
                            ))
                            .await;
                    }
                    Ok(ProtoMessage::PingResponse(_)) => ping.pong("kaskade"),
                    Ok(ProtoMessage::PingRequest(_)) => {
                        // Real ESPs ping their clients and drop silent peers.
                        let _ = tx
                            .send(ProtoMessage::PingResponse(
                                esphome_api_server::proto_types::PingResponse {},
                            ))
                            .await;
                    }
                    Ok(other) => {
                        let d = format!("{other:?}");
                        let d: String = d.chars().take(80).collect();
                        tracing::debug!("kaskade: unhandled {d}");
                    }
                    Err(_) => break,
                }
                }
            }
        }
        self.connected = false;
        self.actual_state = None;
        if let Some(shared) = &self.shared {
            let mut sh = shared.lock().expect("ksk lock");
            sh.connected = false;
            // Python `_on_disconnect`: actual → None, STATUS shows `ksk=?`
            // instead of a stale last state.
            sh.actual = None;
        }
        tracing::warn!(
            "kaskade: disconnected unexpectedly ({} updates this session)",
            self.updates
        );
    }

    /// Set the desired relay state; applies immediately when connected,
    /// remembered across reconnects (Python `set_switch`).
    pub fn set_switch(&mut self, state: bool) {
        self.desired_state = Some(state);
    }

    /// Python `check_consistency`: re-send on actual/desired mismatch.
    pub fn consistency_ok(&self) -> bool {
        match (self.desired_state, self.actual_state) {
            (Some(d), Some(a)) => d == a,
            _ => true,
        }
    }

    pub fn stats(&self) -> (bool, u64) {
        (self.connected, self.updates)
    }
}

/// Powerlogger client (grid power) — port of `powerlogger.py`.
pub struct PowerloggerClient {
    pub client: EspHomeClient,
    pub entity_object_id: String,
    pub reconnect_delay: f64,
    pub sensor_key: Option<u32>,
    pub sensor_device_id: u32,
    pub last_power: Option<f64>,
    pub last_power_ts: f64,
    pub connected: bool,
    pub updates: u64,
    pub last_interval: f64,
    pub shared: Option<Arc<Mutex<PlShared>>>,
}

impl PowerloggerClient {
    pub fn new(host: &str, port: u16, noise_psk: &str, entity_object_id: &str, reconnect_delay: f64) -> Self {
        Self {
            client: EspHomeClient::new(host, port, noise_psk, "heatingrod-powerlogger"),
            entity_object_id: entity_object_id.to_string(),
            reconnect_delay,
            sensor_key: None,
            sensor_device_id: 0,
            last_power: None,
            last_power_ts: 0.0,
            connected: false,
            updates: 0,
            last_interval: 0.0,
            shared: None,
        }
    }

    /// Attach the shared state bridge (session ↔ controller thread).
    pub fn set_shared(&mut self, shared: Arc<Mutex<PlShared>>) {
        self.shared = Some(shared);
    }

    pub fn get_grid_power(&self, max_age: f64, now: f64) -> Option<f64> {
        let p = self.last_power?;
        if self.last_power_ts == 0.0 {
            return None;
        }
        if now - self.last_power_ts > max_age {
            return None;
        }
        Some(p)
    }

    /// One connection session: resolve sensor key by object_id, subscribe,
    /// track pushes (~3.3s SML telegram cadence).
    pub async fn run_session(&mut self) {
        self.connected = false;
        let connection = match self.client.connect().await {
            Ok(c) => c,
            Err(e) => {
                tracing::warn!("powerlogger: connect failed: {e}");
                return;
            }
        };
        tracing::info!("powerlogger: connected to {}:{}", self.client.host, self.client.port);

        let tx = connection.sender();
        let mut rx = connection.receiver();
        let _ = tx.send(ProtoMessage::ListEntitiesRequest(
            esphome_api_server::proto_types::ListEntitiesRequest {},
        )).await;

        // Idle watchdog: resets on EVERY received frame — a live session is
        // never torn down, a dead one is detected within 60s.
        let mut watchdog = Box::pin(tokio::time::sleep(tokio::time::Duration::from_secs(60)));
        let mut ping = PingMonitor::new();
        let mut prev_ts: Option<f64> = None;
        loop {
            tokio::select! {
                _ = &mut watchdog => {
                    tracing::warn!("powerlogger: no traffic for 60s, forcing reconnect");
                    break;
                }
                _ = ping.interval.tick() => {
                    let _ = tx
                        .send(ProtoMessage::PingRequest(
                            esphome_api_server::proto_types::PingRequest {},
                        ))
                        .await;
                    ping.ping_sent();
                }
                _ = &mut ping.deadline => {
                    tracing::warn!(
                        "powerlogger: no PingResponse for {}s, forcing reconnect",
                        PONG_DEADLINE.as_secs()
                    );
                    break;
                }
                msg = rx.recv() => {
                    watchdog.as_mut().reset(tokio::time::Instant::now() + tokio::time::Duration::from_secs(60));
                    match msg {
                    Ok(ProtoMessage::ListEntitiesSensorResponse(s)) => {
                        let oid = if s.object_id.is_empty() {
                            compute_object_id(&s.name)
                        } else {
                            s.object_id.clone()
                        };
                        if oid == self.entity_object_id {
                            tracing::info!(
                                "powerlogger: sensor '{}' key={} device_id={}",
                                oid, s.key, s.device_id
                            );
                            self.sensor_key = Some(s.key);
                            self.sensor_device_id = s.device_id;
                        }
                    }
                    Ok(ProtoMessage::ListEntitiesDoneResponse(_)) => {
                        // Python: log.error when the configured object_id was
                        // not found — a silent session would leave the grid
                        // source on the HA fallback without any trace.
                        if self.sensor_key.is_none() {
                            tracing::error!(
                                "powerlogger: sensor object_id='{}' not found on device",
                                self.entity_object_id
                            );
                        }
                        let _ = tx.send(ProtoMessage::SubscribeStatesRequest(
                            esphome_api_server::proto_types::SubscribeStatesRequest {},
                        )).await;
                    }
                    Ok(ProtoMessage::SensorStateResponse(s)) => {
                        if Some(s.key) == self.sensor_key && !s.missing_state {
                            let now = std::time::SystemTime::now()
                                .duration_since(std::time::UNIX_EPOCH)
                                .map(|d| d.as_secs_f64())
                                .unwrap_or(0.0);
                            if let Some(prev) = prev_ts {
                                self.last_interval = now - prev;
                                // Python: warn on telegram gaps (>10s) so a
                                // stalling SML stream is visible in the log.
                                if self.last_interval > 10.0 {
                                    tracing::warn!(
                                        "powerlogger: gap: {:.1}s since last update",
                                        self.last_interval
                                    );
                                }
                            }
                            prev_ts = Some(now);
                            self.updates += 1;
                            self.last_power = Some(s.state as f64);
                            self.last_power_ts = now;
                            self.connected = true;
                            if let Some(shared) = &self.shared {
                                let mut sh = shared.lock().expect("pl lock");
                                sh.last = Some(s.state as f64);
                                sh.ts = now;
                                sh.connected = true;
                                sh.updates = self.updates;
                                if self.last_interval > 0.0 {
                                    sh.last_interval = Some(self.last_interval);
                                }
                            }
                            tracing::debug!("powerlogger: grid = {:.0}W", s.state);
                        }
                    }
                    Ok(ProtoMessage::HelloResponse(_)) => {}
                    Ok(ProtoMessage::GetTimeRequest(_)) => {
                        let now = std::time::SystemTime::now()
                            .duration_since(std::time::UNIX_EPOCH)
                            .map(|d| d.as_secs() as u32)
                            .unwrap_or(0);
                        let _ = tx
                            .send(ProtoMessage::GetTimeResponse(
                                esphome_api_server::proto_types::GetTimeResponse {
                                    epoch_seconds: now,
                                    timezone: String::new(),
                                    parsed_timezone: None,
                                },
                            ))
                            .await;
                    }
                    Ok(ProtoMessage::PingResponse(_)) => ping.pong("powerlogger"),
                    Ok(ProtoMessage::PingRequest(_)) => {
                        // Real ESPs ping their clients and drop silent peers.
                        let _ = tx
                            .send(ProtoMessage::PingResponse(
                                esphome_api_server::proto_types::PingResponse {},
                            ))
                            .await;
                    }
                    Ok(_) => {}
                    Err(_) => break,
                }
                }
            }
        }
        self.connected = false;
        if let Some(shared) = &self.shared {
            shared.lock().expect("pl lock").connected = false;
        }
        tracing::warn!("powerlogger: disconnected unexpectedly ({} updates)", self.updates);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Manual check against the REAL kaskade ESP — READ-ONLY: hello, list
    /// entities, subscribe, track the actual switch state. Never sends
    /// commands. Requires the production config (gitignored).
    ///
    /// Run: cargo test -p heatingrod --bin heatingrod real_kaskade_probe -- --ignored --nocapture
    #[tokio::test]
    #[ignore]
    async fn real_kaskade_probe() {
        let cfg_path = concat!(env!("CARGO_MANIFEST_DIR"), "/../../config/production.yaml");
        let config =
            crate::config::Config::load(std::path::Path::new(cfg_path)).expect("production config");
        let mut client = KaskadeClient::new(
            &config.kaskade.host,
            config.kaskade.port,
            &config.kaskade.noise_psk,
            &config.kaskade.switch_object_id,
            5.0,
        );
        eprintln!(
            "probe: host={}:{} psk_len={}",
            config.kaskade.host,
            config.kaskade.port,
            config.kaskade.noise_psk.len()
        );
        // Raw connect first to surface handshake errors.
        match client.client.connect().await {
            Ok(_) => eprintln!("probe: raw connect OK"),
            Err(e) => {
                eprintln!("probe: raw connect FAILED: {e:?}");
                return;
            }
        }
        let _ = tokio::time::timeout(std::time::Duration::from_secs(12), client.run_session()).await;
        println!(
            "probe: connected={} switch_key={:?} actual_state={:?} updates={}",
            client.connected, client.switch_key, client.actual_state, client.updates
        );
        assert!(
            client.switch_key.is_some(),
            "switch key must resolve by object_id"
        );
    }

    /// Same probe for the powerlogger ESP.
    ///
    /// Run: cargo test -p heatingrod --bin heatingrod real_powerlogger_probe -- --ignored --nocapture
    #[tokio::test]
    #[ignore]
    async fn real_powerlogger_probe() {
        let cfg_path = concat!(env!("CARGO_MANIFEST_DIR"), "/../../config/production.yaml");
        let config =
            crate::config::Config::load(std::path::Path::new(cfg_path)).expect("production config");
        let mut client = PowerloggerClient::new(
            &config.powerlogger.host,
            config.powerlogger.port,
            &config.powerlogger.noise_psk,
            &config.powerlogger.entity_object_id,
            5.0,
        );
        let _ = tokio::time::timeout(std::time::Duration::from_secs(15), client.run_session()).await;
        println!(
            "probe: connected={} sensor_key={:?} last_power={:?} updates={}",
            client.connected, client.sensor_key, client.last_power, client.updates
        );
        assert!(client.sensor_key.is_some(), "sensor key must resolve");
    }
}


// appended test: initiator vs our own responder

mod initiator_local_tests {
    use super::*;
    use esphome_api_server::{ApiServer, ServerConfig};
    use std::sync::Arc;

    #[tokio::test]
    async fn initiator_against_our_responder() {
        let port = 16666u16;
        let mut scfg = ServerConfig {
            name: "test-esp".into(),
            friendly_name: "Test ESP".into(),
            mac_address: "AA:BB:CC:DD:EE:01".into(),
            bind_address: "127.0.0.1".into(),
            port,
            noise_psk: "AAECAwQFBgcICQoLDA0ODxAREhMUFRYXGBkaGxwdHh8=".into(),
            ..Default::default()
        };
        scfg.normalize();
        let (tx, _rx) = tokio::sync::mpsc::channel(16);
        let (htx, _hrx) = tokio::sync::mpsc::channel(16);
        let server = Arc::new(ApiServer::new(scfg, tx, htx));
        let s = server.clone();
        tokio::spawn(async move { let _ = s.run().await; });

        let client = EspHomeClient::new(
            "127.0.0.1",
            port,
            "AAECAwQFBgcICQoLDA0ODxAREhMUFRYXGBkaGxwdHh8=",
            "initiator-test",
        );
        // Retry: the accept loop binds asynchronously.
        let mut attempts = 0;
        let connection = loop {
            match client.connect().await {
                Ok(c) => break c,
                Err(e) if attempts < 20 => {
                    attempts += 1;
                    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
                    let _ = e;
                }
                Err(e) => {
                    eprintln!("initiator connect FAILED: {e:?}");
                    panic!("connect failed");
                }
            }
        };
        eprintln!("initiator vs responder: CONNECT OK");
        drop(connection);
    }
}
