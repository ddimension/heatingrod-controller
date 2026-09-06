//! Wire-level protocol regression tests — port of
//! `aioesphomeserver/tests/test_noise.py` semantics.
//!
//! A hand-written ESPHome API CLIENT (initiator side, noise-protocol crates
//! directly, no esphome-native-api) verifies the exact wire behavior of our
//! server stack. These pin the frame sequence that took the Python side days
//! to get right:
//!
//!   client -> ClientHello   [0x01][len=0]
//!   client -> handshake     [0x01][len][0x00 | noise msg 1]   (one write)
//!   server -> ServerHello   [0x01][len][0x01 | name \0 mac \0]
//!   server -> handshake     [0x01][len][0x00 | noise msg 2]
//!
//! Regressions pinned: MAC uppercase, wrong-PSK error frame, encrypted
//! initial burst, per-connection encryption of state pushes (the ~70
//! reconnects/min bug), msg124 declined, plaintext rejected.

use std::sync::atomic::{AtomicU16, Ordering};
use std::sync::Arc;
use std::time::Duration;

use base64::Engine;
use noise_protocol::patterns::noise_nn_psk0;
use noise_protocol::{CipherState, HandshakeState};
use noise_rust_crypto::{ChaCha20Poly1305, Sha256, X25519};
use prost::Message;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::sync::mpsc;

use esphome_api_server::proto_types::{
    DeviceInfoResponse, HelloResponse, ListEntitiesDoneResponse, NoiseEncryptionSetKeyResponse,
    SensorStateResponse,
};
use esphome_api_server::{
    keys::entity_key, ApiServer, SensorEntity, SensorEntry, SensorValue, ServerConfig,
};

/// base64 of bytes 0..32.
const PSK_B64: &str = "AAECAwQFBgcICQoLDA0ODxAREhMUFRYXGBkaGxwdHh8=";
static NEXT_PORT: AtomicU16 = AtomicU16::new(16054);

fn psk_bytes() -> Vec<u8> {
    base64::engine::general_purpose::STANDARD
        .decode(PSK_B64)
        .expect("psk decodes")
}

fn test_config(port: u16) -> ServerConfig {
    ServerConfig {
        name: "heatingrod-test".into(),
        friendly_name: "Heizstab Test".into(),
        // Lowercase on purpose — must be normalized to uppercase on the wire.
        mac_address: "b8:27:eb:96:ae:01".into(),
        project_name: "custom.heatingrod".into(),
        project_version: "2.0.0".into(),
        bind_address: "127.0.0.1".into(),
        port,
        noise_psk: PSK_B64.into(),
        devices: vec![(1, "DS100 Energiezähler".into())],
        ..Default::default()
    }
}

/// Spawn a server with one numeric sensor `test_sensor` and return it.
async fn spawn_server() -> (Arc<ApiServer>, u16) {
    let port = NEXT_PORT.fetch_add(1, Ordering::SeqCst) + (std::process::id() as u16 % 100);
    let (tx, _rx) = mpsc::channel(16);
    let (htx, _hrx) = mpsc::channel(16);
    let server = Arc::new(ApiServer::new(test_config(port), tx, htx));
    let mut sensor = SensorEntity::new("Test Sensor");
    sensor.object_id = "test_sensor".into();
    sensor.key = entity_key("test_sensor");
    sensor.unit_of_measurement = "W".into();
    server.add_sensor(SensorEntry::Numeric(sensor));
    let s = server.clone();
    tokio::spawn(async move {
        let _ = s.run().await;
    });
    // Wait for the listener: probe connects are dropped (empty peek).
    for _ in 0..100 {
        if TcpStream::connect(("127.0.0.1", port)).await.is_ok() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(30)).await;
    }
    (server, port)
}

async fn read_frame(stream: &mut TcpStream) -> Result<Vec<u8>, String> {
    let mut header = [0u8; 3];
    stream
        .read_exact(&mut header)
        .await
        .map_err(|e| e.to_string())?;
    if header[0] != 0x01 {
        return Err(format!("expected 0x01 frame, got 0x{:02X}", header[0]));
    }
    let len = u16::from_be_bytes([header[1], header[2]]) as usize;
    let mut body = vec![0u8; len];
    stream
        .read_exact(&mut body)
        .await
        .map_err(|e| e.to_string())?;
    Ok(body)
}

