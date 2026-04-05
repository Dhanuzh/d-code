/// Shared OAuth / PKCE utilities.
use base64::Engine;
use sha2::{Digest, Sha256};

// ── PKCE ──────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct PkcePair {
    pub verifier: String,
    pub challenge: String,
}

/// Generate a fresh PKCE S256 code-verifier + challenge pair.
pub fn generate_pkce() -> PkcePair {
    let mut bytes = [0u8; 64];
    use rand::RngCore;
    rand::thread_rng().fill_bytes(&mut bytes);

    let verifier = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes);

    let digest = Sha256::digest(verifier.as_bytes());
    let challenge = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(digest);

    PkcePair {
        verifier,
        challenge,
    }
}

// ── URL encoding ──────────────────────────────────────────────────────────────

/// Percent-encode a string for use in OAuth query parameters.
pub fn url_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for byte in s.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(byte as char)
            }
            b' ' => out.push('+'),
            b => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}
