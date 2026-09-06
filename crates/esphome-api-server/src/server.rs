//! ESPHome Native API server — accept loop, per-connection dispatch,
//! subscriber broadcast.
//!
//! Built on the vendored `esphome-native-api` low-level `EspHomeApi` (Noise
//! handshake, framing, per-connection encryption, auto-replies for
//! Hello/DeviceInfo/Ping/Disconnect/Authentication). This layer adds what the
//! Python `server.py` provides on top: entity registry serving, subscriber
//! management, command dispatch, HA-state store, log frames.

use std::sync::{Arc, Mutex, RwLock};

use esphome_native_api::esphomeapi::EspHomeApi;
use esphome_native_api::parser::ProtoMessage;
use esphome_native_api::proto::{
    GetTimeResponse, NoiseEncryptionSetKeyResponse, PingResponse,
    SubscribeHomeAssistantStateResponse, SubscribeLogsResponse,
};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::mpsc;
use tokio::sync::mpsc::error::TrySendError;

use crate::entities::{ClimateEntity, NumberEntity};
use crate::ha_state::HaStateStore;
use crate::registry::{Registry, SensorEntry, SensorValue};

/// Command from HA (mirrors Python `on_climate_command`/`on_number_command`).
#[derive(Debug, Clone)]
pub enum Command {
    Climate {
        key: u32,
        mode: Option<i32>,
        target_temperature: Option<f32>,
    },
    Number { key: u32, value: f32 },
}

/// HA state push event (mirrors Python `on_ha_state`).
#[derive(Debug, Clone)]
pub struct HaStateEvent {
    pub entity_id: String,
    pub state: String,
}

/// Device identity + listener configuration (mirrors Python `DeviceConfig`).
#[derive(Debug, Clone)]
pub struct ServerConfig {
    pub name: String,
    pub friendly_name: String,
    /// Uppercased by [`ServerConfig::normalize`] — HA relies on it (CLAUDE.md).
    pub mac_address: String,
    pub model: String,
    pub manufacturer: String,
    pub esphome_version: String,
    pub project_name: String,
    pub project_version: String,
    pub bind_address: String,
    pub port: u16,
    /// Base64 32-byte PSK; empty = no encryption.
    pub noise_psk: String,
    /// Accept plaintext even with a PSK set — DEBUG ONLY (exposes entities).
    pub allow_plaintext: bool,
    /// Sub-devices reported in DeviceInfoResponse.
    pub devices: Vec<(u32, String)>,
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            name: "esphome-device".into(),
            friendly_name: "ESPHome Device".into(),
            mac_address: "AA:BB:CC:DD:EE:FF".into(),
            model: String::new(),
            manufacturer: String::new(),
            esphome_version: "2026.6.2".into(),
            project_name: String::new(),
            project_version: "1.0.0".into(),
            bind_address: "0.0.0.0".into(),
            port: 6053,
            noise_psk: String::new(),
            allow_plaintext: false,
            devices: Vec::new(),
        }
    }
}

impl ServerConfig {
    /// Uppercase the MAC like Python's `DeviceConfig.__post_init__`.
    pub fn normalize(&mut self) {
        self.mac_address = self.mac_address.to_uppercase();
    }
}

/// Per-connection senders that receive state pushes and log frames.
#[derive(Debug, Default)]
struct Hub {
    subscribers: Vec<(u64, mpsc::Sender<ProtoMessage>)>,
    log_subscribers: Vec<(u64, mpsc::Sender<ProtoMessage>, i32)>,
}

/// Removes a connection's entries from the hub when the connection ends.
struct SubscriberGuard {
    hub: Arc<Mutex<Hub>>,
    id: u64,
}

impl Drop for SubscriberGuard {
    fn drop(&mut self) {
        if let Ok(mut hub) = self.hub.lock() {
            hub.subscribers.retain(|(id, _)| *id != self.id);
            hub.log_subscribers.retain(|(id, _, _)| *id != self.id);
        }
    }
}

pub struct ApiServer {
    config: ServerConfig,
    registry: Arc<RwLock<Registry>>,
    ha_states: Arc<Mutex<HaStateStore>>,
    hub: Arc<Mutex<Hub>>,
    ha_entities: Arc<RwLock<Vec<String>>>,
    command_tx: mpsc::Sender<Command>,
    ha_event_tx: mpsc::Sender<HaStateEvent>,
    /// Instance with PSK (handshake requires key when configured).
    api_keyed: EspHomeApi,
    /// Instance without PSK — used for plaintext clients in allow_plaintext
    /// mode (the crate rejects plaintext whenever a key is configured).
    api_plain: EspHomeApi,
    next_sub_id: std::sync::atomic::AtomicU64,
}

