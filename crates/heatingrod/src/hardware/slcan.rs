//! slcan ASCII transport for the USBtin CAN adapter.
//!
//! Mirrors python-can's slcan interface (which the Python sorel-canbus uses):
//! open sequence `C\r` (close), `S4\r` (250 kbit), `O\r` (open), then
//! `t`-frames for standard 11-bit ids. No kernel slcan module needed.

use std::time::Duration;

/// LAWICEL slcan S-index for a bus bitrate (S0..S8).
/// python-can mapping: 10k=0, 20k=1, 50k=2, 100k=3, 125k=4, 250k=5,
/// 500k=6, 800k=7, 1M=8. NB: S4 is 125k, NOT 250k (first cut sent S4
/// for 250000 and read nothing — bit errors on the bus).
pub fn bitrate_index(bitrate: u32) -> Option<u8> {
    Some(match bitrate {
        10_000 => 0,
        20_000 => 1,
        50_000 => 2,
        100_000 => 3,
        125_000 => 4,
        250_000 => 5,
        500_000 => 6,
        800_000 => 7,
        1_000_000 => 8,
        _ => return None,
    })
}

/// Parse one received frame line: "t" + 3 hex id + 1 hex dlc + dlc*2 hex data.
pub fn parse_frame_line(line: &str) -> Option<(u32, Vec<u8>)> {
    let line = line.trim_end_matches(['\r', '\n']);
    if line.is_empty() {
        return None;
    }
    let (prefix, rest) = line.split_at(1);
    let id_chars: usize = match prefix {
        "t" => 3,
        "T" => 8,
        _ => return None,
    };
    if rest.len() < id_chars + 1 {
        return None;
    }
    let id = u32::from_str_radix(&rest[..id_chars], 16).ok()?;
    let dlc = usize::from_str_radix(&rest[id_chars..id_chars + 1], 16).ok()?;
    let data_hex = &rest[id_chars + 1..];
    if data_hex.len() < dlc * 2 {
        return None;
    }
    let mut data = Vec::with_capacity(dlc);
    for i in 0..dlc {
        data.push(u8::from_str_radix(&data_hex[i * 2..i * 2 + 2], 16).ok()?);
    }
    Some((id, data))
}

/// Resolve the USB device node for a serial port and power-cycle it via
/// sysfs de-authorize/re-authorize (real bus detach → adapter MCU reset).
///
/// The USBtin wedges occasionally: `C\r` gets a NAK and the bus stays
/// silent. `rmmod cdc_acm` does NOT help (host-driver reset only) — this
/// does (verified 04.09.2026). Walks up the sysfs tree from the tty's
/// device until a node with an `authorized` file (the USB device) is found.
pub fn usb_deauthorize(serial_path: &str) -> Result<(), String> {
    // /dev/serial/by-id/... -> ../../ttyACM0
    let real = std::fs::read_link(serial_path)
        .map_err(|e| format!("usb reset: readlink {serial_path}: {e}"))?;
    let tty_name = real
        .file_name()
        .ok_or_else(|| "usb reset: no tty name".to_string())?;
    let tty_dir = std::path::Path::new("/sys/class/tty").join(&tty_name);
    let mut dir = tty_dir.join("device");
    // Resolve symlink chain, then walk up to the node with `authorized`.
    let mut guard = 0;
    loop {
        guard += 1;
        if guard > 16 {
            return Err("usb reset: sysfs walk too deep".into());
        }
        match std::fs::read_link(&dir) {
            Ok(target) => {
                dir = if target.is_absolute() {
                    target
                } else {
                    dir.parent().unwrap_or(std::path::Path::new("/")).join(target)
                };
            }
            Err(_) => break, // not a symlink — inspect this dir
        }
    }
    loop {
        let auth = dir.join("authorized");
        if auth.exists() {
            std::fs::write(&auth, b"0")
                .map_err(|e| format!("usb reset: deauthorize: {e}"))?;
            std::thread::sleep(std::time::Duration::from_secs(2));
            std::fs::write(&auth, b"1")
                .map_err(|e| format!("usb reset: reauthorize: {e}"))?;
            std::thread::sleep(std::time::Duration::from_secs(3));
            return Ok(());
        }
        dir = dir
            .parent()
            .ok_or_else(|| "usb reset: no USB device node found".to_string())?
            .to_path_buf();
    }
}

pub struct SlcanTransport {
    port: Option<Box<dyn serialport::SerialPort>>,
    path: String,
    tty_baudrate: u32,
    /// True when the last open() saw a NAK on an init command — the
    /// USBtin wedge signature (healthy adapters ack C/S/O cleanly).
    init_nak: bool,
}

impl SlcanTransport {
    pub fn new(path: &str, tty_baudrate: u32) -> Self {
        Self {
            port: None,
            path: path.to_string(),
            tty_baudrate,
            init_nak: false,
        }
    }

    /// Wedge signature observed during the last init (see usb_deauthorize).
    pub fn saw_init_nak(&self) -> bool {
        self.init_nak
    }

