//! The connection handshake: the server's greeting and the client's response.

use crate::auth::{self, MYSQL_NATIVE_PASSWORD};
use crate::buf::{BufReader, BufWriter};
use crate::capabilities::{status, Capabilities};
use crate::column::charset;

/// The server's opening greeting (`HandshakeV10`).
#[derive(Debug, Clone)]
pub struct Handshake {
    /// Reported to the client as the server version. Clients gate features on
    /// this string, so it must look like a real MySQL 8 version.
    pub server_version: String,
    pub connection_id: u32,
    pub nonce: [u8; auth::NONCE_LEN],
    pub capabilities: Capabilities,
    pub charset: u8,
    pub status_flags: u16,
    pub auth_plugin: String,
}

impl Handshake {
    pub fn new(server_version: impl Into<String>, connection_id: u32) -> Self {
        Self {
            server_version: server_version.into(),
            connection_id,
            nonce: auth::generate_nonce(connection_id),
            capabilities: Capabilities::server_default(),
            // utf8mb4_general_ci: a one-byte id, unlike the MySQL 8 default
            // collation whose id does not fit in this field.
            charset: charset::UTF8MB4_GENERAL_CI as u8,
            status_flags: status::AUTOCOMMIT,
            auth_plugin: MYSQL_NATIVE_PASSWORD.to_owned(),
        }
    }

    pub fn encode(&self) -> Vec<u8> {
        let mut w = BufWriter::with_capacity(128);

        w.u8(10)
            .null_str(&self.server_version)
            .u32(self.connection_id)
            // The nonce is split: 8 bytes here, the remainder further down.
            .bytes(&self.nonce[..8])
            .u8(0)
            .u16(self.capabilities.lower())
            .u8(self.charset)
            .u16(self.status_flags)
            .u16(self.capabilities.upper());

        if self.capabilities.contains(Capabilities::PLUGIN_AUTH) {
            // Includes the NUL terminator the client expects.
            w.u8(self.nonce.len() as u8 + 1);
        } else {
            w.u8(0);
        }

        w.zeros(10);

        // The second half is padded to at least 13 bytes, the last being NUL.
        let rest = &self.nonce[8..];
        w.bytes(rest).zeros(13usize.saturating_sub(rest.len()));

        if self.capabilities.contains(Capabilities::PLUGIN_AUTH) {
            w.null_str(&self.auth_plugin);
        }

        w.into_inner()
    }
}

/// The client's reply to the greeting (`HandshakeResponse41`).
#[derive(Debug, Clone, Default)]
pub struct HandshakeResponse {
    pub capabilities: Capabilities,
    pub max_packet_size: u32,
    pub charset: u8,
    pub username: String,
    pub auth_response: Vec<u8>,
    pub database: Option<String>,
    pub auth_plugin: Option<String>,
    /// Connection attributes such as `_client_name` and `program_name`. Useful
    /// for identifying which driver connected.
    pub attributes: Vec<(String, String)>,
}

/// Why a handshake response could not be understood.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HandshakeError {
    /// The payload ended before the layout was complete.
    Truncated,
    /// The client did not request `PROTOCOL_41`.
    UnsupportedProtocol,
}

impl std::fmt::Display for HandshakeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            HandshakeError::Truncated => f.write_str("handshake response ended unexpectedly"),
            HandshakeError::UnsupportedProtocol => {
                f.write_str("client did not request PROTOCOL_41; pre-4.1 clients are not supported")
            }
        }
    }
}

impl std::error::Error for HandshakeError {}

impl From<crate::buf::Truncated> for HandshakeError {
    fn from(_: crate::buf::Truncated) -> Self {
        HandshakeError::Truncated
    }
}

impl HandshakeResponse {
    pub fn decode(payload: &[u8]) -> Result<Self, HandshakeError> {
        let mut r = BufReader::new(payload);

        let lower = r.u16()?;
        let upper = r.u16()?;
        let capabilities = Capabilities::from_parts(lower, upper);

        // A pre-4.1 client sends a 2-byte capability and 3-byte max-packet
        // header instead. Parsing that as a HandshakeResponse41 yields a
        // garbage username and auth response, and the session would then run
        // with OK and ERR packets encoded for a protocol the client cannot
        // read. Refuse instead.
        if !capabilities.contains(Capabilities::PROTOCOL_41) {
            return Err(HandshakeError::UnsupportedProtocol);
        }

        let max_packet_size = r.u32()?;
        let charset = r.u8()?;
        r.skip(23)?;

        let username = r.null_str()?;

        let auth_response = if capabilities.contains(Capabilities::PLUGIN_AUTH_LENENC_CLIENT_DATA) {
            r.lenenc_bytes()?.unwrap_or_default().to_vec()
        } else if capabilities.contains(Capabilities::SECURE_CONNECTION) {
            let len = r.u8()? as usize;
            r.take(len)?.to_vec()
        } else {
            r.null_bytes()?.to_vec()
        };

        let database = if capabilities.contains(Capabilities::CONNECT_WITH_DB) {
            // Some clients set the flag but send nothing when there is no
            // default schema, so a truncated read here is not fatal.
            r.null_str().ok().filter(|s| !s.is_empty())
        } else {
            None
        };

        let auth_plugin = if capabilities.contains(Capabilities::PLUGIN_AUTH) {
            r.null_str().ok().filter(|s| !s.is_empty())
        } else {
            None
        };

        let mut attributes = Vec::new();
        if capabilities.contains(Capabilities::CONNECT_ATTRS) {
            if let Ok(Some(total)) = r.lenenc_int() {
                let block = r.take(total as usize).unwrap_or(&[]);
                let mut ar = BufReader::new(block);
                while !ar.is_empty() {
                    let (Ok(Some(k)), Ok(Some(v))) = (ar.lenenc_bytes(), ar.lenenc_bytes()) else {
                        break;
                    };
                    attributes.push((
                        String::from_utf8_lossy(k).into_owned(),
                        String::from_utf8_lossy(v).into_owned(),
                    ));
                }
            }
        }

        Ok(Self {
            capabilities,
            max_packet_size,
            charset,
            username,
            auth_response,
            database,
            auth_plugin,
            attributes,
        })
    }
}

/// Asks the client to re-authenticate with a different plugin. Sent when a
/// client opens with `caching_sha2_password` but we speak native password.
pub fn encode_auth_switch_request(plugin: &str, nonce: &[u8]) -> Vec<u8> {
    let mut w = BufWriter::with_capacity(32);
    w.u8(0xfe).null_str(plugin).bytes(nonce).u8(0);
    w.into_inner()
}
