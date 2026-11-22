//! One full reconcile pass between the local folder and the server.
//!
//! For each file this device has synced before, `State` remembers the server
//! revision our copy was based on and the hash it had. Comparing that against
//! the folder (did *we* change it?) and the server's head (did *they*?) gives
//! four cases; when both sides changed, the upload carries our base revision
//! and the server decides: fast-forward, clean text merge, or a conflict the
//! user resolves as keep local / keep remote / keep both.

use std::collections::{HashMap, HashSet};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use data_encoding::HEXLOWER;
use sha2::{Digest, Sha256};

use crate::api::{Api, Entry, Outcome, PutResult};
use crate::config::{trash_dir, Conflict, FileState, State};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Choice {
    Local,
    Remote,
    Both,
}

#[derive(Debug, Default)]
pub struct Report {
    pub uploaded: Vec<String>,
    pub downloaded: Vec<String>,
    pub merged: Vec<String>,
    pub deleted_local: Vec<String>,
    pub deleted_remote: Vec<String>,
    pub conflicts: Vec<String>,
    pub errors: Vec<String>,
}

impl Report {
    pub fn changed_anything(&self) -> bool {
        !(self.uploaded.is_empty()
            && self.downloaded.is_empty()
            && self.merged.is_empty()
            && self.deleted_local.is_empty()
            && self.deleted_remote.is_empty())
    }

    pub fn summary(&self) -> String {
        let mut parts = Vec::new();
        let mut push = |n: usize, what: &str| {
            if n > 0 {
                parts.push(format!("{n} {what}"));
            }
        };
        push(self.uploaded.len(), "uploaded");
        push(self.downloaded.len(), "downloaded");
        push(self.merged.len(), "merged");
        push(self.deleted_local.len(), "removed locally");
        push(self.deleted_remote.len(), "removed on server");
        push(self.conflicts.len(), "conflicts");
        push(self.errors.len(), "errors");
        if parts.is_empty() {
            "up to date".into()
        } else {
            parts.join(", ")
        }
    }
}

#[derive(Clone, Copy)]
struct Stat {
    size: u64,
    mtime: u128,
}

pub type BoxError = Box<dyn std::error::Error>;

/// Files never synced: hidden files, our own temp files, editor/browser
/// scratch files that would otherwise churn revisions.
pub fn is_synced_name(name: &str) -> bool {
    !(name.starts_with('.')
        || name.ends_with('~')
        || name.ends_with(".mapaki-tmp")
        || name.ends_with(".swp")
        || name.ends_with(".part")
        || name.ends_with(".crdownload"))
}

pub fn sha256_hex(bytes: &[u8]) -> String {
    HEXLOWER.encode(&Sha256::digest(bytes))
}

fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

fn stat(path: &Path) -> Option<Stat> {
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
    Some(Stat { size: meta.len(), mtime })
}

fn scan(folder: &Path) -> io::Result<HashMap<String, Stat>> {
    let mut out = HashMap::new();
    for e in fs::read_dir(folder)?.flatten() {
        let Ok(name) = e.file_name().into_string() else { continue };
        if !is_synced_name(&name) {
            continue;
        }
        if let Some(s) = stat(&e.path()) {
            out.insert(name, s);
        }
    }
    Ok(out)
}

/// `notes.txt` -> `notes (conflict copy).txt`, `-2`, `-3`... if taken.
pub fn conflict_copy_name(folder: &Path, name: &str) -> String {
    let p = Path::new(name);
    let stem = p.file_stem().and_then(|s| s.to_str()).unwrap_or(name);
    let ext = p.extension().and_then(|e| e.to_str());
    let make = |n: u32| {
        let suffix = if n == 1 { " (conflict copy)".to_string() } else { format!(" (conflict copy {n})") };
        match ext {
            Some(ext) => format!("{stem}{suffix}.{ext}"),
            None => format!("{stem}{suffix}"),
        }
    };
    let mut n = 1;
    while folder.join(make(n)).exists() {
        n += 1;
    }
    make(n)
}

pub struct Syncer<'a> {
    api: &'a Api,
    folder: PathBuf,
    state: &'a mut State,
    /// Asked on conflict; None defers it (recorded for `mapaki resolve`).
    decide: &'a mut dyn FnMut(&str, &Entry) -> Option<Choice>,
    pub report: Report,
}