/// `%Y-%m-%d %H:%M:%S %z` like Python's `time.strftime` in the reference.
fn compilation_time_now() -> String {
    unsafe {
        let now = libc::time(std::ptr::null_mut());
        let mut tm: libc::tm = std::mem::zeroed();
        libc::localtime_r(&now, &mut tm);
        let mut buf = [0u8; 64];
        let fmt = b"%Y-%m-%d %H:%M:%S %z\0";
        let n = libc::strftime(
            buf.as_mut_ptr() as *mut libc::c_char,
            buf.len(),
            fmt.as_ptr() as *const libc::c_char,
            &tm,
        );
        String::from_utf8_lossy(&buf[..n]).into_owned()
    }
}

impl ApiServer {
    pub fn new(
        mut config: ServerConfig,
        command_tx: mpsc::Sender<Command>,
        ha_event_tx: mpsc::Sender<HaStateEvent>,
    ) -> Self {
        config.normalize();

        let devices: Vec<esphome_native_api::proto::DeviceInfo> = config
            .devices
            .iter()
            .map(|(id, name)| esphome_native_api::proto::DeviceInfo {
                device_id: *id,
                name: name.clone(),
                area_id: 0,
            })
            .collect();
        let server_info = format!("{} {}", config.project_name, config.project_version);
        let compilation_time = compilation_time_now();

        let build = |with_key: bool| {
            let builder = EspHomeApi::builder()
                .name(config.name.clone())
                .friendly_name(config.friendly_name.clone())
                .mac(config.mac_address.clone())
                .model(config.model.clone())
                .manufacturer(config.manufacturer.clone())
                .project_name(config.project_name.clone())
                .project_version(config.project_version.clone())
                .esphome_version(config.esphome_version.clone())
                .devices(devices.clone())
                .api_version_major(1)
                .api_version_minor(14)
                .server_info(server_info.clone())
                .compilation_time(compilation_time.clone());
            if with_key {
                builder
                    .encryption_key(config.noise_psk.clone())
                    .build()
                    .expect("EspHomeApi config must be valid")
            } else {
                builder.build().expect("EspHomeApi config must be valid")
            }
        };

        Self {
            api_keyed: build(true),
            api_plain: build(false),
            config,
            registry: Arc::new(RwLock::new(Registry::new())),
            ha_states: Arc::new(Mutex::new(HaStateStore::default())),
            hub: Arc::new(Mutex::new(Hub::default())),
            ha_entities: Arc::new(RwLock::new(Vec::new())),
            command_tx,
            ha_event_tx,
            next_sub_id: std::sync::atomic::AtomicU64::new(1),
        }
    }

    // ---- entity registration (mirrors Python add_*) ----

    pub fn add_sensor(&self, entry: SensorEntry) -> u32 {
        self.registry.write().expect("registry lock").add_sensor(entry)
    }

    pub fn add_number(&self, number: NumberEntity) -> u32 {
        self.registry.write().expect("registry lock").add_number(number)
    }

    pub fn add_climate(&self, climate: ClimateEntity) -> u32 {
        self.registry.write().expect("registry lock").add_climate(climate)
    }

    // ---- state updates (mirrors Python update_* + _push) ----

    pub fn update_sensor(&self, key: u32, value: SensorValue) {
        let frame = self
            .registry
            .write()
            .expect("registry lock")
            .update_sensor(key, value);
        if let Some(frame) = frame {
            self.broadcast(&frame);
        }
    }

    pub fn update_number(&self, key: u32, value: f64) {
        let frame = self
            .registry
            .write()
            .expect("registry lock")
            .update_number(key, value);
        if let Some(frame) = frame {
            self.broadcast(&frame);
        }
    }

    pub fn update_climate(
        &self,
        key: u32,
        mode: i32,
        target_temperature: f64,
        current_temperature: Option<f64>,
        action: Option<i32>,
    ) {
        let frame = self
            .registry
            .write()
            .expect("registry lock")
            .update_climate(key, mode, target_temperature, current_temperature, action);
        if let Some(frame) = frame {
            self.broadcast(&frame);
        }
    }

    /// Push a state frame to all subscribers.
    ///
    /// A subscriber is only dropped when its connection is CLOSED. A FULL
    /// channel (slow consumer, e.g. HA briefly not reading while we burst a
    /// 71-frame full push) must NOT evict the connection — the missed update
    /// self-heals on the next push, but an eviction here leaves HA connected
    /// yet deaf (subscriber_count()=0 with a live TCP session — observed
    /// 04.09.2026).
    /// The crate encrypts per connection in its write loop.
    fn broadcast(&self, frame: &ProtoMessage) {
        let mut hub = self.hub.lock().expect("hub lock");
        hub.subscribers.retain(|(_, tx)| match tx.try_send(frame.clone()) {
            Ok(()) => true,
            Err(TrySendError::Full(_)) => true,    // slow consumer: skip frame, keep it
            Err(TrySendError::Closed(_)) => false, // connection gone
        });
    }

