//! Logging layers: ESPHome log subscriber bridge + native journald layer.
//!
//! `LogForwardLayer` mirrors Python `_ESPHomeLogHandler` (server.py):
//! forwards events of the `heatingrod` module tree to ESPHome log
//! subscribers with the level map CRITICAL/ERROR→1, WARNING→2, INFO→3,
//! DEBUG→5, format `[module] message`.
//!
//! `JournaldLayer` speaks the journald **native protocol** directly (one
//! datagram of newline-separated `KEY=value` fields to
//! `/run/systemd/journal/socket`) instead of linking libsystemd. That link
//! is impossible for static musl binaries, so this layer is what lets the
//! musl build keep native journald fields (PRIORITY, SYSLOG_IDENTIFIER,
//! TARGET — same field set as the former tracing-journald path). When the
//! socket does not exist (e.g. OpenWrt), construction fails and the caller
//! falls back to stderr like before.

use std::fmt;
use std::fmt::Write as _;
use std::os::unix::net::UnixDatagram;
use std::sync::Arc;

use esphome_api_server::ApiServer;
use tracing::{Event, Level, Subscriber};
use tracing_subscriber::layer::{Context, Layer};

/// Native journald layer (no libsystemd). Sends `MESSAGE`, `PRIORITY`,
/// `SYSLOG_IDENTIFIER`, `TARGET`, `CODE_FILE`, `CODE_LINE` per event.
pub struct JournaldLayer {
    ident: String,
    socket_path: String,
    socket: UnixDatagram,
}

impl JournaldLayer {
    /// Journald default socket; fails (→ stderr fallback) when absent.
    pub fn new(ident: &str) -> std::io::Result<Self> {
        Self::at("/run/systemd/journal/socket", ident)
    }

    /// Explicit socket path (tests use a temp socket).
    pub fn at(socket_path: &str, ident: &str) -> std::io::Result<Self> {
        if !std::path::Path::new(socket_path).exists() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                format!("journal socket {socket_path} not found"),
            ));
        }
        Ok(Self {
            ident: ident.to_string(),
            socket_path: socket_path.to_string(),
            socket: UnixDatagram::unbound()?,
        })
    }

    /// Python/tracing-journald mapping: DEBUG=7, INFO=6, WARNING=4,
    /// ERROR=3 (TRACE rides along with DEBUG).
    fn map_level(level: &Level) -> u8 {
        match *level {
            Level::ERROR => 3,
            Level::WARN => 4,
            Level::INFO => 6,
            Level::DEBUG | Level::TRACE => 7,
        }
    }

    /// Journald value encoding: a literal newline inside a field must be
    /// sent as `\n` (two chars), otherwise the receiver splits the field.
    fn escape(value: &str) -> String {
        value.replace('\n', "\\n").replace('\0', "\\0")
    }
}

impl<S: Subscriber> Layer<S> for JournaldLayer {
    fn on_event(&self, event: &Event<'_>, _ctx: Context<'_, S>) {
        let meta = event.metadata();
        let mut visitor = FieldVisitor::default();
        event.record(&mut visitor);

        let mut fields = format!(
            "MESSAGE={}\nPRIORITY={}\nSYSLOG_IDENTIFIER={}\nTARGET={}\n",
            Self::escape(&visitor.message()),
            Self::map_level(meta.level()),
            self.ident,
            meta.target(),
        );
        if let Some(file) = meta.file() {
            let _ = writeln!(fields, "CODE_FILE={file}");
        }
        if let Some(line) = meta.line() {
            let _ = writeln!(fields, "CODE_LINE={line}");
        }
        // Logging must never panic the daemon — drop on send errors
        // (journald restarting, socket buffer full).
        let _ = self.socket.send_to(fields.as_bytes(), &self.socket_path);
    }
}

pub struct LogForwardLayer {
    server: Arc<ApiServer>,
}

impl LogForwardLayer {
    pub fn new(server: Arc<ApiServer>) -> Self {
        Self { server }
    }

    /// ESPHome level map (lower = more severe), Python `_LOG_LEVEL_MAP`.
    fn map_level(level: &Level) -> i32 {
        match *level {
            Level::ERROR => 1,
            Level::WARN => 2,
            Level::INFO => 3,
            Level::DEBUG | Level::TRACE => 5,
        }
    }
}

impl<S: Subscriber> Layer<S> for LogForwardLayer {
    fn on_event(&self, event: &Event<'_>, _ctx: Context<'_, S>) {
        let meta = event.metadata();
        // Python `_ESPHomeLogHandler` sits on the `heatingrod` logger only —
        // third-party crates are NOT forwarded (CLAUDE.md: FIRST_PARTY
        // principle, otherwise the journal/HA floods).
        let module_path = meta.module_path().unwrap_or("heatingrod");
        if module_path != "heatingrod" && !module_path.starts_with("heatingrod::") {
            return;
        }
        // Only INFO and above (Python's upstream logger level filter).
        if *meta.level() > Level::INFO {
            return;
        }
        let level = Self::map_level(meta.level());
        let module = module_path.strip_prefix("heatingrod::").unwrap_or("heatingrod");
        let mut visitor = FieldVisitor::default();
        event.record(&mut visitor);
        self.server.send_log(level, &format!("[{module}] {}", visitor.message()));
    }
}

