//! Unauthenticated listener-ownership proof: HMAC-SHA256 over a domain label,
//! nonce, and address. The token keys the proof and never crosses the wire; a
//! fresh random nonce and the bound listener address make proofs worthless to
//! a relaying thief.

use super::super::board_web_secrets::{
    PROOF_HEX_LEN, constant_time_equal, fill_random, hex_encode,
};
use super::BoardWebToken;
use anyhow::{Context, Result, ensure};
use sha2::{Digest, Sha256};
use std::net::SocketAddr;

pub(crate) const NONCE_BYTES: usize = 32;

#[derive(Clone)]
pub(crate) struct ChallengeNonce([u8; NONCE_BYTES]);

impl ChallengeNonce {
    pub(crate) fn generate() -> Result<Self> {
        let mut bytes = [0; NONCE_BYTES];
        fill_random(&mut bytes)
            .context("board_web_challenge: read operating-system random source")?;
        Ok(Self(bytes))
    }

    pub(crate) fn from_hex(text: &str) -> Result<Self> {
        ensure!(
            text.len() == PROOF_HEX_LEN && text.bytes().all(|byte| byte.is_ascii_hexdigit()),
            "board_web_challenge: nonce must be 64 hexadecimal characters"
        );
        let value = |byte: u8| match byte {
            b'0'..=b'9' => byte - b'0',
            b'a'..=b'f' => byte - b'a' + 10,
            _ => byte - b'A' + 10,
        };
        let mut bytes = [0; NONCE_BYTES];
        for (index, pair) in text.as_bytes().chunks_exact(2).enumerate() {
            bytes[index] = value(pair[0]) << 4 | value(pair[1]);
        }
        Ok(Self(bytes))
    }

    pub(crate) fn to_hex(&self) -> String {
        hex_encode(&self.0)
    }
}

/// Domain label separating listener proofs from any other token-keyed MAC.
const PROOF_LABEL: &[u8] = b"trufflepig-board-listener-proof-v1";

/// HMAC-SHA256(key = token, message = label NUL || nonce || address), hex.
pub(crate) fn proof_hex(
    token: &BoardWebToken,
    nonce: &ChallengeNonce,
    address: &SocketAddr,
) -> String {
    hex_encode(&proof(token, nonce, address))
}

fn proof(token: &BoardWebToken, nonce: &ChallengeNonce, address: &SocketAddr) -> [u8; 32] {
    let mut message = Vec::with_capacity(PROOF_LABEL.len() + 1 + NONCE_BYTES + 22);
    message.extend_from_slice(PROOF_LABEL);
    message.push(0);
    message.extend_from_slice(&nonce.0);
    message.extend_from_slice(address.to_string().as_bytes());
    hmac_sha256(token.expose().as_bytes(), &message)
}

/// Constant-time comparison of a received hex proof against the local one.
pub(crate) fn proof_matches(
    token: &BoardWebToken,
    nonce: &ChallengeNonce,
    address: &SocketAddr,
    candidate_hex: &str,
) -> bool {
    let expected = hex_encode(&proof(token, nonce, address));
    constant_time_equal(expected.as_bytes(), candidate_hex.as_bytes())
}