    // ---- HA entity subscription (mirrors subscribe_ha_entity/get_ha_state) ----

    pub fn subscribe_ha_entity(&self, entity_id: &str) {
        self.ha_entities
            .write()
            .expect("ha_entities lock")
            .push(entity_id.to_string());
    }

    pub fn get_ha_state(&self, entity_id: &str, max_age: std::time::Duration) -> Option<String> {
        self.ha_states
            .lock()
            .expect("ha_states lock")
            .get(entity_id, max_age)
            .map(str::to_string)
    }

    // ---- log frames (mirrors send_log) ----

    /// Send a log message to all log subscribers (ESPHome: lower = more severe).
    pub fn send_log(&self, level: i32, message: &str) {
        let frame = ProtoMessage::SubscribeLogsResponse(SubscribeLogsResponse {
            level,
            message: message.as_bytes().to_vec(),
        });
        let mut hub = self.hub.lock().expect("hub lock");
        hub.log_subscribers.retain(|(_, tx, min_level)| {
            level > *min_level
                || match tx.try_send(frame.clone()) {
                    Ok(()) => true,
                    Err(TrySendError::Full(_)) => true,
                    Err(TrySendError::Closed(_)) => false,
                }
        });
    }

    pub fn subscriber_count(&self) -> usize {
        self.hub.lock().expect("hub lock").subscribers.len()
    }

    /// Snapshot of all registered entity keys (parity tests).
    pub fn registry_snapshot(&self) -> Vec<u32> {
        self.registry.read().expect("registry lock").all_keys()
    }

    /// Which api instance handles a client that sent this first byte.
    fn api_for_preamble(&self, preamble: u8) -> Option<&EspHomeApi> {
        match preamble {
            0x00 => {
                // Plaintext client. The keyed instance rejects it with the
                // error frame that makes HA prompt for the key — unless
                // allow_plaintext explicitly opens the door (debug only).
                if !self.config.noise_psk.is_empty() && !self.config.allow_plaintext {
                    Some(&self.api_keyed)
                } else {
                    Some(&self.api_plain)
                }
            }
            0x01 => Some(&self.api_keyed),
            _ => None,
        }
    }

    /// Accept loop; spawned per-connection tasks die with the runtime.
    pub async fn run(self: Arc<Self>) -> std::io::Result<()> {
        let listener =
            TcpListener::bind((self.config.bind_address.as_str(), self.config.port)).await?;
        tracing::info!(
            "ESPHome API server listening on {}:{} ({} sensors, {} numbers, {} climates)",
            self.config.bind_address,
            self.config.port,
            self.registry.read().expect("registry lock").sensor_count(),
            self.registry.read().expect("registry lock").number_count(),
            self.registry.read().expect("registry lock").climate_count(),
        );
        loop {
            let (stream, peer) = listener.accept().await?;
            let server = self.clone();
            tokio::spawn(async move {
                handle_connection(server, stream, peer).await;
            });
        }
    }
}