impl<'a> Syncer<'a> {
    pub fn new(
        api: &'a Api,
        folder: &Path,
        state: &'a mut State,
        decide: &'a mut dyn FnMut(&str, &Entry) -> Option<Choice>,
    ) -> Self {
        Syncer {
            api,
            folder: folder.to_path_buf(),
            state,
            decide,
            report: Report::default(),
        }
    }

    pub fn run(mut self) -> Result<Report, BoxError> {
        fs::create_dir_all(&self.folder)?;
        let changes = self.api.changes(0)?;
        let remote: HashMap<String, Entry> = changes.entries.into_iter().map(|e| (e.id.clone(), e)).collect();
        let local = scan(&self.folder)?;
        let mut claimed: HashSet<String> = HashSet::new();

        let mut ids: Vec<String> = self.state.files.keys().cloned().collect();
        ids.sort();
        for id in ids {
            let Some(fs_) = self.state.files.get(&id).cloned() else { continue };
            claimed.insert(fs_.name.clone());
            if self.state.conflicts.contains_key(&fs_.name) {
                continue;
            }
            if let Err(e) = self.sync_known(&id, &fs_, remote.get(&id), &local, &mut claimed) {
                self.report.errors.push(format!("{}: {e}", fs_.name));
            }
        }

        let mut untracked: Vec<String> = local.keys().filter(|n| !claimed.contains(*n)).cloned().collect();
        untracked.sort();
        for name in untracked {
            if claimed.contains(&name) || self.state.conflicts.contains_key(&name) {
                continue;
            }
            claimed.insert(name.clone());
            if let Err(e) = self.sync_untracked(&name, &remote) {
                self.report.errors.push(format!("{name}: {e}"));
            }
        }

        let mut fresh: Vec<&Entry> = remote
            .values()
            .filter(|e| !e.deleted && !self.state.files.contains_key(&e.id))
            .filter(|e| !claimed.contains(&e.name) && !self.state.conflicts.contains_key(&e.name))
            .collect();
        fresh.sort_by(|a, b| a.name.cmp(&b.name));
        for e in fresh {
            if let Err(err) = self.download(e, None) {
                self.report.errors.push(format!("{}: {err}", e.name));
            }
        }

        self.state.seq = changes.seq;
        self.state.save()?;
        Ok(self.report)
    }

    fn path(&self, name: &str) -> PathBuf {
        self.folder.join(name)
    }

    fn local_hash(&self, name: &str, s: Stat, known: Option<&FileState>) -> io::Result<String> {
        if let Some(k) = known {
            if k.name == name && k.size == s.size && k.mtime == s.mtime {
                return Ok(k.hash.clone());
            }
        }
        Ok(sha256_hex(&fs::read(self.path(name))?))
    }

    fn record(&mut self, id: &str, name: &str, base: &str, hash: &str) {
        let s = stat(&self.path(name)).unwrap_or(Stat { size: 0, mtime: 0 });
        self.state.files.insert(
            id.to_string(),
            FileState {
                name: name.to_string(),
                base: base.to_string(),
                hash: hash.to_string(),
                size: s.size,
                mtime: s.mtime,
            },
        );
    }

    fn write_local(&self, name: &str, bytes: &[u8]) -> io::Result<()> {
        let tmp = self.path(&format!(".{name}.mapaki-tmp"));
        fs::write(&tmp, bytes)?;
        fs::rename(&tmp, self.path(name))
    }

    /// Never deletes outright: removed files go to the trash folder.
    fn trash_local(&self, name: &str) -> io::Result<()> {
        let src = self.path(name);
        if !src.exists() {
            return Ok(());
        }
        let dir = trash_dir();
        fs::create_dir_all(&dir)?;
        let dest = dir.join(format!("{}-{name}", now()));
        if fs::rename(&src, &dest).is_err() {
            fs::copy(&src, &dest)?;
            fs::remove_file(&src)?;
        }
        Ok(())
    }

