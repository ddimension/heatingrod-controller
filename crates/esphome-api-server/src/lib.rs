//! ESPHome Native API server domain layer.
//!
//! Mirror of the Python `aioesphomeserver` package: entity registry, state
//! cache, per-connection command dispatch — built on the low-level
//! `EspHomeApi` of `esphome-native-api` (handshake, framing, auto-replies).
//!
//! The high-level `EspHomeServer` abstraction of that crate is intentionally
//! NOT used (see plan/): it does not wire encryption, only supports
//! binary sensors, and uses incrementing entity keys instead of our stable
//! MD5-based keys.
//!
//! Identity rules (from CLAUDE.md, must never regress):
//! - entity key = first 8 hex chars of MD5(object_id) as u32 (stable across
//!   restarts, keeps HA entity registry identity)
//! - mac_address must be UPPERCASE in device_info
//! - device_id must be set in BOTH list and state responses

pub mod entities;
pub mod ha_state;
pub mod keys;
pub mod registry;
pub mod server;

/// Re-export of the vendored crate's wire types (version-pinned protobuf
/// structs) — consumers should only touch these in protocol-level code.
pub mod proto_types {
    pub use esphome_native_api::proto::*;
}

pub use entities::{
    BinarySensorEntity, ClimateEntity, NumberEntity, SensorEntity, TextSensorEntity,
};
pub use registry::{Registry, SensorEntry, SensorValue};
pub use server::{ApiServer, Command, HaStateEvent, ServerConfig};
