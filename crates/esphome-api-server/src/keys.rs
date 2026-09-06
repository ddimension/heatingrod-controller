//! Entity keys — identity-critical (see CLAUDE.md, „Entity-Identität").
//!
//! HA maps state pushes to entities by key. The Python server derives keys as
//! `int(md5(object_id)[:8], 16)` — NOT FNV-1 (the esphome-native-api crate's
//! convention). At cutover the Rust server must produce byte-identical keys,
//! otherwise HA sees new keys and creates duplicate entities.

use md5::Digest;

/// First 8 hex chars of MD5(object_id), parsed as u32.
pub fn entity_key(object_id: &str) -> u32 {
    let digest = md5::Md5::digest(object_id.as_bytes());
    let hex = hex::encode(&digest[..4]);
    u32::from_str_radix(&hex, 16).expect("hex u32 parse cannot fail")
}

/// Default object_id derived from a name (Python `__post_init__` semantics):
/// lowercase, spaces→underscores, slashes→underscores.
pub fn default_object_id(name: &str) -> String {
    name.to_lowercase().replace(' ', "_").replace('/', "_")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn key_matches_python_golden_values() {
        // Frozen from aioesphomeserver/entities.py (run on 2026-09-04).
        assert_eq!(entity_key("ds100_power"), 0x6ac4_68da);
        assert_eq!(entity_key("spike_uptime"), 0x6c9a_35b3);
        assert_eq!(entity_key("ctrl_state"), 0xe087_9d33);
        assert_eq!(entity_key("heizkessel_hk1"), 0xf782_29ba);
        assert_eq!(entity_key("sorel_pumpe_r0"), 0x2e2d_04fb);
        assert_eq!(entity_key("1w_28_ff110f721603"), 0x4694_6b97);
        assert_eq!(entity_key("heizstab_temperatur"), 0x6352_ef78);
        assert_eq!(entity_key("ctrl_dac_level"), 0xa6d1_7f2c);
    }

    #[test]
    fn key_is_stable() {
        assert_eq!(entity_key("x"), entity_key("x"));
        assert!(entity_key("x") > 0);
    }

    #[test]
    fn default_object_id_replaces_spaces_and_slashes() {
        assert_eq!(default_object_id("Speicher Oben/A"), "speicher_oben_a");
    }
}
