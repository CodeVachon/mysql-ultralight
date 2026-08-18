//! Connection tracing.
//!
//! Observing exactly what a client sends is the whole point of the viability
//! spike, and it stays useful afterwards as the query-log feature tests use to
//! assert on query behaviour.

use std::time::Duration;

use mysql_ultralight_protocol::{Capabilities, Command, ErrPacket};

/// Something that happened on a connection.
#[derive(Debug)]
pub enum Event<'a> {
    Connected {
        connection_id: u32,
        peer: &'a str,
    },
    /// Emitted once the client's handshake response has been parsed. The
    /// capability list and connection attributes identify the driver.
    Authenticated {
        connection_id: u32,
        username: &'a str,
        database: Option<&'a str>,
        auth_plugin: Option<&'a str>,
        capabilities: Capabilities,
        attributes: &'a [(String, String)],
    },
    Command {
        connection_id: u32,
        command: Command,
        detail: &'a str,
    },
    Completed {
        connection_id: u32,
        summary: &'a str,
        elapsed: Duration,
    },
    Failed {
        connection_id: u32,
        error: &'a ErrPacket,
    },
    Disconnected {
        connection_id: u32,
    },
}

/// A sink for connection events.
pub trait Trace: Send + Sync {
    fn event(&self, event: Event<'_>);
}

/// Discards everything. The default, so tracing costs nothing when unused.
#[derive(Debug, Clone, Copy, Default)]
pub struct NoTrace;

impl Trace for NoTrace {
    fn event(&self, _event: Event<'_>) {}
}

/// Writes a human-readable trace to stderr.
#[derive(Debug, Clone, Copy, Default)]
pub struct StderrTrace;

impl Trace for StderrTrace {
    fn event(&self, event: Event<'_>) {
        match event {
            Event::Connected {
                connection_id,
                peer,
            } => {
                eprintln!("[{connection_id}] connected from {peer}");
            }
            Event::Authenticated {
                connection_id,
                username,
                database,
                auth_plugin,
                capabilities,
                attributes,
            } => {
                eprintln!(
                    "[{connection_id}] authenticated user={username} db={} plugin={}",
                    database.unwrap_or("-"),
                    auth_plugin.unwrap_or("-")
                );
                eprintln!(
                    "[{connection_id}]   capabilities: {}",
                    capabilities.names().join(" ")
                );
                if !attributes.is_empty() {
                    let rendered: Vec<String> =
                        attributes.iter().map(|(k, v)| format!("{k}={v}")).collect();
                    eprintln!("[{connection_id}]   attributes: {}", rendered.join(" "));
                }
            }
            Event::Command {
                connection_id,
                command,
                detail,
            } => {
                if detail.is_empty() {
                    eprintln!("[{connection_id}] {}", command.name());
                } else {
                    eprintln!(
                        "[{connection_id}] {} {}",
                        command.name(),
                        truncate(detail, 2000)
                    );
                }
            }
            Event::Completed {
                connection_id,
                summary,
                elapsed,
            } => {
                eprintln!(
                    "[{connection_id}]   -> {summary} ({:.3}ms)",
                    elapsed.as_secs_f64() * 1000.0
                );
            }
            Event::Failed {
                connection_id,
                error,
            } => {
                eprintln!("[{connection_id}]   -> {error}");
            }
            Event::Disconnected { connection_id } => {
                eprintln!("[{connection_id}] disconnected");
            }
        }
    }
}

/// Shortens a string for logging, marking that it was cut.
fn truncate(s: &str, max: usize) -> std::borrow::Cow<'_, str> {
    if s.len() <= max {
        return std::borrow::Cow::Borrowed(s);
    }
    let mut end = max;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    std::borrow::Cow::Owned(format!("{}… ({} bytes total)", &s[..end], s.len()))
}
