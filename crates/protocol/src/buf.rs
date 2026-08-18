//! Primitive encoders and decoders for the MySQL wire format.
//!
//! Everything on the wire is little-endian. Strings come in three flavours:
//! length-encoded (a variable-width integer prefix), null-terminated, and
//! "rest of packet". Getting these wrong is the single most common source of
//! desynchronisation, so they all live here rather than being open-coded.

/// Appends values to a packet payload.
#[derive(Debug, Default)]
pub struct BufWriter {
    buf: Vec<u8>,
}

impl BufWriter {
    pub fn new() -> Self {
        Self { buf: Vec::new() }
    }

    pub fn with_capacity(capacity: usize) -> Self {
        Self {
            buf: Vec::with_capacity(capacity),
        }
    }

    pub fn into_inner(self) -> Vec<u8> {
        self.buf
    }

    pub fn as_slice(&self) -> &[u8] {
        &self.buf
    }

    pub fn len(&self) -> usize {
        self.buf.len()
    }

    pub fn is_empty(&self) -> bool {
        self.buf.is_empty()
    }

    pub fn u8(&mut self, v: u8) -> &mut Self {
        self.buf.push(v);
        self
    }

    pub fn u16(&mut self, v: u16) -> &mut Self {
        self.buf.extend_from_slice(&v.to_le_bytes());
        self
    }

    pub fn u24(&mut self, v: u32) -> &mut Self {
        self.buf.extend_from_slice(&v.to_le_bytes()[..3]);
        self
    }

    pub fn u32(&mut self, v: u32) -> &mut Self {
        self.buf.extend_from_slice(&v.to_le_bytes());
        self
    }

    pub fn u64(&mut self, v: u64) -> &mut Self {
        self.buf.extend_from_slice(&v.to_le_bytes());
        self
    }

    pub fn f32(&mut self, v: f32) -> &mut Self {
        self.buf.extend_from_slice(&v.to_le_bytes());
        self
    }

    pub fn f64(&mut self, v: f64) -> &mut Self {
        self.buf.extend_from_slice(&v.to_le_bytes());
        self
    }

    pub fn bytes(&mut self, v: &[u8]) -> &mut Self {
        self.buf.extend_from_slice(v);
        self
    }

    /// `n` zero bytes, used for the protocol's reserved/filler regions.
    pub fn zeros(&mut self, n: usize) -> &mut Self {
        self.buf.resize(self.buf.len() + n, 0);
        self
    }

    /// A length-encoded integer, the protocol's variable-width unsigned int.
    pub fn lenenc_int(&mut self, v: u64) -> &mut Self {
        match v {
            // 0xFB is NULL and 0xFF is ERR, so single-byte form stops at 0xFA.
            0..=0xfa => self.u8(v as u8),
            0xfb..=0xffff => {
                self.u8(0xfc).u16(v as u16);
                self
            }
            0x1_0000..=0xff_ffff => {
                self.u8(0xfd).u24(v as u32);
                self
            }
            _ => {
                self.u8(0xfe).u64(v);
                self
            }
        }
    }

    /// A length-encoded string: a `lenenc_int` length followed by the bytes.
    pub fn lenenc_bytes(&mut self, v: &[u8]) -> &mut Self {
        self.lenenc_int(v.len() as u64).bytes(v)
    }

    pub fn lenenc_str(&mut self, v: &str) -> &mut Self {
        self.lenenc_bytes(v.as_bytes())
    }

    /// A null-terminated string. The caller must ensure `v` has no interior NUL.
    pub fn null_str(&mut self, v: &str) -> &mut Self {
        self.bytes(v.as_bytes()).u8(0)
    }

    pub fn null_bytes(&mut self, v: &[u8]) -> &mut Self {
        self.bytes(v).u8(0)
    }
}

/// The error returned when a payload ends earlier than the protocol requires.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Truncated;

impl std::fmt::Display for Truncated {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("packet payload ended unexpectedly")
    }
}

impl std::error::Error for Truncated {}