/// Hand-written initiator client (no esphome-native-api).
struct WireClient {
    stream: TcpStream,
    encrypt: CipherState<ChaCha20Poly1305>,
    decrypt: CipherState<ChaCha20Poly1305>,
}

impl WireClient {
    async fn connect(port: u16, psk: &[u8]) -> Result<Self, String> {
        let mut stream = TcpStream::connect(("127.0.0.1", port))
            .await
            .map_err(|e| e.to_string())?;

        let mut hs: HandshakeState<X25519, ChaCha20Poly1305, Sha256> =
            HandshakeState::new(noise_nn_psk0(), true, b"NoiseAPIInit\0\0", None, None, None, None);
        hs.push_psk(psk);
        let msg1 = hs.write_message_vec(b"").map_err(|e| e.to_string())?;

        // ClientHello + handshake message in ONE write (like aioesphomeapi).
        // Wire format per frame: [0x01][len:u16][payload].
        let mut write_buf = vec![0x01, 0x00, 0x00]; // ClientHello: preamble 1, len 0
        let mut handshake_frame = vec![0x00]; // payload preamble 0x00 = ok
        handshake_frame.extend_from_slice(&msg1);
        write_buf.push(0x01); // second frame preamble
        write_buf.extend_from_slice(&(handshake_frame.len() as u16).to_be_bytes());
        write_buf.extend_from_slice(&handshake_frame);
        stream
            .write_all(&write_buf)
            .await
            .map_err(|e| e.to_string())?;

        // ServerHello: payload[0]=0x01, then name\0 mac\0
        let server_hello = read_frame(&mut stream).await?;
        if server_hello.first() != Some(&0x01) {
            return Err(format!("bad ServerHello preamble: {:02X?}", server_hello.first()));
        }
        let rest = &server_hello[1..];
        let (name, mac) = split_nul_nul(rest).ok_or("ServerHello not name\\0mac\\0")?;
        assert_eq!(name, "heatingrod-test");
        assert_eq!(mac, "B8:27:EB:96:AE:01", "MAC must be UPPERCASE on the wire");

        // Handshake frame 2: payload[0] must be 0x00 (ok preamble)
        let frame2 = read_frame(&mut stream).await?;
        if frame2.first() != Some(&0x00) {
            return Err(format!(
                "handshake failed: {:?}",
                String::from_utf8_lossy(&frame2[..frame2.len().min(64)])
            ));
        }
        hs.read_message_vec(&frame2[1..]).map_err(|e| e.to_string())?;
        // Noise spec Split(): the first cipher is for ENCRYPTING when the
        // local role is initiator (the responder server uses it as decrypt —
        // see the vendored crate's `get_ciphers()` call).
        let (encrypt, decrypt) = hs.get_ciphers();
        Ok(Self {
            stream,
            encrypt,
            decrypt,
        })
    }

    /// Send one encrypted API message: plaintext framing [type:u16][len:u16][payload].
    async fn send(&mut self, msg_type: u16, payload: &[u8]) -> Result<(), String> {
        let mut plain = Vec::new();
        plain.extend_from_slice(&msg_type.to_be_bytes());
        plain.extend_from_slice(&(payload.len() as u16).to_be_bytes());
        plain.extend_from_slice(payload);
        let ct = self.encrypt.encrypt_vec(&plain);
        let mut out = vec![0x01];
        out.extend_from_slice(&(ct.len() as u16).to_be_bytes());
        out.extend_from_slice(&ct);
        self.stream
            .write_all(&out)
            .await
            .map_err(|e| e.to_string())
    }

