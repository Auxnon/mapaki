use std::fs;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

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

/// Persisted next to the tokens so `mapaki-daemon pair-code` (a separate
/// process) can mint a code the running daemon will accept.
#[derive(Serialize, Deserialize)]
struct PendingOtp {
    code: String,
    expires_at: i64,
    attempts: u8,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct TokenRecord {
    hash: String,
    created_at: i64,
    /// Label recorded on revisions this device makes.
    #[serde(default)]
    device: String,
}

impl TokenRecord {
    fn label(&self) -> String {
        if self.device.is_empty() {
            format!("device-{}", &self.hash[..8])
        } else {
            self.device.clone()
        }
    }
}

pub struct Auth {
    tokens_path: PathBuf,
    otp_path: PathBuf,
    pending: std::sync::Mutex<()>,
    tokens: std::sync::Mutex<Vec<TokenRecord>>,
}

impl Auth {
    pub fn load(tokens_path: PathBuf) -> Self {
        let tokens: Vec<TokenRecord> = fs::read(&tokens_path)
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default();
        let otp_path = tokens_path.with_file_name(".mapaki-otp.json");
        Auth {
            tokens_path,
            otp_path,
            pending: std::sync::Mutex::new(()),
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

    fn write_otp(&self, otp: Option<&PendingOtp>) {
        match otp {
            Some(otp) => {
                let tmp = self.otp_path.with_extension("tmp");
                if fs::write(&tmp, serde_json::to_vec(otp).unwrap()).is_ok() {
                    let _ = fs::rename(&tmp, &self.otp_path);
                }
            }
            None => {
                let _ = fs::remove_file(&self.otp_path);
            }
        }
    }

    /// Generates a fresh one-time pairing code, printable to the console.
    /// Replaces any code that was still pending.
    pub fn start_pairing(&self) -> String {
        let _guard = self.pending.lock().unwrap();
        let code = BASE32_NOPAD.encode(&random_bytes(5)); // ~40 bits, 8 chars
        self.write_otp(Some(&PendingOtp {
            code: code.clone(),
            expires_at: now() + OTP_TTL_SECS as i64,
            attempts: 0,
        }));
        code
    }

    /// Verifies a submitted pairing code and, on success, mints a new bearer
    /// token (returned raw, once) whose hash is persisted for future checks.
    pub fn redeem_pairing(&self, submitted: &str, device: Option<&str>) -> Result<String, &'static str> {
        let guard = self.pending.lock().unwrap();
        let mut otp: PendingOtp = fs::read(&self.otp_path)
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .ok_or("no pairing in progress — run `mapaki-daemon pair-code` on the server")?;

        if now() > otp.expires_at {
            self.write_otp(None);
            return Err("pairing code expired");
        }
        if otp.attempts >= MAX_OTP_ATTEMPTS {
            self.write_otp(None);
            return Err("too many attempts");
        }
        otp.attempts += 1;

        if !ct_eq(&otp.code, submitted.trim()) {
            self.write_otp(Some(&otp));
            return Err("invalid pairing code");
        }
        self.write_otp(None);
        drop(guard);

        let token = HEXLOWER.encode(&random_bytes(32));
        let hash = sha256_hex(&token);
        let device: String = device
            .unwrap_or("")
            .chars()
            .filter(|c| c.is_alphanumeric() || matches!(c, '-' | '_' | ' ' | '.'))
            .take(40)
            .collect();
        self.tokens.lock().unwrap().push(TokenRecord {
            hash,
            created_at: now(),
            device: device.trim().to_string(),
        });
        self.save_tokens();
        Ok(token)
    }

    /// The device label for a valid token, None if the token is unknown.
    pub fn check_token(&self, presented: &str) -> Option<String> {
        let hash = sha256_hex(presented);
        self.tokens
            .lock()
            .unwrap()
            .iter()
            .find(|t| ct_eq(&t.hash, &hash))
            .map(|t| t.label())
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
        let token = auth.redeem_pairing(&otp, None).unwrap();
        assert!(auth.check_token(&token).is_some());
    }

    #[test]
    fn wrong_otp_is_rejected_and_does_not_mint_a_token() {
        let (auth, _dir) = temp_auth();
        let _otp = auth.start_pairing();
        assert!(auth.redeem_pairing("not-it", None).is_err());
    }

    #[test]
    fn otp_is_single_use() {
        let (auth, _dir) = temp_auth();
        let otp = auth.start_pairing();
        assert!(auth.redeem_pairing(&otp, None).is_ok());
        assert!(auth.redeem_pairing(&otp, None).is_err());
    }

    #[test]
    fn unknown_token_is_rejected() {
        let (auth, _dir) = temp_auth();
        assert!(auth.check_token("deadbeef").is_none());
    }

    #[test]
    fn too_many_wrong_attempts_burns_the_pairing() {
        let (auth, _dir) = temp_auth();
        let otp = auth.start_pairing();
        for _ in 0..MAX_OTP_ATTEMPTS {
            let _ = auth.redeem_pairing("wrong", None);
        }
        // Even the correct code no longer works once attempts are exhausted.
        assert!(auth.redeem_pairing(&otp, None).is_err());
    }

    #[test]
    fn code_minted_by_another_process_is_accepted() {
        let (auth, dir) = temp_auth();
        // `mapaki-daemon pair-code` loads its own Auth over the same files.
        let otp = Auth::load(dir.path().join("tokens.json")).start_pairing();
        let token = auth.redeem_pairing(&otp, Some("laptop")).unwrap();
        assert_eq!(auth.check_token(&token).as_deref(), Some("laptop"));
    }
}
