//! Passwords (Argon2id), session tokens and a simple brute-force limiter.

use argon2::password_hash::{rand_core::OsRng, PasswordHash, PasswordHasher, PasswordVerifier, SaltString};
use argon2::Argon2;
use base64::Engine;
use rand::RngCore;
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::sync::{LazyLock, Mutex};
use std::time::{Duration, Instant};

/// Hashes a password with Argon2id (PHC string format, salt included).
pub fn hash_password(password: &str) -> String {
    let salt = SaltString::generate(&mut OsRng);
    Argon2::default()
        .hash_password(password.as_bytes(), &salt)
        .expect("argon2 hashing failed")
        .to_string()
}

pub fn verify_password(password: &str, stored: &str) -> bool {
    PasswordHash::new(stored)
        .map(|h| Argon2::default().verify_password(password.as_bytes(), &h).is_ok())
        .unwrap_or(false)
}

/// Hash checked when a username does not exist, so response time doesn't reveal valid usernames.
pub static DUMMY_HASH: LazyLock<String> = LazyLock::new(|| hash_password("dummy-password-for-timing"));

/// A new random 256-bit session token (URL-safe base64). Only its SHA-256 is stored.
pub fn new_token() -> String {
    let mut b = [0u8; 32];
    rand::thread_rng().fill_bytes(&mut b);
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(b)
}

pub fn sha256_hex(value: &str) -> String {
    hex::encode(Sha256::digest(value.as_bytes()))
}

/// After `max_failures` failures within `window`, the key is blocked until the window has passed.
pub struct RateLimiter {
    max_failures: u32,
    window: Duration,
    entries: Mutex<HashMap<String, (u32, Instant)>>,
}

impl RateLimiter {
    pub fn new(max_failures: u32, window: Duration) -> Self {
        Self { max_failures, window, entries: Mutex::new(HashMap::new()) }
    }

    pub fn is_blocked(&self, key: &str) -> bool {
        let mut map = self.entries.lock().unwrap();
        match map.get(key) {
            Some((_, start)) if start.elapsed() > self.window => {
                map.remove(key);
                false
            }
            Some((failures, _)) => *failures >= self.max_failures,
            None => false,
        }
    }

    pub fn record_failure(&self, key: &str) {
        let mut map = self.entries.lock().unwrap();
        let now = Instant::now();
        let entry = map.entry(key.to_string()).or_insert((0, now));
        if entry.1.elapsed() > self.window {
            *entry = (0, now);
        }
        entry.0 += 1;
        if map.len() > 10_000 {
            let window = self.window;
            map.retain(|_, (_, start)| start.elapsed() <= window);
        }
    }

    pub fn reset(&self, key: &str) {
        self.entries.lock().unwrap().remove(key);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn passwords() {
        let h = hash_password("geheim123");
        assert!(verify_password("geheim123", &h));
        assert!(!verify_password("geheim124", &h));
        assert!(!verify_password("geheim123", "garbage"));
        assert_ne!(hash_password("geheim123"), h, "salted");
    }

    #[test]
    fn tokens() {
        let a = new_token();
        assert_eq!(a.len(), 43);
        assert_ne!(a, new_token());
        assert_eq!(sha256_hex("abc").len(), 64);
    }

    #[test]
    fn rate_limiter() {
        let r = RateLimiter::new(3, Duration::from_secs(60));
        for _ in 0..3 {
            assert!(!r.is_blocked("k"));
            r.record_failure("k");
        }
        assert!(r.is_blocked("k"));
        r.reset("k");
        assert!(!r.is_blocked("k"));
    }
}
