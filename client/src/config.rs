use std::collections::HashMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// One mapaki store, reachable at one or more URLs (e.g. the LAN address
/// and a public fallback). URLs are tried in order; the first that answers
/// /health wins, so put the LAN address first.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    pub urls: Vec<String>,
    pub token: String,
    pub folder: PathBuf,
}

/// What this device last agreed with the server about, per file id.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct FileState {
    pub name: String,
    /// Server revision our local copy is based on.
    pub base: String,
    /// sha256 of the local copy when it was last in sync.
    pub hash: String,
    pub size: u64,
    pub mtime: u128,
}

/// A conflict waiting on the user (background mode can't prompt).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Conflict {
    /// Server file id; None when an untracked local file collided by name.
    pub id: Option<String>,
    pub name: String,
    pub remote_deleted: bool,
    pub detected_at: i64,
}

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct State {
    pub seq: u64,
    pub files: HashMap<String, FileState>,
    /// Keyed by local file name.
    #[serde(default)]
    pub conflicts: HashMap<String, Conflict>,
}

pub fn config_dir() -> PathBuf {
    dirs::config_dir().unwrap_or_else(|| PathBuf::from(".")).join("mapaki")
}

pub fn data_dir() -> PathBuf {
    dirs::data_local_dir().unwrap_or_else(|| PathBuf::from(".")).join("mapaki")
}

pub fn trash_dir() -> PathBuf {
    data_dir().join("trash")
}

pub fn default_folder() -> PathBuf {
    dirs::home_dir().unwrap_or_else(|| PathBuf::from(".")).join("Mapaki")
}

fn run_lock_path() -> PathBuf {
    data_dir().join("run.lock")
}

/// Held by `mapaki run` for as long as it's running (the OS drops it if the
/// process dies), so other commands know a background sync is active.
pub fn take_run_lock() -> io::Result<Option<fs::File>> {
    fs::create_dir_all(data_dir())?;
    let file = fs::File::create(run_lock_path())?;
    match file.try_lock() {
        Ok(()) => Ok(Some(file)),
        Err(fs::TryLockError::WouldBlock) => Ok(None),
        Err(fs::TryLockError::Error(e)) => Err(e),
    }
}

/// Whether `mapaki run` is active, in which case it syncs folder edits itself.
pub fn service_running() -> bool {
    let Ok(file) = fs::OpenOptions::new().write(true).open(run_lock_path()) else { return false };
    matches!(file.try_lock(), Err(fs::TryLockError::WouldBlock))
}

fn config_path() -> PathBuf {
    config_dir().join("config.json")
}

fn state_path() -> PathBuf {
    data_dir().join("state.json")
}

fn write_atomic(path: &Path, bytes: &[u8]) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let tmp = path.with_extension("tmp");
    fs::write(&tmp, bytes)?;
    fs::rename(&tmp, path)
}

impl Config {
    pub fn load() -> Option<Config> {
        let bytes = fs::read(config_path()).ok()?;
        serde_json::from_slice(&bytes).ok()
    }

    pub fn save(&self) -> io::Result<()> {
        let path = config_path();
        write_atomic(&path, &serde_json::to_vec_pretty(self).unwrap())?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&path, fs::Permissions::from_mode(0o600))?;
        }
        Ok(())
    }
}

impl State {
    pub fn load() -> State {
        fs::read(state_path())
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default()
    }

    pub fn save(&self) -> io::Result<()> {
        write_atomic(&state_path(), &serde_json::to_vec_pretty(self).unwrap())
    }

    pub fn reset() -> io::Result<()> {
        match fs::remove_file(state_path()) {
            Err(e) if e.kind() != io::ErrorKind::NotFound => Err(e),
            _ => Ok(()),
        }
    }
}