    pub fn open(&mut self, bitrate: u32) -> Result<(), String> {
        self.close();
        self.init_nak = false;
        let mut port = serialport::new(&self.path, self.tty_baudrate)
            .timeout(Duration::from_secs(1))
            .open()
            .map_err(|e| format!("slcan open {}: {e}", self.path))?;
        // pyserial parity: assert DTR/RTS on open (pyserial does this by
        // default; the Microchip CDC demo firmware can gate TX on line
        // state — python-can depends on it).
        let _ = port.write_data_terminal_ready(true);
        let _ = port.write_request_to_send(true);

        let s_index = bitrate_index(bitrate)
            .ok_or_else(|| format!("slcan: unsupported bitrate {bitrate}"))?;
        // python-can slcan init sequence: send a command, READ its ack
        // ('\r' = OK, '\a' = NAK) before the next — fire-and-forget makes
        // the USBtin drop follow-up commands and the bus stays closed
        // (observed 04.09.2026: pkts=0 despite live traffic).
        use std::io::{Read, Write};
        let s_cmd = format!("S{s_index}\r");
        for cmd in ["C\r", s_cmd.as_str(), "O\r"] {
            port.write_all(cmd.as_bytes())
                .map_err(|e| format!("slcan write {cmd:?}: {e}"))?;
            let mut ack = [0u8; 1];
            match port.read(&mut ack) {
                Ok(1) if ack[0] == 0x07 => {
                    // BEL = NAK — wedge signature on the USBtin (healthy
                    // adapters ack all of C/S/O). Don't abort the init
                    // (python-can doesn't check responses at all), but
                    // remember it so the caller can do the USB reset.
                    self.init_nak = true;
                    tracing::warn!("slcan command {cmd:?} NAK (continuing)");
                }
                Ok(_) => {}   // '\r' ack
                Err(_) => {}  // timeout — keep going like python-can
            }
        }
        // python-can slcan: sleep_after_open=2 — let the adapter settle
        // after 'O' before the first frames are read.
        std::thread::sleep(Duration::from_secs(2));
        self.port = Some(port);
        Ok(())
    }

    pub fn close(&mut self) {
        if let Some(port) = self.port.as_mut() {
            use std::io::Write;
            let _ = port.write_all(b"C\r");
        }
        self.port = None;
    }

    pub fn is_open(&self) -> bool {
        self.port.is_some()
    }

    /// Read one CAN frame (blocking, ~1s timeout).
    pub fn read_frame(&mut self) -> Result<(u32, Vec<u8>), String> {
        use std::io::Read;
        let Some(port) = self.port.as_mut() else {
            return Err("slcan not open".into());
        };
        let mut line = Vec::with_capacity(32);
        let mut byte = [0u8; 1];
        loop {
            match port.read(&mut byte) {
                Ok(0) => return Err("slcan eof".into()), // device gone → disconnect
                Ok(_) => {
                    if byte[0] == b'\r' {
                        if line.is_empty() {
                            continue; // command ack
                        }
                        break;
                    }
                    line.push(byte[0]);
                    if line.len() > 40 {
                        return Err("slcan line overflow".into());
                    }
                }
                Err(e) if e.kind() == std::io::ErrorKind::TimedOut => {
                    return Err("slcan read timeout".into())
                }
                Err(e) => return Err(format!("slcan read: {e}")),
            }
        }
        let text = String::from_utf8_lossy(&line);
        parse_frame_line(&text).ok_or_else(|| format!("slcan unparsed line: {text}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bitrate_index_mapping() {
        assert_eq!(bitrate_index(250_000), Some(5));
        assert_eq!(bitrate_index(125_000), Some(4));
        assert_eq!(bitrate_index(500_000), Some(6));
        assert_eq!(bitrate_index(1_000_000), Some(8));
        assert_eq!(bitrate_index(10_000), Some(0));
        assert_eq!(bitrate_index(33_333), None);
    }

    #[test]
    fn parses_standard_frame() {
        // SCBI-style: 11-bit id 0x800, dlc 5
        let (id, data) = parse_frame_line("t800510203040506\r").unwrap();
        assert_eq!(id, 0x800);
        assert_eq!(data, vec![0x10, 0x20, 0x30, 0x40, 0x50]);
    }

    #[test]
    fn parses_extended_frame() {
        let (id, data) = parse_frame_line("T123456781AB\r").unwrap();
        assert_eq!(id, 0x12345678);
        assert_eq!(data, vec![0xAB]);
    }

    #[test]
    fn rejects_garbage() {
        assert!(parse_frame_line("r8001FF\r").is_none()); // RTR frame
        assert!(parse_frame_line("F00\r").is_none()); // status
        assert!(parse_frame_line("t80").is_none()); // too short
        assert!(parse_frame_line("").is_none());
    }

    #[test]
    fn dlc_hex() {
        // dlc 'A' = 10
        let (_, data) = parse_frame_line("t7FFA00010203040506070809\r").unwrap();
        assert_eq!(data.len(), 10);
    }
}