    fn sync_known(
        &mut self,
        id: &str,
        known: &FileState,
        remote: Option<&Entry>,
        local: &HashMap<String, Stat>,
        claimed: &mut HashSet<String>,
    ) -> Result<(), BoxError> {
        let local_hash = match local.get(&known.name) {
            Some(s) => Some(self.local_hash(&known.name, *s, Some(known))?),
            None => None,
        };
        let local_changed = local_hash.as_deref() != Some(known.hash.as_str());
        let remote_live = remote.filter(|e| !e.deleted);
        let remote_changed = remote_live.map(|e| e.head != known.base).unwrap_or(true);

        match (local_changed, remote_changed) {
            (false, false) => {
                let e = remote_live.unwrap();
                if e.name != known.name && !self.path(&e.name).exists() {
                    fs::rename(self.path(&known.name), self.path(&e.name))?;
                    claimed.insert(e.name.clone());
                    self.record(id, &e.name, &known.base, &known.hash);
                }
            }
            (true, false) => {
                if local_hash.is_some() {
                    self.upload(id, &known.name, Some(&known.base), false)?;
                } else if let Some(new_name) = self.find_renamed(&known.hash, local, claimed)? {
                    claimed.insert(new_name.clone());
                    let e = self.api.rename(id, &new_name)?;
                    let mut name = new_name.clone();
                    if e.name != new_name && !self.path(&e.name).exists() {
                        fs::rename(self.path(&new_name), self.path(&e.name))?;
                        name = e.name.clone();
                    }
                    self.record(id, &name, &known.base, &known.hash);
                    self.report.uploaded.push(name);
                } else {
                    match self.api.delete(id, &known.base)? {
                        Outcome::Ok(()) => {
                            self.state.files.remove(id);
                            self.report.deleted_remote.push(known.name.clone());
                        }
                        // Edited elsewhere since we last saw it: an edit beats a delete.
                        Outcome::Conflict(e) if !e.deleted => self.download(&e, None)?,
                        Outcome::Conflict(_) => {
                            self.state.files.remove(id);
                        }
                    }
                }
            }
            (false, true) => match remote_live {
                None => {
                    self.trash_local(&known.name)?;
                    self.state.files.remove(id);
                    self.report.deleted_local.push(known.name.clone());
                }
                Some(e) => self.download(e, Some(&known.name))?,
            },
            (true, true) => match (local_hash, remote_live) {
                (None, None) => {
                    self.state.files.remove(id);
                }
                (None, Some(e)) => self.download(e, None)?,
                // Deleted there, edited here: keep the edit as a new file.
                (Some(_), None) => {
                    self.state.files.remove(id);
                    self.create(&known.name)?;
                }
                (Some(_), Some(_)) => self.upload(id, &known.name, Some(&known.base), false)?,
            },
        }
        Ok(())
    }

    /// A tracked file vanished locally; if an untracked file has its exact
    /// content, it was renamed rather than deleted.
    fn find_renamed(
        &self,
        hash: &str,
        local: &HashMap<String, Stat>,
        claimed: &HashSet<String>,
    ) -> io::Result<Option<String>> {
        let tracked: HashSet<&str> = self.state.files.values().map(|f| f.name.as_str()).collect();
        let mut candidates: Vec<&String> = local
            .keys()
            .filter(|n| !claimed.contains(*n) && !tracked.contains(n.as_str()))
            .collect();
        candidates.sort();
        for name in candidates {
            if sha256_hex(&fs::read(self.path(name))?) == hash {
                return Ok(Some(name.clone()));
            }
        }
        Ok(None)
    }

    fn sync_untracked(&mut self, name: &str, remote: &HashMap<String, Entry>) -> Result<(), BoxError> {
        let bytes = fs::read(self.path(name))?;
        let hash = sha256_hex(&bytes);
        let same_name = remote
            .values()
            .find(|e| !e.deleted && e.name == name && !self.state.files.contains_key(&e.id))
            .cloned();
        match same_name {
            Some(e) if e.hash == hash => self.record(&e.id, name, &e.head, &hash),
            Some(e) => self.conflict(name, &e)?,
            None => self.create(name)?,
        }
        Ok(())
    }

    fn create(&mut self, name: &str) -> Result<(), BoxError> {
        let bytes = fs::read(self.path(name))?;
        let e = self.api.create(name, &bytes)?;
        let mut final_name = name.to_string();
        if e.name != name && !self.path(&e.name).exists() {
            fs::rename(self.path(name), self.path(&e.name))?;
            final_name = e.name.clone();
        }
        self.record(&e.id, &final_name, &e.head, &e.hash);
        self.report.uploaded.push(final_name);
        Ok(())
    }

