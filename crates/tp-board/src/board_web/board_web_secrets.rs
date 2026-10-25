//! Shared web-secret primitives: hex encoding, constant-time comparison,
//! and operating-system randomness for tokens, nonces, and proofs.
use std::{fs::File, io::Read};

/// Hex length of a 32-byte proof or nonce.
pub(crate) const PROOF_HEX_LEN: usize = 64;

/// Lowercase hex encoding of secret bytes.
pub(crate) fn hex_encode(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut encoded = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        encoded.push(HEX[usize::from(byte >> 4)] as char);
        encoded.push(HEX[usize::from(byte & 15)] as char);
    }
    encoded
}

/// Constant-time equality: length and every expected position feed one accumulator.
pub(crate) fn constant_time_equal(expected: &[u8], candidate: &[u8]) -> bool {
    let mut difference = candidate.len() ^ expected.len();
    for (index, expected) in expected.iter().enumerate() {
        difference |= usize::from(expected ^ candidate.get(index).copied().unwrap_or(0));
    }
    difference == 0
}

/// Fill `bytes` from the operating-system random source.
pub(crate) fn fill_random(bytes: &mut [u8]) -> std::io::Result<()> {
    File::open("/dev/urandom").and_then(|mut source| source.read_exact(bytes))
}
