//! PKCE (RFC 7636).

use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use sha2::{Digest, Sha256};

#[derive(Debug, Clone)]
pub struct Pkce {
    pub verifier: String,
    pub challenge: String,
}

/// `challenge = base64url(sha256(verifier))`, method S256.
pub fn challenge_for(verifier: &str) -> String {
    URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()))
}

fn random_b64(bytes: usize) -> String {
    let mut buf = vec![0u8; bytes];
    getrandom::fill(&mut buf).expect("the system random source is unavailable");
    URL_SAFE_NO_PAD.encode(buf)
}

impl Pkce {
    /// 32 random bytes, base64url without padding.
    pub fn generate() -> Self {
        let verifier = random_b64(32);
        let challenge = challenge_for(&verifier);
        Self { verifier, challenge }
    }
}

/// A UUID; claude.ai refuses a shorter `state` ("Invalid request format").
pub fn random_state() -> String {
    uuid::Uuid::new_v4().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn appendix_b_vector() {
        assert_eq!(
            challenge_for("dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk"),
            "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"
        );
    }

    #[test]
    fn generated_pair_is_consistent() {
        let p = Pkce::generate();
        assert_eq!(p.verifier.len(), 43);
        assert_eq!(p.challenge, challenge_for(&p.verifier));
        assert_ne!(p.verifier, Pkce::generate().verifier);
    }
}