    fn upload(&mut self, id: &str, name: &str, base: Option<&str>, force: bool) -> Result<(), BoxError> {
        let bytes = fs::read(self.path(name))?;
        match self.api.put(id, &bytes, base, force)? {
            Outcome::Ok((PutResult::Merged, e)) => {
                self.download(&e, Some(name))?;
                self.report.downloaded.pop();
                self.report.merged.push(e.name);
            }
            Outcome::Ok((result, e)) => {
                self.record(id, name, &e.head, &e.hash);
                if result == PutResult::FastForward {
                    self.report.uploaded.push(name.to_string());
                }
            }
            Outcome::Conflict(e) => self.conflict(name, &e)?,
        }
        Ok(())
    }

    /// Writes the server's current content for `e` to `e.name`, removing the
    /// old local name (`previous`) if the file was renamed remotely.
    fn download(&mut self, e: &Entry, previous: Option<&str>) -> Result<(), BoxError> {
        let bytes = self.api.content(&e.id, None)?;
        let hash = sha256_hex(&bytes);
        // The head may have moved since /changes; only trust e.head if the
        // bytes match it, otherwise leave base stale so the next pass refetches.
        let base = if hash == e.hash { e.head.clone() } else { String::new() };
        let target_is_other_file = previous != Some(e.name.as_str()) && self.path(&e.name).exists();
        if target_is_other_file {
            self.trash_local(&e.name)?;
        }
        self.write_local(&e.name, &bytes)?;
        if let Some(prev) = previous {
            if prev != e.name {
                let _ = fs::remove_file(self.path(prev));
            }
        }
        self.record(&e.id, &e.name, &base, &hash);
        self.report.downloaded.push(e.name.clone());
        Ok(())
    }

    fn conflict(&mut self, name: &str, remote: &Entry) -> Result<(), BoxError> {
        match (self.decide)(name, remote) {
            Some(choice) => self.apply(name, remote, choice),
            None => {
                self.state.conflicts.insert(
                    name.to_string(),
                    Conflict {
                        id: Some(remote.id.clone()),
                        name: name.to_string(),
                        remote_deleted: remote.deleted,
                        detected_at: now(),
                    },
                );
                self.report.conflicts.push(name.to_string());
                Ok(())
            }
        }
    }

    /// Carries out a conflict decision for the local file `name` against the
    /// server's entry `remote`.
    pub fn apply(&mut self, name: &str, remote: &Entry, choice: Choice) -> Result<(), BoxError> {
        self.state.conflicts.remove(name);
        let local_exists = self.path(name).exists();
        match choice {
            Choice::Remote if remote.deleted => {
                self.trash_local(name)?;
                self.state.files.remove(&remote.id);
                self.report.deleted_local.push(name.to_string());
            }
            Choice::Remote => self.download(remote, Some(name))?,
            _ if !local_exists => self.apply(name, remote, Choice::Remote)?,
            Choice::Local | Choice::Both if remote.deleted => {
                self.state.files.remove(&remote.id);
                self.create(name)?;
            }
            Choice::Local => self.upload(&remote.id, name, None, true)?,
            Choice::Both => {
                let copy = conflict_copy_name(&self.folder, name);
                fs::rename(self.path(name), self.path(&copy))?;
                self.create(&copy)?;
                self.state.files.remove(&remote.id);
                self.download(remote, None)?;
            }
        }
        self.state.save()?;
        Ok(())
    }
}

/// Resolves a recorded conflict outside a sync pass (`mapaki resolve`).
pub fn resolve(api: &Api, folder: &Path, state: &mut State, name: &str, choice: Choice) -> Result<Report, BoxError> {
    let conflict = state.conflicts.get(name).cloned().ok_or("no conflict recorded for that file")?;
    let changes = api.changes(0)?;
    let remote = changes
        .entries
        .into_iter()
        .find(|e| Some(&e.id) == conflict.id.as_ref())
        .ok_or("the server no longer knows that file; run `mapaki sync`")?;
    let mut never = |_: &str, _: &Entry| None;
    let mut syncer = Syncer::new(api, folder, state, &mut never);
    syncer.apply(name, &remote, choice)?;
    Ok(syncer.report)
}