async fn handle_connection(server: Arc<ApiServer>, stream: TcpStream, peer: std::net::SocketAddr) {
    tracing::info!("ESPHome API client connected from {peer}");

    // Peek (non-consuming) to pick keyed vs plain instance — mirrors the
    // Python preamble detection including allow_plaintext semantics.
    let mut first = [0u8; 1];
    let preamble = match stream.peek(&mut first).await {
        Ok(0) => {
            tracing::debug!("client {peer} closed before sending anything");
            return;
        }
        Ok(_) => first[0],
        Err(e) => {
            tracing::warn!("peek on {peer} failed: {e}");
            return;
        }
    };
    let api = match server.api_for_preamble(preamble) {
        Some(api) => api.clone(),
        None => {
            tracing::warn!("client {peer} sent invalid preamble 0x{preamble:02X}");
            return;
        }
    };

    let connection = match api.start(stream).await {
        Ok(c) => c,
        Err(e) => {
            tracing::info!("handshake with {peer} failed: {e}");
            return;
        }
    };

    let tx = connection.sender();
    let mut rx = connection.receiver();
    let sub_id = server
        .next_sub_id
        .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let _guard = SubscriberGuard {
        hub: server.hub.clone(),
        id: sub_id,
    };

    loop {
        match rx.recv().await {
            Ok(ProtoMessage::PingRequest(_)) => {
                // Real ESPs answer pings — a silent server looks dead to a
                // client that runs its own liveness probes.
                let _ = tx.send(ProtoMessage::PingResponse(PingResponse {})).await;
            }
            Ok(ProtoMessage::ListEntitiesRequest(_)) => {
                tracing::debug!("ListEntitiesRequest from {peer}");
                let frames = server.registry.read().expect("registry lock").list_frames();
                for frame in frames {
                    if tx.send(frame).await.is_err() {
                        return;
                    }
                }
            }
            Ok(ProtoMessage::SubscribeStatesRequest(_)) => {
                tracing::debug!("SubscribeStatesRequest from {peer}");
                // Register BEFORE the initial burst so concurrent pushes are
                // ordered after it (Python: subscriber registration, then burst).
                // Dedup: a second SubscribeStates on the same connection must
                // not create a second hub entry (double delivery, Python
                // deduplicates in server.py).
                {
                    let mut hub = server.hub.lock().expect("hub lock");
                    if !hub.subscribers.iter().any(|(id, _)| *id == sub_id) {
                        hub.subscribers.push((sub_id, tx.clone()));
                    }
                }
                let frames = server
                    .registry
                    .read()
                    .expect("registry lock")
                    .initial_state_frames();
                for frame in frames {
                    if tx.send(frame).await.is_err() {
                        return;
                    }
                }
            }
            Ok(ProtoMessage::SubscribeLogsRequest(l)) => {
                let min_level = if l.level != 0 { l.level } else { 3 };
                tracing::info!("ESPHome log subscriber added (level={min_level})");
                let mut hub = server.hub.lock().expect("hub lock");
                if !hub.log_subscribers.iter().any(|(id, _, _)| *id == sub_id) {
                    hub.log_subscribers.push((sub_id, tx.clone(), min_level));
                }
            }
            Ok(ProtoMessage::GetTimeRequest(_)) => {
                // Python answers GetTimeRequest; a client that expects the
                // reply (real-ESP parity) stalls without it.
                let epoch = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_secs() as u32)
                    .unwrap_or(0);
                let _ = tx
                    .send(ProtoMessage::GetTimeResponse(GetTimeResponse {
                        epoch_seconds: epoch,
                        timezone: String::new(),
                        parsed_timezone: None,
                    }))
                    .await;
            }
            Ok(ProtoMessage::ClimateCommandRequest(c)) => {
                let mode = if c.has_mode { Some(c.mode) } else { None };
                let target = if c.has_target_temperature {
                    Some(c.target_temperature)
                } else {
                    None
                };
                tracing::debug!(
                    "ClimateCommand: key={} mode={mode:?} target={target:?}",
                    c.key
                );
                let _ = server
                    .command_tx
                    .send(Command::Climate {
                        key: c.key,
                        mode,
                        target_temperature: target,
                    })
                    .await;
            }
            Ok(ProtoMessage::NumberCommandRequest(n)) => {
                tracing::debug!("NumberCommand: key={} value={:.1}", n.key, n.state);
                let _ = server
                    .command_tx
                    .send(Command::Number {
                        key: n.key,
                        value: n.state,
                    })
                    .await;
            }
            Ok(ProtoMessage::NoiseEncryptionSetKeyRequest(_)) => {
                // Key is config-managed — decline, otherwise the client stalls
                // in its 10s timeout on every connect (CLAUDE.md).
                tracing::info!("Declining Noise key provisioning request (key is config-managed)");
                let _ = tx
                    .send(ProtoMessage::NoiseEncryptionSetKeyResponse(
                        NoiseEncryptionSetKeyResponse { success: false },
                    ))
                    .await;
            }
            Ok(ProtoMessage::HomeAssistantStateResponse(h)) => {
                tracing::debug!("HA state: {} = {}", h.entity_id, h.state);
                server
                    .ha_states
                    .lock()
                    .expect("ha_states lock")
                    .store(&h.entity_id, &h.state);
                let _ = server
                    .ha_event_tx
                    .send(HaStateEvent {
                        entity_id: h.entity_id,
                        state: h.state,
                    })
                    .await;
            }
            Ok(ProtoMessage::SubscribeHomeAssistantStatesRequest(_)) => {
                let entities = server.ha_entities.read().expect("ha_entities lock").clone();
                if entities.is_empty() {
                    continue;
                }
                tracing::info!(
                    "Subscribed to {} HA entities via ESPHome API",
                    entities.len()
                );
                for entity_id in entities {
                    let _ = tx
                        .send(ProtoMessage::SubscribeHomeAssistantStateResponse(
                            SubscribeHomeAssistantStateResponse {
                                entity_id,
                                attribute: String::new(),
                                once: false,
                            },
                        ))
                        .await;
                }
            }
            Ok(other) => {
                let d = format!("{other:?}");
                let d: String = d.chars().take(100).collect();
                tracing::debug!("unhandled inbound from {peer}: {d}");
            }
            Err(_) => break, // connection closed
        }
    }
    tracing::info!("ESPHome API client disconnected from {peer}");
}