    /// Receive one API message; returns (msg_type, payload).
    async fn recv(&mut self) -> Result<(u16, Vec<u8>), String> {
        let ct = read_frame(&mut self.stream).await?;
        let plain = self
            .decrypt
            .decrypt_vec(&ct)
            .map_err(|_| "AEAD decrypt failed".to_string())?;
        if plain.len() < 4 {
            return Err(format!("frame too short: {}", plain.len()));
        }
        let msg_type = u16::from_be_bytes([plain[0], plain[1]]);
        Ok((msg_type, plain[4..].to_vec()))
    }
}

fn split_nul_nul(buf: &[u8]) -> Option<(&str, &str)> {
    let first = buf.iter().position(|&b| b == 0)?;
    let rest = &buf[first + 1..];
    let second = rest.iter().position(|&b| b == 0)?;
    Some((
        std::str::from_utf8(&buf[..first]).ok()?,
        std::str::from_utf8(&rest[..second]).ok()?,
    ))
}

// ---- tests ----

#[tokio::test]
async fn handshake_and_device_info() {
    let (_server, port) = spawn_server().await;
    let mut c = WireClient::connect(port, &psk_bytes()).await.expect("connect");

    // HelloRequest (1) → HelloResponse (2), api 1/14
    let mut empty = Vec::new();
    prost_types_helper::hello(&mut empty);
    c.send(1, &empty).await.unwrap();
    let (t, payload) = c.recv().await.unwrap();
    assert_eq!(t, 2);
    let hello = HelloResponse::decode(payload.as_slice()).unwrap();
    assert_eq!(hello.api_version_major, 1);
    assert_eq!(hello.api_version_minor, 14);
    assert_eq!(hello.name, "heatingrod-test");
    assert_eq!(hello.server_info, "custom.heatingrod 2.0.0");

    // DeviceInfoRequest (9) → DeviceInfoResponse (10)
    c.send(9, &[]).await.unwrap();
    let (t, payload) = c.recv().await.unwrap();
    assert_eq!(t, 10);
    let info = DeviceInfoResponse::decode(payload.as_slice()).unwrap();
    assert_eq!(info.mac_address, "B8:27:EB:96:AE:01");
    assert_eq!(info.project_name, "custom.heatingrod");
    assert_eq!(info.project_version, "2.0.0");
    assert_eq!(info.api_encryption_supported, true);
    assert_eq!(info.devices.len(), 1);
    assert_eq!(info.devices[0].device_id, 1);
    assert_eq!(info.devices[0].name, "DS100 Energiezähler");
}

#[tokio::test]
async fn list_entities_stream_and_done() {
    let (_server, port) = spawn_server().await;
    let mut c = WireClient::connect(port, &psk_bytes()).await.unwrap();

    c.send(11, &[]).await.unwrap(); // ListEntitiesRequest
    let (t, payload) = c.recv().await.unwrap();
    assert_eq!(t, 16); // ListEntitiesSensorResponse
    let info = esphome_api_server::proto_types::ListEntitiesSensorResponse::decode(
        payload.as_slice(),
    )
    .unwrap();
    assert_eq!(info.object_id, "test_sensor");
    assert_eq!(info.key, entity_key("test_sensor"));
    assert_eq!(info.unit_of_measurement, "W");
    let (t, _) = c.recv().await.unwrap();
    assert_eq!(t, 19); // ListEntitiesDoneResponse
    let _ = ListEntitiesDoneResponse::default();
}

#[tokio::test]
async fn subscribe_states_initial_burst_is_encrypted() {
    let (_server, port) = spawn_server().await;
    let mut c = WireClient::connect(port, &psk_bytes()).await.unwrap();

    c.send(20, &[]).await.unwrap(); // SubscribeStatesRequest
    // The burst arrives as ENCRYPTED frames — `recv` decrypts, so any
    // plaintext leak would fail the AEAD and error here.
    let (t, payload) = c.recv().await.unwrap();
    assert_eq!(t, 25); // SensorStateResponse
    let state = SensorStateResponse::decode(payload.as_slice()).unwrap();
    assert_eq!(state.key, entity_key("test_sensor"));
    assert!(state.missing_state);
}

