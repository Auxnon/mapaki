use std::fs;
use std::path::PathBuf;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use data_encoding::{BASE32_NOPAD, HEXLOWER};
use rand::RngCore;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;

const OTP_TTL_SECS: u64 = 10 * 60;
const MAX_OTP_ATTEMPTS: u8 = 5;

fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

fn random_bytes(n: usize) -> Vec<u8> {
    let mut buf = vec![0u8; n];
    rand::thread_rng().fill_bytes(&mut buf);
    buf
}

fn sha256_hex(s: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(s.as_bytes());
    HEXLOWER.encode(&hasher.finalize())
}

fn ct_eq(a: &str, b: &str) -> bool {
    a.as_bytes().ct_eq(b.as_bytes()).into()
}

struct PendingOtp {
    code: String,
    expires_at: Instant,
    attempts: u8,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct TokenRecord {
    hash: String,
    created_at: i64,
}

pub struct Auth {
    tokens_path: PathBuf,
    pending: std::sync::Mutex<Option<PendingOtp>>,
    tokens: std::sync::Mutex<Vec<TokenRecord>>,
}

impl Auth {
    pub fn load(tokens_path: PathBuf) -> Self {
        let tokens: Vec<TokenRecord> = fs::read(&tokens_path)
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default();
        Auth {
            tokens_path,
            pending: std::sync::Mutex::new(None),
            tokens: std::sync::Mutex::new(tokens),
        }
    }

    fn save_tokens(&self) {
        if let Ok(tokens) = self.tokens.lock() {
            if let Ok(bytes) = serde_json::to_vec_pretty(&*tokens) {
                let tmp = self.tokens_path.with_extension("tmp");
                if fs::write(&tmp, bytes).is_ok() {
                    let _ = fs::rename(&tmp, &self.tokens_path);
                }
            }
        }
    }

    /// Generates a fresh one-time pairing code, printable to the console.
    pub fn start_pairing(&self) -> String {
        let code = BASE32_NOPAD.encode(&random_bytes(5)); // ~40 bits, 8 chars
        *self.pending.lock().unwrap() = Some(PendingOtp {
            code: code.clone(),
            expires_at: Instant::now() + std::time::Duration::from_secs(OTP_TTL_SECS),
            attempts: 0,
        });
        code
    }

    /// Verifies a submitted pairing code and, on success, mints a new bearer
    /// token (returned raw, once) whose hash is persisted for future checks.
    pub fn redeem_pairing(&self, submitted: &str) -> Result<String, &'static str> {
        let mut pending = self.pending.lock().unwrap();
        let otp = pending.as_mut().ok_or("no pairing in progress")?;

        if Instant::now() > otp.expires_at {
            *pending = None;
            return Err("pairing code expired");
        }
        if otp.attempts >= MAX_OTP_ATTEMPTS {
            *pending = None;
            return Err("too many attempts");
        }
        otp.attempts += 1;

        if !ct_eq(&otp.code, submitted.trim()) {
            return Err("invalid pairing code");
        }
        *pending = None;
        drop(pending);

        let token = HEXLOWER.encode(&random_bytes(32));
        let hash = sha256_hex(&token);
        self.tokens.lock().unwrap().push(TokenRecord {
            hash,
            created_at: now(),
        });
        self.save_tokens();
        Ok(token)
    }

    pub fn check_token(&self, presented: &str) -> bool {
        let hash = sha256_hex(presented);
        self.tokens
            .lock()
            .unwrap()
            .iter()
            .any(|t| ct_eq(&t.hash, &hash))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_auth() -> (Auth, tempdir::TempDir) {
        let dir = tempdir::TempDir::new("mapaki-auth-test").unwrap();
        let auth = Auth::load(dir.path().join("tokens.json"));
        (auth, dir)
    }

    #[test]
    fn correct_otp_mints_a_working_token() {
        let (auth, _dir) = temp_auth();
        let otp = auth.start_pairing();
        let token = auth.redeem_pairing(&otp).unwrap();
        assert!(auth.check_token(&token));
    }

    #[test]
    fn wrong_otp_is_rejected_and_does_not_mint_a_token() {
        let (auth, _dir) = temp_auth();
        let _otp = auth.start_pairing();
        assert!(auth.redeem_pairing("not-it").is_err());
    }

    #[test]
    fn otp_is_single_use() {
        let (auth, _dir) = temp_auth();
        let otp = auth.start_pairing();
        assert!(auth.redeem_pairing(&otp).is_ok());
        assert!(auth.redeem_pairing(&otp).is_err());
    }

    #[test]
    fn unknown_token_is_rejected() {
        let (auth, _dir) = temp_auth();
        assert!(!auth.check_token("deadbeef"));
    }

    #[test]
    fn too_many_wrong_attempts_burns_the_pairing() {
        let (auth, _dir) = temp_auth();
        let otp = auth.start_pairing();
        for _ in 0..MAX_OTP_ATTEMPTS {
            let _ = auth.redeem_pairing("wrong");
        }
        // Even the correct code no longer works once attempts are exhausted.
        assert!(auth.redeem_pairing(&otp).is_err());
    }
}
