//! The mapaki folder plus its version history.
//!
//! Every file has its own git-style revision chain: each revision records its
//! content hash and its parent revision(s). A client uploading new content
//! says which revision it last saw (its `base`); comparing that against the
//! current head is what decides fast-forward vs. merge vs. conflict. The
//! global `seq` counter is only a change-feed cursor ("what moved since I
//! last looked") — it's assigned solely by the server, so it never needs to
//! carry causal meaning.
//!
//! Layout inside the mapaki dir:
//!   <name>                          working copies, browsable/editable by hand
//!   .mapaki-index.json              entries + seq
//!   .mapaki/history/<id>.json       revision list per file, oldest first
//!   .mapaki/objects/<id>/<sha256>   content of recent revisions (pruned)

use std::collections::{HashMap, HashSet, VecDeque};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use data_encoding::HEXLOWER;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use uuid::Uuid;

/// Revisions whose content is kept around for merges/restores. Older
/// revisions keep their metadata (ancestry still works) but lose their bytes.
const KEEP_REVISION_CONTENT: usize = 20;

/// Device label for changes noticed directly in the server's folder.
pub const SERVER_DEVICE: &str = "server";

/// A file changed by hand more recently than this is left for the next
/// rescan, so a copy still in progress isn't recorded half-written.
pub const SETTLE: std::time::Duration = std::time::Duration::from_secs(2);

/// What a rescan of the folder found.
#[derive(Debug, Default, Clone, Copy)]
pub struct Rescan {
    /// Something was recorded as a new revision, file, or delete.
    pub changed: bool,
    /// Some files were still being written and were skipped; rescan again after [`SETTLE`].
    pub unsettled: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    Image,
    Link,
    File,
}

