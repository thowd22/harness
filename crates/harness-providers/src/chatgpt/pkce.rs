//! Fresh random values for each sign-in attempt: `state`, `nonce`, the
//! PKCE verifier and its `S256` challenge, and the per-host id.

use aws_lc_rs::digest::{SHA256, digest};
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use zeroize::Zeroizing;

/// `n` random bytes from the system CSPRNG.
fn random_bytes(n: usize) -> Vec<u8> {
    let mut buf = vec![0u8; n];
    aws_lc_rs::rand::fill(&mut buf).expect("system random number generator");
    buf
}

/// A base64url (no padding) string of `bytes` random bytes.
pub fn random_token(bytes: usize) -> String {
    URL_SAFE_NO_PAD.encode(random_bytes(bytes))
}

/// Base64url (no padding) SHA-256 of `verifier`: the `S256` challenge.
pub fn challenge_for(verifier: &str) -> String {
    URL_SAFE_NO_PAD.encode(digest(&SHA256, verifier.as_bytes()).as_ref())
}

/// A PKCE pair. The verifier is zeroized on drop and never printed.
pub struct Pkce {
    verifier: Zeroizing<String>,
    /// `code_challenge` for the authorization request.
    pub challenge: String,
}

impl std::fmt::Debug for Pkce {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Pkce {{ challenge: {} }}", self.challenge)
    }
}

impl Default for Pkce {
    fn default() -> Self {
        Self::new()
    }
}

impl Pkce {
    /// A fresh pair with a 43-character verifier (32 random bytes).
    pub fn new() -> Self {
        let verifier = Zeroizing::new(random_token(32));
        let challenge = challenge_for(&verifier);
        Self {
            verifier,
            challenge,
        }
    }

    /// `code_verifier` for the code exchange.
    pub fn verifier(&self) -> &str {
        &self.verifier
    }
}

/// A new `ext_agent_host_id`: a random (version 4) UUID as `urn:uuid:…`.
pub fn new_host_id() -> String {
    let mut b = [0u8; 16];
    b.copy_from_slice(&random_bytes(16));
    b[6] = (b[6] & 0x0f) | 0x40;
    b[8] = (b[8] & 0x3f) | 0x80;
    let h: String = b.iter().map(|x| format!("{x:02x}")).collect();
    format!(
        "urn:uuid:{}-{}-{}-{}-{}",
        &h[0..8],
        &h[8..12],
        &h[12..16],
        &h[16..20],
        &h[20..32]
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pkce_matches_rfc_7636_example() {
        // RFC 7636 appendix B.
        assert_eq!(
            challenge_for("dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk"),
            "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"
        );
        let p = Pkce::new();
        assert_eq!(p.verifier().len(), 43);
        assert_eq!(p.challenge, challenge_for(p.verifier()));
        assert!(!format!("{p:?}").contains(p.verifier()));
    }

    #[test]
    fn values_are_fresh() {
        assert_ne!(random_token(16), random_token(16));
        assert_ne!(Pkce::new().challenge, Pkce::new().challenge);
        let id = new_host_id();
        assert!(id.starts_with("urn:uuid:"), "{id}");
        assert_eq!(id.len(), 9 + 36);
        assert_eq!(&id[9 + 14..9 + 15], "4");
        assert_ne!(id, new_host_id());
    }
}