pub type BufResult<T> = Result<T, Truncated>;

/// Reads values out of a packet payload, tracking a cursor.
#[derive(Debug, Clone)]
pub struct BufReader<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> BufReader<'a> {
    pub fn new(data: &'a [u8]) -> Self {
        Self { data, pos: 0 }
    }

    pub fn position(&self) -> usize {
        self.pos
    }

    pub fn remaining(&self) -> usize {
        self.data.len().saturating_sub(self.pos)
    }

    pub fn is_empty(&self) -> bool {
        self.remaining() == 0
    }

    /// Everything not yet consumed, without advancing.
    pub fn peek_rest(&self) -> &'a [u8] {
        &self.data[self.pos.min(self.data.len())..]
    }

    /// Everything not yet consumed, advancing to the end.
    pub fn rest(&mut self) -> &'a [u8] {
        let out = self.peek_rest();
        self.pos = self.data.len();
        out
    }

    pub fn skip(&mut self, n: usize) -> BufResult<()> {
        self.take(n).map(|_| ())
    }

    pub fn take(&mut self, n: usize) -> BufResult<&'a [u8]> {
        let end = self.pos.checked_add(n).ok_or(Truncated)?;
        let out = self.data.get(self.pos..end).ok_or(Truncated)?;
        self.pos = end;
        Ok(out)
    }

    pub fn u8(&mut self) -> BufResult<u8> {
        Ok(self.take(1)?[0])
    }

    /// Reads a byte without consuming it.
    pub fn peek_u8(&self) -> BufResult<u8> {
        self.data.get(self.pos).copied().ok_or(Truncated)
    }

    pub fn u16(&mut self) -> BufResult<u16> {
        let b = self.take(2)?;
        Ok(u16::from_le_bytes([b[0], b[1]]))
    }

    pub fn u24(&mut self) -> BufResult<u32> {
        let b = self.take(3)?;
        Ok(u32::from_le_bytes([b[0], b[1], b[2], 0]))
    }

    pub fn u32(&mut self) -> BufResult<u32> {
        let b = self.take(4)?;
        Ok(u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    }

    pub fn u64(&mut self) -> BufResult<u64> {
        let b = self.take(8)?;
        Ok(u64::from_le_bytes([
            b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7],
        ]))
    }

    pub fn f32(&mut self) -> BufResult<f32> {
        let b = self.take(4)?;
        Ok(f32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    }

    pub fn f64(&mut self) -> BufResult<f64> {
        let b = self.take(8)?;
        Ok(f64::from_le_bytes([
            b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7],
        ]))
    }

    /// A length-encoded integer. `None` means the 0xFB NULL marker.
    pub fn lenenc_int(&mut self) -> BufResult<Option<u64>> {
        match self.u8()? {
            0xfb => Ok(None),
            0xfc => Ok(Some(self.u16()? as u64)),
            0xfd => Ok(Some(self.u24()? as u64)),
            0xfe => Ok(Some(self.u64()?)),
            v => Ok(Some(v as u64)),
        }
    }

    /// A length-encoded string. `None` means the 0xFB NULL marker.
    pub fn lenenc_bytes(&mut self) -> BufResult<Option<&'a [u8]>> {
        match self.lenenc_int()? {
            None => Ok(None),
            Some(n) => {
                let n = usize::try_from(n).map_err(|_| Truncated)?;
                self.take(n).map(Some)
            }
        }
    }

    /// Bytes up to the next NUL, consuming the terminator.
    pub fn null_bytes(&mut self) -> BufResult<&'a [u8]> {
        let rest = self.peek_rest();
        let end = rest.iter().position(|&b| b == 0).ok_or(Truncated)?;
        let out = &rest[..end];
        self.pos += end + 1;
        Ok(out)
    }

    /// A null-terminated string, decoded lossily so malformed UTF-8 from a
    /// client cannot take a connection down.
    pub fn null_str(&mut self) -> BufResult<String> {
        Ok(String::from_utf8_lossy(self.null_bytes()?).into_owned())
    }
}