pub fn kind_for_name(name: &str) -> Kind {
    let ext = Path::new(name)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    match ext.as_str() {
        "link" => Kind::Link,
        "png" | "jpg" | "jpeg" | "gif" | "webp" | "bmp" | "heic" | "heif" => Kind::Image,
        _ => Kind::File,
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Entry {
    pub id: Uuid,
    pub name: String,
    pub kind: Kind,
    pub size: u64,
    pub added_at: i64,
    pub position: i64,
    /// Current head revision id.
    #[serde(default)]
    pub head: String,
    /// sha256 (hex) of the head content.
    #[serde(default)]
    pub hash: String,
    /// Change-feed position of this entry's last change.
    #[serde(default)]
    pub seq: u64,
    /// Tombstone: kept so other devices learn about the delete via /changes.
    #[serde(default)]
    pub deleted: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Revision {
    pub id: String,
    pub parents: Vec<String>,
    pub hash: String,
    pub size: u64,
    pub device: String,
    pub at: i64,
    pub name: String,
    #[serde(default)]
    pub deleted: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PutResult {
    /// Content was identical to the head; nothing recorded.
    Unchanged,
    /// The uploader was based on the head (or forced); new revision on top.
    FastForward,
    /// The uploader was behind but a 3-way text merge applied cleanly. The
    /// stored content differs from what was uploaded — re-download it.
    Merged,
}

#[derive(Debug)]
pub enum StoreError {
    NotFound,
    Invalid(&'static str),
    /// The caller's base isn't the head and no clean merge exists. Carries
    /// the current (possibly tombstoned) entry so the client can show it.
    Conflict(Entry),
    Io(io::Error),
}

impl From<io::Error> for StoreError {
    fn from(e: io::Error) -> Self {
        StoreError::Io(e)
    }
}

impl std::fmt::Display for StoreError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            StoreError::NotFound => write!(f, "not found"),
            StoreError::Invalid(m) => write!(f, "{m}"),
            StoreError::Conflict(_) => write!(f, "conflict"),
            StoreError::Io(e) => write!(f, "io error: {e}"),
        }
    }
}

#[derive(Default, Serialize, Deserialize)]
struct Index {
    seq: u64,
    entries: Vec<Entry>,
}

/// The pre-versioning index was a bare array of entries.
#[derive(Deserialize)]
#[serde(untagged)]
enum IndexFile {
    Current(Index),
    Legacy(Vec<Entry>),
}

struct Inner {
    index: Index,
    /// (size, mtime nanos) of each working copy as last seen, so periodic
    /// rescans only re-hash files that actually changed on disk.
    stats: HashMap<Uuid, (u64, u128)>,
}

pub struct Store {
    dir: PathBuf,
    index_path: PathBuf,
    meta_dir: PathBuf,
    inner: std::sync::Mutex<Inner>,
    seq_tx: tokio::sync::watch::Sender<u64>,
}

fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

pub fn sha256_hex(bytes: &[u8]) -> String {
    HEXLOWER.encode(&Sha256::digest(bytes))
}

fn revision_id(hash: &str, parents: &[String], device: &str, at: i64) -> String {
    let mut hasher = Sha256::new();
    hasher.update(hash.as_bytes());
    for p in parents {
        hasher.update(p.as_bytes());
    }
    hasher.update(device.as_bytes());
    hasher.update(at.to_le_bytes());
    hasher.update(Uuid::new_v4().as_bytes()); // two identical edits in one second stay distinct
    HEXLOWER.encode(&hasher.finalize()[..16])
}

/// Only a bare filename, no separators or traversal. Returns an error message on rejection.
fn sanitize_name(name: &str) -> Result<String, &'static str> {
    let name = name.trim();
    if name.is_empty() || name == "." || name == ".." {
        return Err("invalid file name");
    }
    if name.contains('/') || name.contains('\\') || name.starts_with('.') {
        return Err("invalid file name");
    }
    Ok(name.to_string())
}

fn unique_name(dir: &Path, wanted: &str) -> String {
    let path = Path::new(wanted);
    let stem = path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("file");
    let ext = path.extension().and_then(|e| e.to_str());
    let mut candidate = wanted.to_string();
    let mut n = 2;
    while dir.join(&candidate).exists() {
        candidate = match ext {
            Some(ext) => format!("{stem}-{n}.{ext}"),
            None => format!("{stem}-{n}"),
        };
        n += 1;
    }
    candidate
}

fn write_atomic(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let tmp = path.with_extension("mapaki-tmp");
    fs::write(&tmp, bytes)?;
    fs::rename(&tmp, path)
}

pub(crate) fn write_json_atomic<T: Serialize>(path: &Path, value: &T) -> io::Result<()> {
    write_atomic(path, &serde_json::to_vec_pretty(value).unwrap())
}

fn file_stat(path: &Path) -> Option<(u64, u128)> {
    let meta = fs::metadata(path).ok()?;
    if !meta.is_file() {
        return None;
    }
    let mtime = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    Some((meta.len(), mtime))
}

/// Text that's safe to line-merge: valid UTF-8 without NUL bytes.
fn as_text(bytes: &[u8]) -> Option<&str> {
    if bytes.contains(&0) {
        return None;
    }
    std::str::from_utf8(bytes).ok()
}

impl Store {
    pub fn load(dir: PathBuf) -> io::Result<Self> {
        fs::create_dir_all(&dir)?;
        let meta_dir = dir.join(".mapaki");
        fs::create_dir_all(meta_dir.join("history"))?;
        fs::create_dir_all(meta_dir.join("objects"))?;
        let index_path = dir.join(".mapaki-index.json");

        let index = match fs::read(&index_path) {
            Ok(bytes) => match serde_json::from_slice::<IndexFile>(&bytes) {
                Ok(IndexFile::Current(index)) => index,
                Ok(IndexFile::Legacy(entries)) => Index { seq: 0, entries },
                Err(_) => Index::default(),
            },
            Err(_) => Index::default(),
        };

        let (seq_tx, _) = tokio::sync::watch::channel(index.seq);
        let store = Store {
            dir,
            index_path,
            meta_dir,
            inner: std::sync::Mutex::new(Inner {
                index,
                stats: HashMap::new(),
            }),
            seq_tx,
        };
        store.rescan(true)?;
        Ok(store)
    }

    /// Live entries in display order, read straight from the index without
    /// loading (or rescanning) the store; safe while the daemon is running.
    pub fn read_entries(dir: &Path) -> Vec<Entry> {
        let mut entries = match fs::read(dir.join(".mapaki-index.json")).ok().and_then(|b| serde_json::from_slice(&b).ok()) {
            Some(IndexFile::Current(index)) => index.entries,
            Some(IndexFile::Legacy(entries)) => entries,
            None => vec![],
        };
        entries.retain(|e| !e.deleted);
        entries.sort_by_key(|e| e.position);
        entries
    }

    pub fn subscribe(&self) -> tokio::sync::watch::Receiver<u64> {
        self.seq_tx.subscribe()
    }

    #[cfg(test)]
    pub fn seq(&self) -> u64 {
        self.inner.lock().unwrap().index.seq
    }

    fn history_path(&self, id: Uuid) -> PathBuf {
        self.meta_dir.join("history").join(format!("{id}.json"))
    }

    fn object_path(&self, id: Uuid, hash: &str) -> PathBuf {
        self.meta_dir.join("objects").join(id.to_string()).join(hash)
    }

    pub fn history(&self, id: Uuid) -> Vec<Revision> {
        fs::read(self.history_path(id))
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default()
    }

    fn save_history(&self, id: Uuid, history: &[Revision]) -> io::Result<()> {
        write_json_atomic(&self.history_path(id), &history)
    }

    fn save(&self, inner: &Inner) -> io::Result<()> {
        write_json_atomic(&self.index_path, &inner.index)
    }

    fn bump(inner: &mut Inner) -> u64 {
        inner.index.seq += 1;
        inner.index.seq
    }

    fn publish(&self, inner: &Inner) {
        self.seq_tx.send_replace(inner.index.seq);
    }

    /// Appends a revision to a file's history, stores its content (unless
    /// it's a tombstone) and prunes content older than the retention window.
    fn record_revision(
        &self,
        id: Uuid,
        parents: Vec<String>,
        bytes: Option<&[u8]>,
        hash: &str,
        name: &str,
        device: &str,
    ) -> io::Result<Revision> {
        let at = now();
        let rev = Revision {
            id: revision_id(hash, &parents, device, at),
            parents,
            hash: hash.to_string(),
            size: bytes.map(|b| b.len() as u64).unwrap_or(0),
            device: device.to_string(),
            at,
            name: name.to_string(),
            deleted: bytes.is_none(),
        };
        if let Some(bytes) = bytes {
            let path = self.object_path(id, hash);
            if !path.exists() {
                fs::create_dir_all(path.parent().unwrap())?;
                write_atomic(&path, bytes)?;
            }
        }
        let mut history = self.history(id);
        history.push(rev.clone());
        self.save_history(id, &history)?;
        self.prune_objects(id, &history);
        Ok(rev)
    }

    fn prune_objects(&self, id: Uuid, history: &[Revision]) {
        if history.len() <= KEEP_REVISION_CONTENT {
            return;
        }
        let keep: HashSet<&str> = history
            .iter()
            .rev()
            .take(KEEP_REVISION_CONTENT)
            .map(|r| r.hash.as_str())
            .collect();
        let dir = self.meta_dir.join("objects").join(id.to_string());
        if let Ok(read_dir) = fs::read_dir(&dir) {
            for e in read_dir.flatten() {
                let name = e.file_name().to_string_lossy().to_string();
                if !keep.contains(name.as_str()) {
                    let _ = fs::remove_file(e.path());
                }
            }
        }
    }

    /// True when `ancestor` is reachable from `from` by following parents.
    fn is_ancestor(history: &[Revision], ancestor: &str, from: &str) -> bool {
        let by_id: HashMap<&str, &Revision> = history.iter().map(|r| (r.id.as_str(), r)).collect();
        let mut queue: VecDeque<&str> = VecDeque::from([from]);
        let mut seen = HashSet::new();
        while let Some(cur) = queue.pop_front() {
            if cur == ancestor {
                return true;
            }
            if !seen.insert(cur) {
                continue;
            }
            if let Some(rev) = by_id.get(cur) {
                queue.extend(rev.parents.iter().map(|p| p.as_str()));
            }
        }
        false
    }

    /// Reconciles the index with the working folder: adopts files dropped in
    /// by hand, records hand edits as new revisions, and tombstones files that
    /// vanished. `full` re-hashes everything instead of trusting size+mtime.
    pub fn rescan(&self, full: bool) -> io::Result<Rescan> {
        let mut inner = self.inner.lock().unwrap();
        let mut changed = false;
        let mut unsettled = false;
        let settled_before = SystemTime::now()
            .checked_sub(SETTLE)
            .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
            .map_or(0, |d| d.as_nanos());

        for i in 0..inner.index.entries.len() {
            let entry = inner.index.entries[i].clone();
            if entry.deleted {
                continue;
            }
            let path = self.dir.join(&entry.name);
            let Some(stat) = file_stat(&path) else {
                let rev = self.record_revision(entry.id, vec![entry.head.clone()], None, &entry.hash, &entry.name, SERVER_DEVICE)?;
                let seq = Self::bump(&mut inner);
                let e = &mut inner.index.entries[i];
                e.deleted = true;
                e.head = rev.id;
                e.seq = seq;
                inner.stats.remove(&entry.id);
                changed = true;
                continue;
            };
            if !full && inner.stats.get(&entry.id) == Some(&stat) && !entry.head.is_empty() {
                continue;
            }
            if !full && stat.1 > settled_before {
                unsettled = true;
                continue;
            }
            let bytes = fs::read(&path)?;
            let hash = sha256_hex(&bytes);
            inner.stats.insert(entry.id, stat);
            if hash == entry.hash && !entry.head.is_empty() {
                continue;
            }
            let parents = if entry.head.is_empty() { vec![] } else { vec![entry.head.clone()] };
            let rev = self.record_revision(entry.id, parents, Some(&bytes), &hash, &entry.name, SERVER_DEVICE)?;
            let seq = Self::bump(&mut inner);
            let e = &mut inner.index.entries[i];
            e.head = rev.id;
            e.hash = hash;
            e.size = bytes.len() as u64;
            e.seq = seq;
            changed = true;
        }

        let known: HashSet<String> = inner
            .index
            .entries
            .iter()
            .filter(|e| !e.deleted)
            .map(|e| e.name.clone())
            .collect();
        let mut next_position = inner.index.entries.iter().map(|e| e.position).max().unwrap_or(-1) + 1;
        let mut found: Vec<String> = fs::read_dir(&self.dir)?
            .flatten()
            .map(|e| e.file_name().to_string_lossy().to_string())
            .filter(|n| !n.starts_with('.') && !n.ends_with(".mapaki-tmp") && !known.contains(n))
            .filter(|n| self.dir.join(n).is_file())
            .collect();
        found.sort();
        for name in found {
            let path = self.dir.join(&name);
            if !full && file_stat(&path).is_some_and(|(_, mtime)| mtime > settled_before) {
                unsettled = true;
                continue;
            }
            let bytes = fs::read(&path)?;
            let hash = sha256_hex(&bytes);
            let id = Uuid::new_v4();
            let rev = self.record_revision(id, vec![], Some(&bytes), &hash, &name, SERVER_DEVICE)?;
            let seq = Self::bump(&mut inner);
            if let Some(stat) = file_stat(&path) {
                inner.stats.insert(id, stat);
            }
            inner.index.entries.push(Entry {
                id,
                kind: kind_for_name(&name),
                name,
                size: bytes.len() as u64,
                added_at: now(),
                position: next_position,
                head: rev.id,
                hash,
                seq,
                deleted: false,
            });
            next_position += 1;
            changed = true;
        }

        self.save(&inner)?;
        if changed {
            self.publish(&inner);
        }
        Ok(Rescan { changed, unsettled })
    }

    /// Live (non-deleted) entries in display order.
    pub fn list(&self) -> Vec<Entry> {
        let inner = self.inner.lock().unwrap();
        let mut entries: Vec<Entry> = inner.index.entries.iter().filter(|e| !e.deleted).cloned().collect();
        entries.sort_by_key(|e| e.position);
        entries
    }

    /// Current seq plus every entry (tombstones included) changed after `since`.
    pub fn changes(&self, since: u64) -> (u64, Vec<Entry>) {
        let inner = self.inner.lock().unwrap();
        let mut entries: Vec<Entry> = inner.index.entries.iter().filter(|e| e.seq > since).cloned().collect();
        entries.sort_by_key(|e| e.position);
        (inner.index.seq, entries)
    }

    pub fn get(&self, id: Uuid) -> Option<Entry> {
        self.inner.lock().unwrap().index.entries.iter().find(|e| e.id == id).cloned()
    }

    pub fn content_path(&self, id: Uuid) -> Option<(PathBuf, String)> {
        let inner = self.inner.lock().unwrap();
        inner
            .index
            .entries
            .iter()
            .find(|e| e.id == id && !e.deleted)
            .map(|e| (self.dir.join(&e.name), e.name.clone()))
    }

    /// Path to a past revision's content, if it's still retained.
    pub fn revision_content_path(&self, id: Uuid, rev: &str) -> Option<(PathBuf, String)> {
        let history = self.history(id);
        let r = history.iter().find(|r| r.id == rev && !r.deleted)?;
        let path = self.object_path(id, &r.hash);
        path.exists().then(|| (path, r.name.clone()))
    }

    pub fn insert_new(&self, wanted_name: &str, bytes: &[u8], device: &str) -> Result<Entry, StoreError> {
        let wanted_name = sanitize_name(wanted_name).map_err(StoreError::Invalid)?;
        let mut inner = self.inner.lock().unwrap();
        let name = unique_name(&self.dir, &wanted_name);
        let path = self.dir.join(&name);
        write_atomic(&path, bytes)?;

        let id = Uuid::new_v4();
        let hash = sha256_hex(bytes);
        let rev = self.record_revision(id, vec![], Some(bytes), &hash, &name, device)?;
        let seq = Self::bump(&mut inner);
        if let Some(stat) = file_stat(&path) {
            inner.stats.insert(id, stat);
        }
        for e in inner.index.entries.iter_mut() {
            e.position += 1;
        }
        let entry = Entry {
            id,
            kind: kind_for_name(&name),
            name,
            size: bytes.len() as u64,
            added_at: now(),
            position: 0,
            head: rev.id,
            hash,
            seq,
            deleted: false,
        };
        inner.index.entries.push(entry.clone());
        self.save(&inner)?;
        self.publish(&inner);
        Ok(entry)
    }

    /// Uploads new content for an existing file. `base` is the revision the
    /// uploader last synced; `force` overwrites regardless ("keep local").
    pub fn put_content(
        &self,
        id: Uuid,
        bytes: &[u8],
        base: Option<&str>,
        device: &str,
        force: bool,
    ) -> Result<(PutResult, Entry), StoreError> {
        let mut inner = self.inner.lock().unwrap();
        let idx = inner.index.entries.iter().position(|e| e.id == id).ok_or(StoreError::NotFound)?;
        let entry = inner.index.entries[idx].clone();
        let hash = sha256_hex(bytes);

        if entry.deleted && !force {
            return Err(StoreError::Conflict(entry));
        }
        if !entry.deleted && hash == entry.hash {
            return Ok((PutResult::Unchanged, entry));
        }

        let history = self.history(id);
        let (result, final_bytes, parents_for_head): (PutResult, Vec<u8>, Vec<String>) =
            if force || base == Some(entry.head.as_str()) {
                (PutResult::FastForward, bytes.to_vec(), vec![entry.head.clone()])
            } else {
                let base = base.ok_or_else(|| StoreError::Conflict(entry.clone()))?;
                if !Self::is_ancestor(&history, base, &entry.head) {
                    return Err(StoreError::Conflict(entry));
                }
                let merged = self.try_merge(id, &history, base, &entry, bytes).ok_or_else(|| StoreError::Conflict(entry.clone()))?;
                // Keep the uploader's version in history as its own revision
                // branching from its base, then a merge revision joining it
                // with the head — the same shape git would record.
                let theirs = self.record_revision(id, vec![base.to_string()], Some(bytes), &hash, &entry.name, device)?;
                (PutResult::Merged, merged, vec![entry.head.clone(), theirs.id])
            };

        // Restoring a deleted file: its old name may have been taken since.
        let name = if entry.deleted { unique_name(&self.dir, &entry.name) } else { entry.name.clone() };
        let path = self.dir.join(&name);
        write_atomic(&path, &final_bytes)?;
        let final_hash = sha256_hex(&final_bytes);
        let rev = self.record_revision(id, parents_for_head, Some(&final_bytes), &final_hash, &name, device)?;
        let seq = Self::bump(&mut inner);
        if let Some(stat) = file_stat(&path) {
            inner.stats.insert(id, stat);
        }
        let e = &mut inner.index.entries[idx];
        e.name = name;
        e.head = rev.id;
        e.hash = final_hash;
        e.size = final_bytes.len() as u64;
        e.seq = seq;
        e.deleted = false;
        let updated = e.clone();
        self.save(&inner)?;
        self.publish(&inner);
        Ok((result, updated))
    }

    /// 3-way line merge of text files, using the uploader's base as the
    /// common ancestor. None when not text, base content was pruned, or the
    /// edits overlap.
    fn try_merge(&self, id: Uuid, history: &[Revision], base: &str, head: &Entry, ours: &[u8]) -> Option<Vec<u8>> {
        if head.kind == Kind::Image {
            return None;
        }
        let base_rev = history.iter().find(|r| r.id == base && !r.deleted)?;
        let base_bytes = fs::read(self.object_path(id, &base_rev.hash)).ok()?;
        let head_bytes = fs::read(self.dir.join(&head.name)).ok()?;
        let merged = diffy::merge(as_text(&base_bytes)?, as_text(&head_bytes)?, as_text(ours)?).ok()?;
        Some(merged.into_bytes())
    }

    pub fn rename(&self, id: Uuid, new_name: &str) -> Result<Entry, StoreError> {
        let new_name = sanitize_name(new_name).map_err(StoreError::Invalid)?;
        let mut inner = self.inner.lock().unwrap();
        let idx = inner
            .index
            .entries
            .iter()
            .position(|e| e.id == id && !e.deleted)
            .ok_or(StoreError::NotFound)?;
        let old_name = inner.index.entries[idx].name.clone();
        if new_name != old_name {
            let final_name = unique_name(&self.dir, &new_name);
            fs::rename(self.dir.join(&old_name), self.dir.join(&final_name))?;
            let seq = Self::bump(&mut inner);
            let e = &mut inner.index.entries[idx];
            e.name = final_name;
            e.kind = kind_for_name(&e.name);
            e.seq = seq;
        }
        let entry = inner.index.entries[idx].clone();
        self.save(&inner)?;
        self.publish(&inner);
        Ok(entry)
    }

    /// Deletes a file, leaving a tombstone. With a `base`, refuses if the file
    /// changed since that revision (someone edited what you're deleting).
    pub fn delete(&self, id: Uuid, base: Option<&str>, device: &str) -> Result<Entry, StoreError> {
        let mut inner = self.inner.lock().unwrap();
        let idx = inner.index.entries.iter().position(|e| e.id == id).ok_or(StoreError::NotFound)?;
        let entry = inner.index.entries[idx].clone();
        if entry.deleted {
            return Ok(entry);
        }
        if let Some(base) = base {
            if base != entry.head {
                return Err(StoreError::Conflict(entry));
            }
        }
        let rev = self.record_revision(id, vec![entry.head.clone()], None, &entry.hash, &entry.name, device)?;
        let _ = fs::remove_file(self.dir.join(&entry.name));
        let seq = Self::bump(&mut inner);
        inner.stats.remove(&id);
        let e = &mut inner.index.entries[idx];
        e.deleted = true;
        e.head = rev.id;
        e.seq = seq;
        let updated = e.clone();
        self.save(&inner)?;
        self.publish(&inner);
        Ok(updated)
    }

    /// Rewrites positions 0..n from the given id order. Any known ids missing
    /// from `ids` are kept, appended after, in their previous relative order.
    pub fn set_order(&self, ids: &[Uuid]) -> Vec<Entry> {
        let mut inner = self.inner.lock().unwrap();
        let mut entries = std::mem::take(&mut inner.index.entries);
        entries.sort_by_key(|e| e.position);
        let mut ordered: Vec<Entry> = Vec::with_capacity(entries.len());
        for id in ids {
            if let Some(pos) = entries.iter().position(|e| e.id == *id) {
                ordered.push(entries.remove(pos));
            }
        }
        ordered.extend(entries.drain(..));
        let seq = Self::bump(&mut inner);
        for (i, e) in ordered.iter_mut().enumerate() {
            if e.position != i as i64 {
                e.position = i as i64;
                e.seq = seq;
            }
        }
        inner.index.entries = ordered;
        let _ = self.save(&inner);
        self.publish(&inner);
        drop(inner);
        self.list()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_store() -> (Store, tempdir::TempDir) {
        let dir = tempdir::TempDir::new("mapaki-test").unwrap();
        let store = Store::load(dir.path().to_path_buf()).unwrap();
        (store, dir)
    }

    #[test]
    fn new_files_pile_up_at_the_top() {
        let (store, _dir) = temp_store();
        let a = store.insert_new("a.link", b"a", "t").unwrap();
        let b = store.insert_new("b.link", b"b", "t").unwrap();
        let list = store.list();
        assert_eq!(list[0].id, b.id);
        assert_eq!(list[1].id, a.id);
        assert_eq!(list[0].position, 0);
        assert_eq!(list[1].position, 1);
    }

    #[test]
    fn kind_is_inferred_from_extension() {
        let (store, _dir) = temp_store();
        let img = store.insert_new("shot.png", b"x", "t").unwrap();
        let link = store.insert_new("site.link", b"x", "t").unwrap();
        let other = store.insert_new("notes.txt", b"x", "t").unwrap();
        assert_eq!(img.kind, Kind::Image);
        assert_eq!(link.kind, Kind::Link);
        assert_eq!(other.kind, Kind::File);
    }

    #[test]
    fn duplicate_names_are_deduped() {
        let (store, _dir) = temp_store();
        let first = store.insert_new("dup.link", b"1", "t").unwrap();
        let second = store.insert_new("dup.link", b"2", "t").unwrap();
        assert_eq!(first.name, "dup.link");
        assert_eq!(second.name, "dup-2.link");
    }

    #[test]
    fn set_order_rewrites_positions_and_keeps_missing_ids() {
        let (store, _dir) = temp_store();
        let a = store.insert_new("a.link", b"a", "t").unwrap();
        let b = store.insert_new("b.link", b"b", "t").unwrap();
        let c = store.insert_new("c.link", b"c", "t").unwrap();
        // Only reorder a subset; c (unmentioned) should land at the end.
        let result = store.set_order(&[a.id, b.id]);
        assert_eq!(result.iter().map(|e| e.id).collect::<Vec<_>>(), vec![a.id, b.id, c.id]);
        assert_eq!(result[0].position, 0);
        assert_eq!(result[1].position, 1);
        assert_eq!(result[2].position, 2);
    }

    #[test]
    fn delete_tombstones_entry_and_removes_file() {
        let (store, dir) = temp_store();
        let a = store.insert_new("a.link", b"a", "t").unwrap();
        store.delete(a.id, None, "t").unwrap();
        assert!(store.list().is_empty());
        assert!(!dir.path().join("a.link").exists());
        let (_, changes) = store.changes(a.seq);
        assert!(changes.iter().any(|e| e.id == a.id && e.deleted));
    }

    #[test]
    fn path_traversal_names_are_rejected() {
        let (store, _dir) = temp_store();
        assert!(store.insert_new("../evil", b"x", "t").is_err());
        assert!(store.insert_new(".hidden", b"x", "t").is_err());
    }

    #[test]
    fn upload_based_on_head_fast_forwards() {
        let (store, dir) = temp_store();
        let a = store.insert_new("a.txt", b"one", "t").unwrap();
        let (result, e) = store.put_content(a.id, b"two", Some(&a.head), "t", false).unwrap();
        assert_eq!(result, PutResult::FastForward);
        assert_ne!(e.head, a.head);
        assert_eq!(fs::read(dir.path().join("a.txt")).unwrap(), b"two");
        assert_eq!(store.history(a.id)[1].parents, vec![a.head.clone()]);
    }

    #[test]
    fn identical_content_is_unchanged_even_from_a_stale_base() {
        let (store, _dir) = temp_store();
        let a = store.insert_new("a.txt", b"one", "t").unwrap();
        let (_, b) = store.put_content(a.id, b"two", Some(&a.head), "t", false).unwrap();
        let (result, _) = store.put_content(a.id, b"two", Some(&a.head), "u", false).unwrap();
        assert_eq!(result, PutResult::Unchanged);
        assert_eq!(store.get(a.id).unwrap().head, b.head);
    }

    #[test]
    fn stale_binary_upload_conflicts() {
        let (store, _dir) = temp_store();
        let a = store.insert_new("a.png", b"\x89PNG one", "t").unwrap();
        store.put_content(a.id, b"\x89PNG two", Some(&a.head), "t", false).unwrap();
        let err = store.put_content(a.id, b"\x89PNG three", Some(&a.head), "u", false).unwrap_err();
        assert!(matches!(err, StoreError::Conflict(_)));
    }

    #[test]
    fn stale_text_upload_merges_when_edits_do_not_overlap() {
        let (store, dir) = temp_store();
        let a = store.insert_new("n.txt", b"alpha\nbeta\ngamma\n", "t").unwrap();
        store.put_content(a.id, b"ALPHA\nbeta\ngamma\n", Some(&a.head), "phone", false).unwrap();
        let (result, e) = store.put_content(a.id, b"alpha\nbeta\nGAMMA\n", Some(&a.head), "laptop", false).unwrap();
        assert_eq!(result, PutResult::Merged);
        assert_eq!(fs::read(dir.path().join("n.txt")).unwrap(), b"ALPHA\nbeta\nGAMMA\n");
        let history = store.history(a.id);
        let head = history.iter().find(|r| r.id == e.head).unwrap();
        assert_eq!(head.parents.len(), 2);
    }

    #[test]
    fn stale_text_upload_with_overlapping_edits_conflicts() {
        let (store, _dir) = temp_store();
        let a = store.insert_new("n.txt", b"alpha\n", "t").unwrap();
        store.put_content(a.id, b"phone\n", Some(&a.head), "phone", false).unwrap();
        let err = store.put_content(a.id, b"laptop\n", Some(&a.head), "laptop", false).unwrap_err();
        assert!(matches!(err, StoreError::Conflict(_)));
    }

    #[test]
    fn unknown_or_missing_base_conflicts_and_force_overwrites() {
        let (store, dir) = temp_store();
        let a = store.insert_new("a.txt", b"one", "t").unwrap();
        assert!(matches!(store.put_content(a.id, b"x", Some("nope"), "t", false), Err(StoreError::Conflict(_))));
        assert!(matches!(store.put_content(a.id, b"x", None, "t", false), Err(StoreError::Conflict(_))));
        let (result, _) = store.put_content(a.id, b"x", None, "t", true).unwrap();
        assert_eq!(result, PutResult::FastForward);
        assert_eq!(fs::read(dir.path().join("a.txt")).unwrap(), b"x");
    }

    #[test]
    fn delete_with_stale_base_conflicts() {
        let (store, _dir) = temp_store();
        let a = store.insert_new("a.txt", b"one", "t").unwrap();
        store.put_content(a.id, b"two", Some(&a.head), "t", false).unwrap();
        assert!(matches!(store.delete(a.id, Some(&a.head), "u"), Err(StoreError::Conflict(_))));
    }

    #[test]
    fn changes_only_returns_entries_after_cursor() {
        let (store, _dir) = temp_store();
        let a = store.insert_new("a.txt", b"a", "t").unwrap();
        let cursor = store.seq();
        let b = store.insert_new("b.txt", b"b", "t").unwrap();
        let (seq, changes) = store.changes(cursor);
        assert_eq!(seq, b.seq);
        assert_eq!(changes.iter().map(|e| e.id).collect::<Vec<_>>(), vec![b.id]);
        assert!(!changes.iter().any(|e| e.id == a.id));
    }

    #[test]
    fn hand_edits_and_drops_in_the_folder_are_picked_up() {
        let (store, dir) = temp_store();
        let a = store.insert_new("a.txt", b"one", "t").unwrap();
        write_settled(&dir.path().join("a.txt"), b"edited by hand, longer");
        write_settled(&dir.path().join("dropped.txt"), b"new");
        assert!(store.rescan(false).unwrap().changed);
        let e = store.get(a.id).unwrap();
        assert_ne!(e.head, a.head);
        assert_eq!(store.history(a.id).last().unwrap().device, SERVER_DEVICE);
        assert!(store.list().iter().any(|e| e.name == "dropped.txt"));
    }

    /// Writes a file and backdates it past [`SETTLE`], like a copy that finished a while ago.
    fn write_settled(path: &Path, bytes: &[u8]) {
        fs::write(path, bytes).unwrap();
        let past = SystemTime::now() - SETTLE * 2;
        fs::File::options().write(true).open(path).unwrap().set_modified(past).unwrap();
    }

    #[test]
    fn files_still_being_written_wait_for_the_next_rescan() {
        let (store, dir) = temp_store();
        let a = store.insert_new("a.txt", b"one", "t").unwrap();
        fs::write(dir.path().join("a.txt"), b"half of it").unwrap();
        fs::write(dir.path().join("copying.bin"), b"partial").unwrap();
        let r = store.rescan(false).unwrap();
        assert!(!r.changed && r.unsettled);
        assert_eq!(store.get(a.id).unwrap().head, a.head);
        assert!(!store.list().iter().any(|e| e.name == "copying.bin"));

        write_settled(&dir.path().join("a.txt"), b"all of it");
        write_settled(&dir.path().join("copying.bin"), b"complete");
        let r = store.rescan(false).unwrap();
        assert!(r.changed && !r.unsettled);
        assert_ne!(store.get(a.id).unwrap().head, a.head);
        assert!(store.list().iter().any(|e| e.name == "copying.bin"));
    }

    #[test]
    fn legacy_index_is_migrated_with_initial_revisions() {
        let dir = tempdir::TempDir::new("mapaki-legacy").unwrap();
        fs::write(dir.path().join("old.txt"), b"old").unwrap();
        let legacy = serde_json::json!([{
            "id": Uuid::new_v4(), "name": "old.txt", "kind": "file",
            "size": 3, "added_at": 0, "position": 0
        }]);
        fs::write(dir.path().join(".mapaki-index.json"), legacy.to_string()).unwrap();
        let store = Store::load(dir.path().to_path_buf()).unwrap();
        let list = store.list();
        assert_eq!(list.len(), 1);
        assert!(!list[0].head.is_empty());
        assert_eq!(list[0].hash, sha256_hex(b"old"));
    }

    #[test]
    fn force_restoring_a_deleted_file_never_clobbers_its_old_name() {
        let (store, dir) = temp_store();
        let a = store.insert_new("a.txt", b"old", "t").unwrap();
        store.delete(a.id, None, "t").unwrap();
        store.insert_new("a.txt", b"newcomer", "t").unwrap();
        let (_, restored) = store.put_content(a.id, b"restored", None, "t", true).unwrap();
        assert_eq!(restored.name, "a-2.txt");
        assert_eq!(fs::read(dir.path().join("a.txt")).unwrap(), b"newcomer");
    }

    #[test]
    fn old_revision_content_stays_fetchable() {
        let (store, _dir) = temp_store();
        let a = store.insert_new("a.txt", b"one", "t").unwrap();
        store.put_content(a.id, b"two", Some(&a.head), "t", false).unwrap();
        let (path, _) = store.revision_content_path(a.id, &a.head).unwrap();
        assert_eq!(fs::read(path).unwrap(), b"one");
    }
}
