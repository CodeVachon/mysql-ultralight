//! Packet framing: the 4-byte header, sequence numbering, and the generic
//! OK / ERR / EOF packets every command can return.

use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

use crate::buf::BufWriter;
use crate::capabilities::{status, Capabilities};

/// The largest payload that fits in one packet. A payload of exactly this size
/// must be followed by another packet (possibly empty) so the reader knows the
/// logical message continued.
pub const MAX_PAYLOAD: usize = 0xff_ffff;

/// Guards against a malicious or broken client asking us to buffer unbounded
/// memory across a run of maximum-size packets.
pub const DEFAULT_MAX_MESSAGE: usize = 64 * 1024 * 1024;

/// Tracks the packet sequence number, which resets at the start of every
/// command and must increment by one per packet or the client disconnects.
#[derive(Debug, Default, Clone, Copy)]
pub struct Sequence(u8);

impl Sequence {
    pub fn new() -> Self {
        Self(0)
    }

    pub fn get(self) -> u8 {
        self.0
    }

    pub fn reset(&mut self) {
        self.0 = 0;
    }

    pub fn set(&mut self, v: u8) {
        self.0 = v;
    }

    /// Returns the number to stamp on the next packet and advances.
    pub fn advance(&mut self) -> u8 {
        let current = self.0;
        self.0 = self.0.wrapping_add(1);
        current
    }
}

/// Writes one logical message, splitting it across packets when it exceeds
/// `MAX_PAYLOAD`.
pub async fn write_packet<W>(w: &mut W, seq: &mut Sequence, payload: &[u8]) -> std::io::Result<()>
where
    W: AsyncWrite + Unpin,
{
    let mut chunks = payload.chunks(MAX_PAYLOAD);
    let mut wrote_any = false;

    for chunk in chunks.by_ref() {
        write_one(w, seq, chunk).await?;
        wrote_any = true;
    }

    // An empty payload still needs one packet on the wire, and a payload that
    // is an exact multiple of MAX_PAYLOAD needs an empty terminator.
    if !wrote_any || payload.len() % MAX_PAYLOAD == 0 && !payload.is_empty() {
        write_one(w, seq, &[]).await?;
    }

    Ok(())
}

async fn write_one<W>(w: &mut W, seq: &mut Sequence, chunk: &[u8]) -> std::io::Result<()>
where
    W: AsyncWrite + Unpin,
{
    let len = chunk.len() as u32;
    let header = [
        len as u8,
        (len >> 8) as u8,
        (len >> 16) as u8,
        seq.advance(),
    ];
    w.write_all(&header).await?;
    w.write_all(chunk).await
}

/// Reads one logical message, reassembling continuation packets.
///
/// Returns `Ok(None)` on a clean EOF at a packet boundary, which is what a
/// client that disappears without COM_QUIT looks like.
pub async fn read_packet<R>(
    r: &mut R,
    seq: &mut Sequence,
    max_message: usize,
) -> std::io::Result<Option<Vec<u8>>>
where
    R: AsyncRead + Unpin,
{
    let mut payload: Vec<u8> = Vec::new();

    loop {
        let mut header = [0u8; 4];
        match r.read_exact(&mut header).await {
            Ok(_) => {}
            Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof && payload.is_empty() => {
                return Ok(None);
            }
            Err(e) => return Err(e),
        }

        let len = u32::from_le_bytes([header[0], header[1], header[2], 0]) as usize;
        seq.set(header[3].wrapping_add(1));

        if payload.len().saturating_add(len) > max_message {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "packet exceeds the maximum allowed message size",
            ));
        }

        let start = payload.len();
        payload.resize(start + len, 0);
        r.read_exact(&mut payload[start..]).await?;

        // Short packet means the logical message is complete.
        if len < MAX_PAYLOAD {
            return Ok(Some(payload));
        }
    }
}

/// The body of an OK packet.
#[derive(Debug, Clone)]
pub struct OkPacket {
    pub affected_rows: u64,
    pub last_insert_id: u64,
    pub status_flags: u16,
    pub warnings: u16,
    pub info: String,
}

impl Default for OkPacket {
    fn default() -> Self {
        Self {
            affected_rows: 0,
            last_insert_id: 0,
            status_flags: status::AUTOCOMMIT,
            warnings: 0,
            info: String::new(),
        }
    }
}

