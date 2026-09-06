//! Phase-0 spike: verifies the esphome-native-api surface we build on.
//!
//! Runtime checks:
//! 1. Patched builder: `devices_opt` + `esphome_version_opt` reach the
//!    DeviceInfo auto-reply (see plan/CRATE-PATCHES.md).
//! 2. HelloResponse reports api_version 1/14.
//! 3. `NoiseEncryptionSetKeyRequest` arrives on our receiver; we reply
//!    `success=false` (config-managed key, HA would otherwise stall 10s).
//! 4. Client mode probe: `start()` is responder-only (peeks the first byte)
//!    — an initiator connection deadlocks. Expect TIMEOUT here.
//! 5. Plaintext-rejection wire behavior: a plaintext client against a
//!    key-configured server gets a 0x01-preamble error frame.
//!
//! Run: `cargo run -p esphome-api-server --example crate_spike`
//! Cross-check with the heatingrod venv (aioesphomeapi, the same library HA
//! uses):
//!   .venv/bin/python -c "..."   # see tests-py/ in Phase 2
//!
//! Spike key (base64 of bytes 0..32, printed at startup):
//!   AAECAwQFBgcICQoLDA0ODxAREhMUFRYXGBkaGxwdHh8=

use std::time::Duration;

use esphome_native_api::Error;
use esphome_native_api::esphomeapi::EspHomeApi;
use esphome_native_api::parser::ProtoMessage;
use esphome_native_api::proto::{
    ClimateStateResponse, DeviceInfo, ListEntitiesBinarySensorResponse,
    ListEntitiesClimateResponse, ListEntitiesDoneResponse, ListEntitiesNumberResponse,
    ListEntitiesSensorResponse, NoiseEncryptionSetKeyResponse, SensorStateResponse,
    SubscribeLogsResponse,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::time::timeout;

const LISTEN_ADDR: &str = "127.0.0.1:6054";
const PLAINTEXT_PROBE_ADDR: &str = "127.0.0.1:6055";
const SPIKE_KEY: &str = "AAECAwQFBgcICQoLDA0ODxAREhMUFRYXGBkaGxwdHh8=";
const KASKADE_HOST: &str = "esp-heizung-ksk.kalnet.hooya.de:6053";

const SENSOR_KEY: u32 = 0x1234_5678;
const CLIMATE_KEY: u32 = 0x2345_6789;
const NUMBER_KEY: u32 = 0x3456_789a;
const BINARY_KEY: u32 = 0x4567_89ab;

fn server_api() -> Result<EspHomeApi, Error> {
    EspHomeApi::builder()
        .name("heatingrod-test".to_string())
        .friendly_name("Heizstab Rust Test".to_string())
        .mac("B8:27:EB:96:AE:01".to_string())
        .model("Heatingrod v3".to_string())
        .manufacturer("Kalnet".to_string())
        .project_name("custom.heatingrod".to_string())
        .project_version("2.0.0".to_string())
        .esphome_version("2026.6.2".to_string())
        .devices(vec![
            DeviceInfo {
                device_id: 1,
                name: "DS100 Energiezähler".to_string(),
                area_id: 0,
            },
            DeviceInfo {
                device_id: 2,
                name: "Thyristorsteller".to_string(),
                area_id: 0,
            },
        ])
        .api_version_major(1)
        .api_version_minor(14)
        .server_info("custom.heatingrod 2.0.0".to_string())
        .encryption_key(SPIKE_KEY.to_string())
        .build()
}

#[allow(deprecated)]
fn list_frames() -> Vec<ProtoMessage> {
    vec![
        ProtoMessage::ListEntitiesSensorResponse(ListEntitiesSensorResponse {
            object_id: "spike_uptime".into(),
            key: SENSOR_KEY,
            name: "Spike Uptime".into(),
            icon: "".into(),
            unit_of_measurement: "s".into(),
            accuracy_decimals: 0,
            force_update: false,
            device_class: "duration".into(),
            state_class: 1, // measurement
            legacy_last_reset_type: 0,
            disabled_by_default: false,
            entity_category: 0,
            device_id: 0,
        }),
        ProtoMessage::ListEntitiesBinarySensorResponse(ListEntitiesBinarySensorResponse {
            object_id: "spike_flag".into(),
            key: BINARY_KEY,
            name: "Spike Flag".into(),
            device_class: "running".into(),
            is_status_binary_sensor: false,
            disabled_by_default: false,
            icon: "".into(),
            entity_category: 0,
            device_id: 0,
        }),
        ProtoMessage::ListEntitiesClimateResponse(ListEntitiesClimateResponse {
            object_id: "spike_climate".into(),
            key: CLIMATE_KEY,
            name: "Spike Climate".into(),
            supports_current_temperature: true,
            supports_two_point_target_temperature: false,
            supported_modes: vec![3, 0], // HEAT, OFF
            visual_min_temperature: 0.0,
            visual_max_temperature: 100.0,
            visual_target_temperature_step: 1.0,
            legacy_supports_away: false,
            supports_action: true,
            supported_fan_modes: vec![],
            supported_swing_modes: vec![],
            supported_custom_fan_modes: vec![],
            supported_presets: vec![],
            supported_custom_presets: vec![],
            disabled_by_default: false,
            icon: "".into(),
            entity_category: 0,
            visual_current_temperature_step: 1.0,
            supports_current_humidity: false,
            supports_target_humidity: false,
            visual_min_humidity: 0.0,
            visual_max_humidity: 0.0,
            device_id: 0,
            feature_flags: 0,
            temperature_unit: 0,
        }),
        ProtoMessage::ListEntitiesNumberResponse(ListEntitiesNumberResponse {
            object_id: "spike_power_limit".into(),
            key: NUMBER_KEY,
            name: "Spike Power Limit".into(),
            icon: "".into(),
            min_value: 0.0,
            max_value: 7500.0,
            step: 100.0,
            disabled_by_default: false,
            entity_category: 0,
            unit_of_measurement: "W".into(),
            mode: 2, // NUMBER_MODE_SLIDER
            device_class: "power".into(),
            device_id: 0,
        }),
        ProtoMessage::ListEntitiesDoneResponse(ListEntitiesDoneResponse {}),
    ]
}

#[allow(deprecated)]
fn initial_burst() -> Vec<ProtoMessage> {
    vec![
        ProtoMessage::SensorStateResponse(SensorStateResponse {
            key: SENSOR_KEY,
            state: 0.0,
            missing_state: true,
            device_id: 0,
        }),
        ProtoMessage::ClimateStateResponse(ClimateStateResponse {
            key: CLIMATE_KEY,
            mode: 0,
            current_temperature: 0.0,
            target_temperature: 60.0,
            target_temperature_low: 0.0,
            target_temperature_high: 0.0,
            unused_legacy_away: false,
            action: 0,
            fan_mode: 0,
            swing_mode: 0,
            custom_fan_mode: "".into(),
            preset: 0,
            custom_preset: "".into(),
            current_humidity: 0.0,
            target_humidity: 0.0,
            device_id: 0,
        }),
    ]
}

/// Per-connection dispatch: everything the crate forwards to us.
async fn handle_connection(connection: esphome_native_api::Connection) {
    let tx = connection.sender();
    let mut rx = connection.receiver();
    let start = std::time::Instant::now();

    let dispatch = async {
        let mut interval = tokio::time::interval(Duration::from_secs(5));
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            tokio::select! {
                msg = rx.recv() => match msg {
                    Ok(ProtoMessage::ListEntitiesRequest(_)) => {
                        println!("[dispatch] ListEntitiesRequest -> sending list frames");
                        for frame in list_frames() {
                            let _ = tx.send(frame).await;
                        }
                    }
                    Ok(ProtoMessage::SubscribeStatesRequest(_)) => {
                        println!("[dispatch] SubscribeStatesRequest -> initial burst");
                        for frame in initial_burst() {
                            let _ = tx.send(frame).await;
                        }
                    }
                    Ok(ProtoMessage::NoiseEncryptionSetKeyRequest(_)) => {
                        println!("[dispatch] NoiseEncryptionSetKeyRequest -> reply success=false");
                        let _ = tx
                            .send(ProtoMessage::NoiseEncryptionSetKeyResponse(
                                NoiseEncryptionSetKeyResponse { success: false },
                            ))
                            .await;
                    }
                    Ok(ProtoMessage::ClimateCommandRequest(c)) => {
                        println!(
                            "[dispatch] ClimateCommand: key={} has_mode={} mode={} has_target={} target={:.1}",
                            c.key, c.has_mode, c.mode, c.has_target_temperature, c.target_temperature
                        );
                    }
                    Ok(ProtoMessage::NumberCommandRequest(n)) => {
                        println!("[dispatch] NumberCommand: key={} state={:.1}", n.key, n.state);
                    }
                    Ok(ProtoMessage::HomeAssistantStateResponse(h)) => {
                        println!("[dispatch] HA state: {} = {}", h.entity_id, h.state);
                    }
                    Ok(ProtoMessage::SubscribeLogsRequest(l)) => {
                        println!("[dispatch] SubscribeLogs level={} -> sample log frame", l.level);
                        let _ = tx
                            .send(ProtoMessage::SubscribeLogsResponse(SubscribeLogsResponse {
                                level: 3,
                                message: b"[spike] crate spike log line".to_vec(),
                            }))
                            .await;
                    }
                    Ok(other) => {
                        let d = format!("{other:?}");
                        let d: String = d.chars().take(120).collect();
                        println!("[dispatch] unhandled inbound: {d}");
                    }
                    Err(_) => break, // connection closed
                },
                _ = interval.tick() => {
                    let secs = start.elapsed().as_secs();
                    let _ = tx
                        .send(ProtoMessage::SensorStateResponse(SensorStateResponse {
                            key: SENSOR_KEY,
                            state: secs as f32,
                            missing_state: false,
                            device_id: 0,
                        }))
                        .await;
                    println!("[dispatch] pushed spike_uptime = {secs}s");
                },
            }
        }
    };
    dispatch.await;
    match connection.wait().await {
        Ok(()) => println!("[connection] clean shutdown"),
        Err(Error::Disconnected(r)) => println!("[connection] peer left: {r}"),
        Err(e) => println!("[connection] fault: {e}"),
    }
}