fn hmac_sha256(key: &[u8], message: &[u8]) -> [u8; 32] {
    const BLOCK: usize = 64;
    let mut key_block = [0; BLOCK];
    if key.len() > BLOCK {
        key_block[..32].copy_from_slice(&Sha256::digest(key));
    } else {
        key_block[..key.len()].copy_from_slice(key);
    }
    let mut ipad = [0; BLOCK];
    let mut opad = [0; BLOCK];
    for index in 0..BLOCK {
        ipad[index] = key_block[index] ^ 0x36;
        opad[index] = key_block[index] ^ 0x5c;
    }
    let mut inner = Sha256::new();
    inner.update(ipad);
    inner.update(message);
    let inner_hash = inner.finalize();
    let mut outer = Sha256::new();
    outer.update(opad);
    outer.update(inner_hash);
    outer.finalize().into()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hex(bytes: &[u8]) -> String {
        bytes.iter().map(|byte| format!("{byte:02x}")).collect()
    }

    #[test]
    fn hmac_sha256_matches_rfc4231_vectors() {
        assert_eq!(
            hex(&hmac_sha256(&[0x0b; 20], b"Hi There")),
            "b0344c61d8db38535ca8afceaf0bf12b881dc200c9833da726e9376c2e32cff7"
        );
        assert_eq!(
            hex(&hmac_sha256(b"Jefe", b"what do ya want for nothing?")),
            "5bdcc146bf60754e6a042426089575c75a003f089d2739839dec58b964ec3843"
        );
        assert_eq!(
            hex(&hmac_sha256(
                &[0xaa; 131],
                b"Test Using Larger Than Block-Size Key - Hash Key First"
            )),
            "60e431591ee0b67f0d8a26aacbf5b77f8e0bc6213728c5140546040f0ee37f54"
        );
    }

    #[test]
    fn hmac_sha256_pins_a_token_length_block_boundary_key() {
        let key: Vec<u8> = (0..64).collect();
        assert_eq!(
            hex(&hmac_sha256(&key, b"token-length key boundary")),
            "b969df972b168671adbc59da398442a0ba4b208d65b746d6be48bdbe4ebcded7"
        );
    }

    #[test]
    fn proof_prefixes_a_domain_label_before_nonce_and_address() {
        let directory = crate::board::board_test_support::scratch("board-challenge-");
        let path = directory.path().join("token");
        std::fs::write(&path, "0123456789abcdef".repeat(4)).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        }
        let token = BoardWebToken::read_at(&path).unwrap();
        assert_eq!(token.expose().len(), 64);
        let nonce = ChallengeNonce::from_hex(
            &(0..32).map(|byte| format!("{byte:02x}")).collect::<String>(),
        )
        .unwrap();
        let address: SocketAddr = "127.0.0.1:7341".parse().unwrap();
        assert_eq!(
            proof_hex(&token, &nonce, &address),
            "b59701424c6fc8d95335135b5c96802846acbd291d7d95abe6214c00181ae3f2"
        );
    }

    #[test]
    fn nonce_hex_roundtrips_and_rejects_malformed_text() {
        let nonce = ChallengeNonce::generate().unwrap();
        assert_eq!(
            ChallengeNonce::from_hex(&nonce.to_hex()).unwrap().0,
            nonce.0
        );
        for bad in ["", &"a".repeat(63), &"a".repeat(65), &"z".repeat(64)] {
            assert!(ChallengeNonce::from_hex(bad).is_err(), "{bad}");
        }
        assert_ne!(
            ChallengeNonce::generate().unwrap().0,
            ChallengeNonce::generate().unwrap().0
        );
    }

    #[test]
    fn proof_binds_token_nonce_and_listener_address() {
        let directory = crate::board::board_test_support::scratch("board-challenge-");
        let token = BoardWebToken::rotate_at(&directory.path().join("token")).unwrap();
        let other = BoardWebToken::rotate_at(&directory.path().join("other")).unwrap();
        let nonce = ChallengeNonce::generate().unwrap();
        let address: SocketAddr = "127.0.0.1:7341".parse().unwrap();
        let proof = proof_hex(&token, &nonce, &address);
        assert_eq!(proof.len(), 64);
        assert!(proof_matches(&token, &nonce, &address, &proof));
        assert!(!proof_matches(&other, &nonce, &address, &proof));
        let elsewhere: SocketAddr = "127.0.0.1:7342".parse().unwrap();
        assert!(!proof_matches(&token, &nonce, &elsewhere, &proof));
        let replay = ChallengeNonce::generate().unwrap();
        assert!(!proof_matches(&token, &replay, &address, &proof));
        assert!(!proof_matches(&token, &nonce, &address, &proof[..63]));
        assert!(!proof_matches(&token, &nonce, &address, "zz"));
    }
}