/// Collects the `message` field (or all fields as `k=v`) from an event.
#[derive(Default)]
struct FieldVisitor {
    message: Option<String>,
    fields: Vec<String>,
}

impl FieldVisitor {
    fn message(&self) -> String {
        match (&self.message, self.fields.is_empty()) {
            (Some(m), false) => format!("{m} {}", self.fields.join(" ")),
            (Some(m), true) => m.clone(),
            (None, _) => self.fields.join(" "),
        }
    }
}

impl tracing::field::Visit for FieldVisitor {
    fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn fmt::Debug) {
        if field.name() == "message" {
            self.message = Some(format!("{value:?}"));
        } else {
            self.fields.push(format!("{}={value:?}", field.name()));
        }
    }

    fn record_str(&mut self, field: &tracing::field::Field, value: &str) {
        if field.name() == "message" {
            self.message = Some(value.to_string());
        } else {
            self.fields.push(format!("{}={value}", field.name()));
        }
    }

    fn record_bool(&mut self, field: &tracing::field::Field, value: bool) {
        self.fields.push(format!("{}={value}", field.name()));
    }

    fn record_i64(&mut self, field: &tracing::field::Field, value: i64) {
        self.fields.push(format!("{}={value}", field.name()));
    }

    fn record_u64(&mut self, field: &tracing::field::Field, value: u64) {
        self.fields.push(format!("{}={value}", field.name()));
    }

    fn record_f64(&mut self, field: &tracing::field::Field, value: f64) {
        self.fields.push(format!("{}={value}", field.name()));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tracing_subscriber::layer::SubscriberExt;
    use tracing_subscriber::registry;

    /// Temp-dir unix socket bound as journald receiver; the layer sends to
    /// it via its explicit-path constructor.
    struct TestSocket {
        dir: std::path::PathBuf,
        path: String,
        receiver: UnixDatagram,
    }

    impl TestSocket {
        fn new(tag: &str) -> Self {
            let dir = std::env::temp_dir().join(format!(
                "heatingrod-journal-{}-{tag}",
                std::process::id()
            ));
            std::fs::create_dir_all(&dir).unwrap();
            let path = dir.join("journal.sock");
            let receiver = UnixDatagram::bind(&path).unwrap();
            Self {
                dir,
                path: path.to_string_lossy().into_owned(),
                receiver,
            }
        }

        fn recv(&self) -> String {
            let mut buf = [0u8; 4096];
            let n = self.receiver.recv(&mut buf).unwrap();
            String::from_utf8_lossy(&buf[..n]).into_owned()
        }
    }

    impl Drop for TestSocket {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }

    fn emit(dispatch: &tracing::Dispatch) {
        tracing::dispatcher::with_default(dispatch, || {
            tracing::info!(target: "heatingrod::onewire", "sensor {}: {:.1}°C", 42, 23.4);
        });
    }

    #[test]
    fn emits_priority_identifier_target_and_message() {
        let sock = TestSocket::new("fields");
        let layer = JournaldLayer::at(&sock.path, "heatingrod").unwrap();
        let dispatch = tracing::Dispatch::new(registry().with(layer));
        emit(&dispatch);
        let datagram = sock.recv();

        assert!(datagram.starts_with("MESSAGE=sensor 42: 23.4°C\n"), "{datagram}");
        assert!(datagram.contains("\nPRIORITY=6\n"), "{datagram}");
        assert!(datagram.contains("\nSYSLOG_IDENTIFIER=heatingrod\n"), "{datagram}");
        assert!(datagram.contains("\nTARGET=heatingrod::onewire\n"), "{datagram}");
        // CODE_FILE/CODE_LINE point into this test file.
        assert!(datagram.contains("CODE_FILE=crates/heatingrod/src/logging.rs\n"), "{datagram}");
        assert!(datagram.contains("\nCODE_LINE="), "{datagram}");
    }

    #[test]
    fn warning_maps_to_priority_4() {
        let sock = TestSocket::new("warn");
        let layer = JournaldLayer::at(&sock.path, "heatingrod").unwrap();
        let dispatch = tracing::Dispatch::new(registry().with(layer));
        tracing::dispatcher::with_default(&dispatch, || {
            tracing::warn!(target: "heatingrod", "careful");
        });
        let datagram = sock.recv();
        assert!(datagram.contains("\nPRIORITY=4\n"), "{datagram}");
    }

    #[test]
    fn newline_in_message_is_escaped() {
        let sock = TestSocket::new("escape");
        let layer = JournaldLayer::at(&sock.path, "heatingrod").unwrap();
        let dispatch = tracing::Dispatch::new(registry().with(layer));
        tracing::dispatcher::with_default(&dispatch, || {
            tracing::error!(target: "heatingrod", "multi\nline");
        });
        let datagram = sock.recv();
        assert!(datagram.contains("MESSAGE=multi\\nline\n"), "{datagram}");
        assert!(datagram.contains("\nPRIORITY=3\n"), "{datagram}");
    }

    #[test]
    fn missing_socket_returns_err_for_fallback() {
        let path = std::env::temp_dir().join("heatingrod-journal-nonexistent.sock");
        assert!(JournaldLayer::at(&path.to_string_lossy(), "heatingrod").is_err());
    }
}
