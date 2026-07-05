use base64::Engine;
use x25519_dalek::{PublicKey, StaticSecret};

/// A WireGuard key pair, base64 encoded the way the wire protocol expects.
pub struct Pair {
    pub private: String,
    pub public: String,
}

/// Generates a key pair locally. The private half is never sent anywhere.
pub fn generate() -> Result<Pair, String> {
    let mut seed = [0u8; 32];
    getrandom::fill(&mut seed).map_err(|e| format!("no randomness available: {e}"))?;

    let secret = StaticSecret::from(seed);
    let public = PublicKey::from(&secret);

    let engine = base64::engine::general_purpose::STANDARD;
    Ok(Pair {
        private: engine.encode(secret.to_bytes()),
        public: engine.encode(public.as_bytes()),
    })
}
