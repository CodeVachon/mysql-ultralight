//! Authentication.
//!
//! The target deployment runs MySQL with `--mysql-native-password=ON`, so that
//! is the plugin this server advertises. A client that opens with
//! `caching_sha2_password` (the MySQL 8 default) is sent an auth-switch
//! request and re-authenticates, which is exactly what real MySQL does.

use sha1::{Digest, Sha1};

pub const MYSQL_NATIVE_PASSWORD: &str = "mysql_native_password";
pub const CACHING_SHA2_PASSWORD: &str = "caching_sha2_password";

/// The nonce length mandated by the protocol: 20 bytes, sent as an 8-byte and
/// a 12-byte half with a NUL between them.
pub const NONCE_LEN: usize = 20;

/// Computes the `mysql_native_password` scramble.
///
/// `SHA1(password) XOR SHA1(nonce || SHA1(SHA1(password)))`
///
/// An empty password produces an empty scramble, which is how the protocol
/// signals "no password".
pub fn native_password_scramble(password: &[u8], nonce: &[u8]) -> Vec<u8> {
    if password.is_empty() {
        return Vec::new();
    }

    let stage1 = Sha1::digest(password);
    let stage2 = Sha1::digest(stage1);

    let mut hasher = Sha1::new();
    hasher.update(nonce);
    hasher.update(stage2);
    let seeded = hasher.finalize();

    stage1
        .iter()
        .zip(seeded.iter())
        .map(|(a, b)| a ^ b)
        .collect()
}

/// Verifies a client's `mysql_native_password` response in constant time with
/// respect to content, comparing every byte rather than short-circuiting.
pub fn verify_native_password(expected_password: &[u8], nonce: &[u8], response: &[u8]) -> bool {
    let expected = native_password_scramble(expected_password, nonce);
    if expected.len() != response.len() {
        return false;
    }
    let mut diff = 0u8;
    for (a, b) in expected.iter().zip(response.iter()) {
        diff |= a ^ b;
    }
    diff == 0
}

/// Generates a handshake nonce.
///
/// This is a test-only database engine that does not hold real secrets, so the
/// nonce is drawn from a cheap xorshift seeded by the clock and the connection
/// id rather than a CSPRNG. That keeps the dependency tree and the startup
/// cost down. It is not suitable for a server exposed to a network.
pub fn generate_nonce(connection_id: u32) -> [u8; NONCE_LEN] {
    let seed = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0x2545_f491_4f6c_dd1d)
        ^ ((connection_id as u64) << 32 | connection_id as u64);

    let mut state = seed | 1;
    let mut nonce = [0u8; NONCE_LEN];
    for slot in nonce.iter_mut() {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        // The nonce is transmitted as a null-terminated string, so no byte may
        // be zero. Bias into the printable ASCII range, as MySQL itself does.
        *slot = ((state % 94) as u8) + 33;
    }
    nonce
}