impl OkPacket {
    pub fn new(affected_rows: u64, last_insert_id: u64) -> Self {
        Self {
            affected_rows,
            last_insert_id,
            ..Default::default()
        }
    }

    /// Encodes as a real OK packet (header 0x00).
    pub fn encode(&self, caps: Capabilities) -> Vec<u8> {
        self.encode_with_header(caps, 0x00)
    }

    /// Encodes as the OK packet that replaces a trailing EOF when the client
    /// negotiated `DEPRECATE_EOF`. Same body, header 0xFE.
    pub fn encode_as_eof(&self, caps: Capabilities) -> Vec<u8> {
        self.encode_with_header(caps, 0xfe)
    }

    fn encode_with_header(&self, caps: Capabilities, header: u8) -> Vec<u8> {
        let mut w = BufWriter::with_capacity(32);
        w.u8(header)
            .lenenc_int(self.affected_rows)
            .lenenc_int(self.last_insert_id);

        if caps.contains(Capabilities::PROTOCOL_41) {
            w.u16(self.status_flags).u16(self.warnings);
        } else if caps.contains(Capabilities::TRANSACTIONS) {
            w.u16(self.status_flags);
        }

        // With SESSION_TRACK the trailing info is length-encoded; without it,
        // it simply runs to the end of the packet.
        if caps.contains(Capabilities::SESSION_TRACK) {
            if !self.info.is_empty() {
                w.lenenc_str(&self.info);
            }
        } else {
            w.bytes(self.info.as_bytes());
        }

        w.into_inner()
    }
}

/// An EOF packet, used only when the client did not negotiate `DEPRECATE_EOF`.
pub fn encode_eof(caps: Capabilities, warnings: u16, status_flags: u16) -> Vec<u8> {
    let mut w = BufWriter::with_capacity(8);
    w.u8(0xfe);
    if caps.contains(Capabilities::PROTOCOL_41) {
        w.u16(warnings).u16(status_flags);
    }
    w.into_inner()
}

/// A MySQL error, carrying the numeric code and five-character SQLSTATE that
/// clients branch on. Matching real MySQL here matters: drivers and ORMs treat
/// specific codes as retryable, as unique-violations, and so on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ErrPacket {
    pub code: u16,
    pub sql_state: [u8; 5],
    pub message: String,
}

impl ErrPacket {
    pub fn new(code: u16, sql_state: &str, message: impl Into<String>) -> Self {
        let mut state = *b"HY000";
        let bytes = sql_state.as_bytes();
        if bytes.len() == 5 {
            state.copy_from_slice(bytes);
        }
        Self {
            code,
            sql_state: state,
            message: message.into(),
        }
    }

    /// ER_PARSE_ERROR.
    pub fn parse_error(message: impl Into<String>) -> Self {
        Self::new(1064, "42000", message)
    }

    /// ER_NOT_SUPPORTED_YET. The engine's honest answer for an unimplemented
    /// feature, so gaps surface as actionable errors rather than wrong results.
    pub fn unsupported(message: impl Into<String>) -> Self {
        Self::new(1235, "0A000", message)
    }

    /// ER_ACCESS_DENIED_ERROR.
    pub fn access_denied(user: &str, host: &str) -> Self {
        Self::new(
            1045,
            "28000",
            format!("Access denied for user '{user}'@'{host}' (using password: YES)"),
        )
    }

    /// ER_UNKNOWN_STMT_HANDLER.
    pub fn unknown_stmt_handler(what: &str) -> Self {
        Self::new(
            1243,
            "HY000",
            format!("Unknown prepared statement handler ({what}) given to mysqld"),
        )
    }

    pub fn encode(&self, caps: Capabilities) -> Vec<u8> {
        let mut w = BufWriter::with_capacity(16 + self.message.len());
        w.u8(0xff).u16(self.code);
        if caps.contains(Capabilities::PROTOCOL_41) {
            w.u8(b'#').bytes(&self.sql_state);
        }
        w.bytes(self.message.as_bytes());
        w.into_inner()
    }
}

impl std::fmt::Display for ErrPacket {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "ERROR {} ({}): {}",
            self.code,
            String::from_utf8_lossy(&self.sql_state),
            self.message
        )
    }
}

impl std::error::Error for ErrPacket {}