/// Question 4: does `start()` work as an initiator (client)?
/// Expected: TIMEOUT — the code peeks the first byte and both sides wait.
async fn probe_client_mode() {
    println!("[probe] client mode: connecting to {KASKADE_HOST} ...");
    let attempt = async {
        let stream = TcpStream::connect(KASKADE_HOST).await?;
        let api = EspHomeApi::builder()
            .name("spike-client".to_string())
            .encryption_key(SPIKE_KEY.to_string())
            .build()?;
        api.start(stream).await
    };
    match timeout(Duration::from_secs(8), attempt).await {
        Err(_) => println!(
            "[probe] client mode: TIMEOUT => start() is responder-only (peeks first byte). \
             Initiator patch needed for Phase 4 (see CRATE-PATCHES.md)."
        ),
        Ok(Err(e)) => println!("[probe] client mode: error: {e}"),
        Ok(Ok(_)) => println!("[probe] client mode: CONNECTED — responder-only hypothesis WRONG"),
    }
}

/// Question 5: what goes on the wire when a plaintext client hits a
/// key-configured server?
async fn probe_plaintext_rejection() {
    let api = match server_api() {
        Ok(api) => api,
        Err(e) => {
            println!("[probe] plaintext: build failed: {e}");
            return;
        }
    };
    let listener = match TcpListener::bind(PLAINTEXT_PROBE_ADDR).await {
        Ok(l) => l,
        Err(e) => {
            println!("[probe] plaintext: bind failed: {e}");
            return;
        }
    };
    tokio::spawn(async move {
        loop {
            let (stream, _) = match listener.accept().await {
                Ok(x) => x,
                Err(_) => return,
            };
            let api = api.clone();
            tokio::spawn(async move {
                match api.start(stream).await {
                    Ok(_) => println!("[probe] plaintext: connection accepted (unexpected)"),
                    Err(e) => println!("[probe] plaintext: server-side error: {e}"),
                }
            });
        }
    });
    tokio::spawn(async {
        tokio::time::sleep(Duration::from_millis(300)).await;
        let mut s = match TcpStream::connect(PLAINTEXT_PROBE_ADDR).await {
            Ok(s) => s,
            Err(e) => {
                println!("[probe] plaintext: client connect failed: {e}");
                return;
            }
        };
        // Plaintext Hello frame: preamble 0x00, len varint 0, type varint 1
        if let Err(e) = s.write_all(&[0x00, 0x00, 0x01]).await {
            println!("[probe] plaintext: write failed: {e}");
            return;
        }
        let mut buf = [0u8; 64];
        match timeout(Duration::from_secs(3), s.read(&mut buf)).await {
            Ok(Ok(n)) => println!(
                "[probe] plaintext rejection: server reply ({} bytes): {:02X?}",
                n,
                &buf[..n]
            ),
            Ok(Err(e)) => println!("[probe] plaintext: read failed: {e}"),
            Err(_) => println!("[probe] plaintext: no reply within 3s (unexpected)"),
        }
    });
}

#[tokio::main]
async fn main() -> Result<(), Error> {
    println!("esphome-native-api spike — key: {SPIKE_KEY}");
    probe_client_mode().await;
    probe_plaintext_rejection().await;

    let api = server_api()?;
    let listener = TcpListener::bind(LISTEN_ADDR)
        .await
        .map_err(|e| Error::Io(e))?;
    println!("[server] listening on {LISTEN_ADDR} (Ctrl+C to stop)");

    loop {
        tokio::select! {
            accepted = listener.accept() => match accepted {
                Ok((stream, peer)) => {
                    println!("[server] connection from {peer}");
                    let api = api.clone();
                    tokio::spawn(async move {
                        match api.start(stream).await {
                            Ok(connection) => handle_connection(connection).await,
                            Err(e) => println!("[server] handshake with {peer} failed: {e}"),
                        }
                    });
                }
                Err(e) => {
                    println!("[server] accept error: {e}");
                    break;
                }
            },
            _ = tokio::signal::ctrl_c() => {
                println!("[server] shutting down");
                break;
            }
        }
    }
    Ok(())
}