#[tokio::test]
async fn state_push_reaches_noise_client_per_connection_encrypted() {
    // Regression: pushes built as plaintext frames must be encrypted per
    // connection — the bug caused ~70 reconnects/min in HA.
    let (server, port) = spawn_server().await;
    let mut c = WireClient::connect(port, &psk_bytes()).await.unwrap();

    c.send(20, &[]).await.unwrap();
    let _ = c.recv().await.unwrap(); // initial burst

    server.update_sensor(entity_key("test_sensor"), SensorValue::Number(42.5));
    let (t, payload) = c.recv().await.unwrap();
    assert_eq!(t, 25);
    let state = SensorStateResponse::decode(payload.as_slice()).unwrap();
    assert_eq!(state.state, 42.5);
    assert!(!state.missing_state);
}

#[tokio::test]
async fn wrong_psk_gets_mac_failure_frame() {
    let (_server, port) = spawn_server().await;
    let mut stream = TcpStream::connect(("127.0.0.1", port))
        .await
        .expect("connect");

    let wrong = [7u8; 32];
    let mut hs: HandshakeState<X25519, ChaCha20Poly1305, Sha256> =
        HandshakeState::new(noise_nn_psk0(), true, b"NoiseAPIInit\0\0", None, None, None, None);
    hs.push_psk(&wrong);
    let msg1 = hs.write_message_vec(b"").unwrap();
    let mut write_buf = vec![0x01, 0x00, 0x00]; // ClientHello
    let mut handshake_frame = vec![0x00];
    handshake_frame.extend_from_slice(&msg1);
    write_buf.push(0x01); // second frame preamble
    write_buf.extend_from_slice(&(handshake_frame.len() as u16).to_be_bytes());
    write_buf.extend_from_slice(&handshake_frame);
    stream.write_all(&write_buf).await.unwrap();

    let server_hello = read_frame(&mut stream).await.unwrap();
    assert_eq!(server_hello.first(), Some(&0x01)); // ServerHello arrives first

    let frame2 = read_frame(&mut stream).await.unwrap();
    assert_ne!(frame2.first(), Some(&0x00), "error preamble expected");
    assert!(
        String::from_utf8_lossy(&frame2).contains("Handshake MAC failure"),
        "got: {:?}",
        String::from_utf8_lossy(&frame2[..frame2.len().min(64)])
    );
}

#[tokio::test]
async fn plaintext_rejected_when_psk_configured() {
    let (_server, port) = spawn_server().await;
    let mut stream = TcpStream::connect(("127.0.0.1", port))
        .await
        .expect("connect");
    // Plaintext Hello frame: preamble 0x00, len varint 0, type varint 1
    stream.write_all(&[0x00, 0x00, 0x01]).await.unwrap();
    let mut buf = [0u8; 64];
    let n = tokio::time::timeout(Duration::from_secs(3), stream.read(&mut buf))
        .await
        .expect("reply within 3s")
        .expect("read");
    assert_eq!(buf[0], 0x01, "rejection must be a 0x01 error frame");
    assert_ne!(buf[3], 0x00, "payload must carry a non-zero error preamble");
}

#[tokio::test]
async fn noise_key_provisioning_declined() {
    let (_server, port) = spawn_server().await;
    let mut c = WireClient::connect(port, &psk_bytes()).await.unwrap();

    // msg 124 NoiseEncryptionSetKeyRequest
    let req = esphome_api_server::proto_types::NoiseEncryptionSetKeyRequest {
        key: vec![9u8; 32],
    };
    let mut buf = Vec::new();
    req.encode(&mut buf).unwrap();
    c.send(124, &buf).await.unwrap();

    let (t, payload) = c.recv().await.unwrap();
    assert_eq!(t, 125);
    let resp = NoiseEncryptionSetKeyResponse::decode(payload.as_slice()).unwrap();
    assert!(!resp.success, "key is config-managed, must decline");
}

/// Tiny helper to avoid depending on prost in test call sites.
mod prost_types_helper {
    use prost::Message;

    pub fn hello(buf: &mut Vec<u8>) {
        esphome_api_server::proto_types::HelloRequest {
            api_version_major: 1,
            api_version_minor: 14,
            client_info: "wire-client".into(),
        }
        .encode(buf)
        .unwrap();
    }
}
