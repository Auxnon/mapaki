//! Server-to-server sync: this daemon keeps its folder in step with another
//! mapaki daemon (a "peer"), both ways.
//!
//! The link runs on one side only. It pairs with the peer like any device,
//! so the peer needs nothing special, and one link carries changes in both
//! directions. Files are matched up once (by name and content on first
//! contact) and remembered as pairs: our file and revision plus the peer's
//! file and revision that held the same content when the two last agreed.
//! Each pass compares both current heads against that pair:
//!
//!   only ours moved    upload, based on the peer revision we last agreed on
//!   only theirs moved  take theirs
//!   both moved         upload anyway: the peer merges text whose edits don't
//!                      overlap; otherwise both versions are kept, ours as a
//!                      `(conflict copy)`
//!
//! Either server may be switched off for days. While the peer is unreachable
//! the link checks once a minute without logging anything, except once when
//! the peer goes away and once when it's back.

use std::collections::{HashMap, HashSet};
use std::fs;
use std::io::{self, BufRead, BufReader, Read};
use std::net::{IpAddr, SocketAddr, ToSocketAddrs};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::store::{self, Entry, PutResult, Store, StoreError};

/// How often an unreachable peer is checked. One small request; quiet.
const OFFLINE_CHECK: Duration = Duration::from_secs(60);
/// A full pass now and then even while the event stream says nothing, in
/// case a notice was lost.
const SAFETY_PASS: Duration = Duration::from_secs(15 * 60);
/// Local changes are gathered this long first, so a burst syncs as one pass.
const LOCAL_QUIET: Duration = Duration::from_secs(1);
/// How often the running daemon re-reads the peer list (`peer add`/`remove`).
const CONFIG_CHECK: Duration = Duration::from_secs(10);
/// An event stream that dies sooner than this waits before reconnecting, so
/// a peer that accepts and drops connections can't make the link spin.
const MIN_STREAM_LIFE: Duration = Duration::from_secs(30);

/// One link, as saved by `mapaki-daemon peer add`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PeerConfig {
    /// The peer's host name, e.g. `lunchbox.local`; names the link.
    pub name: String,
    pub url: String,
    pub token: String,
}

fn config_path(dir: &Path) -> PathBuf {
    dir.join(".mapaki-peers.json")
}

fn state_path(dir: &Path, name: &str) -> PathBuf {
    dir.join(".mapaki").join("peers").join(format!("{name}.json"))
}

pub fn read_config(dir: &Path) -> Vec<PeerConfig> {
    fs::read(config_path(dir)).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default()
}

/// Holds tokens for the peers, so only the owner can read it.
pub fn write_config(dir: &Path, peers: &[PeerConfig]) -> io::Result<()> {
    let path = config_path(dir);
    let tmp = path.with_extension("tmp");
    let mut options = fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    io::Write::write_all(&mut options.open(&tmp)?, &serde_json::to_vec_pretty(peers).unwrap())?;
    fs::rename(tmp, path)
}

pub fn forget_state(dir: &Path, name: &str) {
    let _ = fs::remove_file(state_path(dir, name));
}

/// The same file on both servers, as of when they last agreed on it.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct Pair {
    local: Uuid,
    remote: String,
    local_rev: String,
    remote_rev: String,
    name: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PeerState {
    remote_seq: u64,
    local_seq: u64,
    pairs: Vec<Pair>,
    /// When a pass last finished cleanly.
    #[serde(default)]
    pub last_sync: i64,
    #[serde(default)]
    pub online: bool,
    /// When `online` last flipped.
    #[serde(default)]
    pub since: i64,
    /// Why the last attempt failed, if it did.
    #[serde(default)]
    pub problem: Option<String>,
}

pub fn read_state(dir: &Path, name: &str) -> PeerState {
    fs::read(state_path(dir, name)).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default()
}

fn now() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs() as i64)
}

#[derive(Debug)]
pub enum LinkError {
    /// Couldn't reach the peer at all: it's off, asleep, or off the network.
    Offline(String),
    /// The peer refused our token (it was revoked there).
    Rejected,
    /// One file couldn't be synced; the rest of the pass carries on.
    File(String),
}

impl std::fmt::Display for LinkError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LinkError::Offline(m) | LinkError::File(m) => f.write_str(m),
            LinkError::Rejected => f.write_str("the peer turned down this server's token"),
        }
    }
}

impl From<StoreError> for LinkError {
    fn from(e: StoreError) -> Self {
        LinkError::File(e.to_string())
    }
}

fn http_error(url: &str, e: ureq::Error) -> LinkError {
    match e {
        ureq::Error::Status(401, _) => LinkError::Rejected,
        ureq::Error::Status(code, resp) => {
            LinkError::File(format!("HTTP {code} from {url}: {}", resp.into_string().unwrap_or_default().trim()))
        }
        ureq::Error::Transport(t) => LinkError::Offline(t.to_string()),
    }
}

/// `host.local` names are answered by mDNS, which a statically linked
/// (musl) binary can't query itself. `getent` goes through the system's
/// resolver, which can. IPv4 first: older daemons listen on IPv4 only.
fn resolve(netloc: &str) -> io::Result<Vec<SocketAddr>> {
    if let Some((host, port)) = netloc.rsplit_once(':') {
        let host = host.trim_matches(['[', ']']);
        if host.ends_with(".local") {
            if let (Ok(port), Ok(out)) = (port.parse::<u16>(), std::process::Command::new("getent").args(["ahosts", host]).output()) {
                let mut addrs: Vec<SocketAddr> = vec![];
                for line in String::from_utf8_lossy(&out.stdout).lines() {
                    if let Some(Ok(ip)) = line.split_whitespace().next().map(str::parse::<IpAddr>) {
                        let addr = SocketAddr::new(ip, port);
                        if !addrs.contains(&addr) {
                            addrs.push(addr);
                        }
                    }
                }
                addrs.sort_by_key(|a| a.is_ipv6());
                if !addrs.is_empty() {
                    return Ok(addrs);
                }
            }
        }
    }
    netloc.to_socket_addrs().map(|a| a.collect())
}

fn agent(read_timeout: Duration) -> ureq::Agent {
    ureq::AgentBuilder::new()
        .timeout_connect(Duration::from_secs(5))
        .timeout_read(read_timeout)
        .resolver(resolve)
        .user_agent(concat!("mapaki-daemon/", env!("CARGO_PKG_VERSION")))
        .build()
}

/// `lunchbox.local` → `http://lunchbox.local:8420`.
pub fn normalize_url(url: &str) -> String {
    let url = url.trim().trim_end_matches('/');
    let url = if url.contains("://") { url.to_string() } else { format!("http://{url}") };
    let after_scheme = &url[url.find("://").unwrap() + 3..];
    let has_port = after_scheme.rsplit_once(':').is_some_and(|(_, p)| p.parse::<u16>().is_ok());
    if has_port { url } else { format!("{url}:8420") }
}

/// The host part of a URL, which names the link.
pub fn host_of(url: &str) -> String {
    let rest = url.split_once("://").map_or(url, |(_, r)| r);
    let host = rest.split('/').next().unwrap_or(rest);
    match host.rsplit_once(':') {
        Some((h, p)) if p.parse::<u16>().is_ok() => h.trim_matches(['[', ']']).to_string(),
        _ => host.to_string(),
    }
}

/// Redeems a pairing code on the peer, as a device called `device`.
pub fn pair(url: &str, otp: &str, device: &str) -> Result<String, LinkError> {
    #[derive(Deserialize)]
    struct Resp {
        token: String,
    }
    let full = format!("{url}/pair");
    let resp = agent(Duration::from_secs(20))
        .post(&full)
        .send_json(serde_json::json!({ "otp": otp, "device": device }))
        .map_err(|e| match e {
            ureq::Error::Status(401, r) => LinkError::File(r.into_string().unwrap_or_else(|_| "code not accepted".into())),
            e => http_error(&full, e),
        })?;
    let resp: Resp = resp.into_json().map_err(|e| LinkError::File(format!("bad response from {url}: {e}")))?;
    Ok(resp.token)
}

#[derive(Debug, Clone, Deserialize)]
struct RemoteEntry {
    id: String,
    name: String,
    #[serde(default)]
    head: String,
    #[serde(default)]
    hash: String,
    #[serde(default)]
    deleted: bool,
}

enum Put {
    Done(PutResult, RemoteEntry),
    /// 409: the peer's current entry.
    Conflict(RemoteEntry),
}

#[derive(Deserialize)]
struct ConflictBody {
    entry: RemoteEntry,
}

/// The peer's HTTP API, as much of it as the link needs.
#[derive(Clone)]
struct Remote {
    base: String,
    token: String,
    agent: ureq::Agent,
}

impl Remote {
    fn new(cfg: &PeerConfig) -> Remote {
        Remote { base: cfg.url.clone(), token: cfg.token.clone(), agent: agent(Duration::from_secs(120)) }
    }

    fn call(&self, method: &str, path: &str, body: Option<&[u8]>) -> Result<ureq::Response, ureq::Error> {
        let req = self.agent.request(method, &format!("{}{path}", self.base)).set("Authorization", &format!("Bearer {}", self.token));
        match body {
            Some(b) => req.send_bytes(b),
            None => req.call(),
        }
    }

    fn json<T: serde::de::DeserializeOwned>(&self, resp: ureq::Response) -> Result<T, LinkError> {
        resp.into_json().map_err(|e| LinkError::File(format!("bad response from {}: {e}", self.base)))
    }

    fn fail(&self, path: &str, e: ureq::Error) -> LinkError {
        http_error(&format!("{}{path}", self.base), e)
    }

    fn changes(&self, since: u64) -> Result<(u64, Vec<RemoteEntry>), LinkError> {
        #[derive(Deserialize)]
        struct Changes {
            seq: u64,
            entries: Vec<RemoteEntry>,
        }
        let path = format!("/changes?since={since}");
        let c: Changes = self.json(self.call("GET", &path, None).map_err(|e| self.fail(&path, e))?)?;
        Ok((c.seq, c.entries))
    }

    fn content(&self, id: &str) -> Result<Vec<u8>, LinkError> {
        let path = format!("/files/{id}/content");
        let resp = self.call("GET", &path, None).map_err(|e| self.fail(&path, e))?;
        let mut bytes = vec![];
        resp.into_reader().read_to_end(&mut bytes).map_err(|e| LinkError::Offline(format!("download cut off: {e}")))?;
        Ok(bytes)
    }

    fn create(&self, name: &str, bytes: &[u8]) -> Result<RemoteEntry, LinkError> {
        let path = format!("/files/new?name={}", encode(name));
        self.json(self.call("POST", &path, Some(bytes)).map_err(|e| self.fail(&path, e))?)
    }

    fn put(&self, id: &str, bytes: &[u8], base: &str, force: bool) -> Result<Put, LinkError> {
        #[derive(Deserialize)]
        struct PutResponse {
            result: PutResult,
            entry: RemoteEntry,
        }
        let path = format!("/files/{id}/content?force={force}&base={base}");
        match self.call("PUT", &path, Some(bytes)) {
            Ok(resp) => {
                let r: PutResponse = self.json(resp)?;
                Ok(Put::Done(r.result, r.entry))
            }
            Err(ureq::Error::Status(409, resp)) => Ok(Put::Conflict(self.json::<ConflictBody>(resp)?.entry)),
            Err(e) => Err(self.fail(&path, e)),
        }
    }

    /// None once deleted; Some(current entry) if it changed since `base`.
    fn delete(&self, id: &str, base: &str) -> Result<Option<RemoteEntry>, LinkError> {
        let path = format!("/files/{id}?base={base}");
        match self.call("DELETE", &path, None) {
            Ok(_) | Err(ureq::Error::Status(404, _)) => Ok(None),
            Err(ureq::Error::Status(409, resp)) => Ok(Some(self.json::<ConflictBody>(resp)?.entry)),
            Err(e) => Err(self.fail(&path, e)),
        }
    }

    fn rename(&self, id: &str, name: &str) -> Result<RemoteEntry, LinkError> {
        let path = format!("/files/{id}");
        let resp = self
            .agent
            .request("PATCH", &format!("{}{path}", self.base))
            .set("Authorization", &format!("Bearer {}", self.token))
            .send_json(serde_json::json!({ "name": name }))
            .map_err(|e| self.fail(&path, e))?;
        self.json(resp)
    }

    /// Blocks on the peer's event stream, sending `Msg::Remote` for each
    /// change notice, until the stream ends or the link is stopped.
    fn listen(&self, tx: &Sender<Msg>, stop: &AtomicBool) {
        let resp = agent(Duration::from_secs(200))
            .get(&format!("{}/events", self.base))
            .set("Authorization", &format!("Bearer {}", self.token))
            .set("Accept", "text/event-stream")
            .call();
        let Ok(resp) = resp else { return };
        let mut first = true;
        for line in BufReader::new(resp.into_reader()).lines() {
            let Ok(line) = line else { return };
            if stop.load(Ordering::Relaxed) {
                return;
            }
            if line.starts_with("data:") {
                // The first event only reports where the feed stands.
                if !std::mem::take(&mut first) && tx.send(Msg::Remote).is_err() {
                    return;
                }
            }
        }
    }
}

/// Minimal query-string percent-encoding (file names can contain anything).
fn encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => out.push(b as char),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// `notes.txt` → `notes (conflict copy).txt`, the desktop client's naming.
fn conflict_name(name: &str) -> String {
    match name.rsplit_once('.') {
        Some((stem, ext)) if !stem.is_empty() => format!("{stem} (conflict copy).{ext}"),
        _ => format!("{name} (conflict copy)"),
    }
}

/// What one pass did, for the log.
#[derive(Debug, Default, PartialEq)]
pub struct Tally {
    pub sent: usize,
    pub received: usize,
    pub merged: usize,
    pub kept_both: usize,
    pub failed: usize,
}

impl Tally {
    fn is_empty(&self) -> bool {
        *self == Tally::default()
    }

    fn summary(&self) -> String {
        let parts: Vec<String> = [
            (self.sent, "sent"),
            (self.received, "received"),
            (self.merged, "merged"),
            (self.kept_both, "kept both (conflict copy)"),
            (self.failed, "failed"),
        ]
        .into_iter()
        .filter(|(n, _)| *n > 0)
        .map(|(n, what)| format!("{n} {what}"))
        .collect();
        if parts.is_empty() { "in sync".into() } else { parts.join(", ") }
    }
}

enum Msg {
    Local,
    Remote,
    /// The event stream ended: the peer may have gone away.
    StreamEnded,
}

pub struct Link {
    cfg: PeerConfig,
    store: Arc<Store>,
    dir: PathBuf,
    remote: Remote,
    state: PeerState,
    /// Recorded on the revisions this link makes here.
    device: String,
    tally: Tally,
    /// Pairs made mid-pass (conflict copies), added when the pass ends.
    added: Vec<Pair>,
}

impl Link {
    pub fn new(cfg: PeerConfig, store: Arc<Store>, dir: PathBuf) -> Link {
        Link {
            remote: Remote::new(&cfg),
            state: read_state(&dir, &cfg.name),
            device: cfg.name.clone(),
            cfg,
            store,
            dir,
            tally: Tally::default(),
            added: vec![],
        }
    }

    fn save_state(&self) {
        let path = state_path(&self.dir, &self.cfg.name);
        if let Some(parent) = path.parent() {
            let _ = fs::create_dir_all(parent);
        }
        if let Err(e) = store::write_json_atomic(&path, &self.state) {
            tracing::warn!("{}: couldn't save link state: {e}", self.cfg.name);
        }
    }

    fn local_bytes(&self, id: Uuid) -> Result<Vec<u8>, LinkError> {
        let (path, name) = self.store.content_path(id).ok_or(LinkError::File(format!("{id} vanished")))?;
        fs::read(path).map_err(|e| LinkError::File(format!("can't read {name}: {e}")))
    }

    /// One reconcile pass over everything that changed on either side since
    /// the last one. Offline/Rejected abort the pass (progress so far is
    /// kept); a file that fails is retried next pass.
    pub fn pass(&mut self) -> Result<Tally, LinkError> {
        self.tally = Tally::default();
        let (remote_seq, remote_changed) = self.remote.changes(self.state.remote_seq)?;
        let (local_seq, local_changed) = self.store.changes(self.state.local_seq);
        let mut remote_changed: HashMap<String, RemoteEntry> = remote_changed.into_iter().map(|e| (e.id.clone(), e)).collect();
        let local_changed_ids: HashSet<Uuid> = local_changed.iter().map(|e| e.id).collect();
        let paired_local: HashSet<Uuid> = self.state.pairs.iter().map(|p| p.local).collect();

        let result = self.sync_pairs(&mut remote_changed, &local_changed_ids).and_then(|()| {
            // Unpaired on both sides: new files, or ones from before the link.
            let mut theirs_by_name: HashMap<String, RemoteEntry> =
                remote_changed.into_values().filter(|e| !e.deleted).map(|e| (e.name.clone(), e)).collect();
            for ours in local_changed.into_iter().filter(|e| !e.deleted && !paired_local.contains(&e.id)) {
                let theirs = theirs_by_name.remove(&ours.name);
                self.attempt(&ours.name.clone(), |link| link.first_meeting(ours, theirs))?;
            }
            let mut theirs: Vec<RemoteEntry> = theirs_by_name.into_values().collect();
            theirs.sort_by(|a, b| a.name.cmp(&b.name));
            for e in theirs {
                self.attempt(&e.name.clone(), |link| link.take_new(e))?;
            }
            Ok(())
        });
        self.state.pairs.append(&mut self.added);
        if result.is_ok() && self.tally.failed == 0 {
            self.state.remote_seq = remote_seq;
            self.state.local_seq = local_seq;
        }
        if result.is_ok() {
            self.state.last_sync = now();
        }
        self.save_state();
        result.map(|()| std::mem::take(&mut self.tally))
    }

    /// Runs one file's step; a per-file failure is logged and counted, a
    /// lost connection ends the pass.
    fn attempt(&mut self, name: &str, step: impl FnOnce(&mut Link) -> Result<(), LinkError>) -> Result<(), LinkError> {
        match step(self) {
            Err(LinkError::File(e)) => {
                tracing::warn!("{}: couldn't sync {name}: {e}; will retry", self.cfg.name);
                self.tally.failed += 1;
                Ok(())
            }
            other => other,
        }
    }

    fn sync_pairs(&mut self, remote_changed: &mut HashMap<String, RemoteEntry>, local_changed: &HashSet<Uuid>) -> Result<(), LinkError> {
        let mut i = 0;
        while i < self.state.pairs.len() {
            let pair = self.state.pairs[i].clone();
            let theirs = remote_changed.remove(&pair.remote);
            if theirs.is_none() && !local_changed.contains(&pair.local) {
                i += 1;
                continue;
            }
            let mut outcome = None;
            self.attempt(&pair.name.clone(), |link| {
                outcome = Some(link.reconcile(&pair, theirs)?);
                Ok(())
            })?;
            match outcome {
                Some(None) => {
                    self.state.pairs.remove(i);
                }
                Some(Some(updated)) => {
                    self.state.pairs[i] = updated;
                    i += 1;
                }
                None => i += 1,
            }
        }
        Ok(())
    }

    /// Brings one pair back in step. None when the file is gone on both sides.
    fn reconcile(&mut self, pair: &Pair, theirs: Option<RemoteEntry>) -> Result<Option<Pair>, LinkError> {
        let Some(ours) = self.store.get(pair.local) else { return Ok(None) };
        // Not in the peer's feed: unchanged there since we last agreed.
        let theirs = theirs.unwrap_or_else(|| RemoteEntry {
            id: pair.remote.clone(),
            name: pair.name.clone(),
            head: pair.remote_rev.clone(),
            hash: String::new(),
            deleted: false,
        });
        let we_changed = ours.head != pair.local_rev;
        let they_changed = theirs.head != pair.remote_rev;
        let synced = match (we_changed, they_changed) {
            _ if ours.deleted && theirs.deleted => return Ok(None),
            (true, false) if ours.deleted => match self.remote.delete(&theirs.id, &pair.remote_rev)? {
                None => {
                    self.tally.sent += 1;
                    return Ok(None);
                }
                Some(now) => self.both_changed(pair, &ours, now)?,
            },
            (true, false) => self.send(pair, &ours, theirs)?,
            (false, true) if theirs.deleted => match self.store.delete(ours.id, Some(&pair.local_rev), &self.device) {
                Ok(_) => {
                    self.tally.received += 1;
                    return Ok(None);
                }
                // Edited here a moment ago; the next pass sorts it out.
                Err(StoreError::Conflict(_)) => return Ok(Some(pair.clone())),
                Err(e) => return Err(e.into()),
            },
            (false, true) => self.receive(pair, &ours, theirs)?,
            (true, true) => self.both_changed(pair, &ours, theirs)?,
            (false, false) => (pair.clone(), theirs.name),
        };
        self.align_names(synced).map(Some)
    }

    /// Our content onto theirs. Returns the updated pair and their name.
    fn send(&mut self, pair: &Pair, ours: &Entry, theirs: RemoteEntry) -> Result<(Pair, String), LinkError> {
        let bytes = self.local_bytes(ours.id)?;
        match self.remote.put(&theirs.id, &bytes, &pair.remote_rev, theirs.deleted)? {
            Put::Done(PutResult::Merged, merged) => self.take_merge(pair, ours, merged),
            Put::Done(result, now) => {
                if result != PutResult::Unchanged {
                    self.tally.sent += 1;
                }
                Ok((Pair { local_rev: ours.head.clone(), remote_rev: now.head, ..pair.clone() }, now.name))
            }
            Put::Conflict(now) => self.keep_both(pair, ours, now),
        }
    }

    /// Their content onto ours.
    fn receive(&mut self, pair: &Pair, ours: &Entry, theirs: RemoteEntry) -> Result<(Pair, String), LinkError> {
        let bytes = self.remote.content(&theirs.id)?;
        let (result, now) = self.store.put_content(ours.id, &bytes, Some(&ours.head), &self.device, ours.deleted)?;
        if result != PutResult::Unchanged {
            self.tally.received += 1;
        }
        Ok((Pair { local_rev: now.head, remote_rev: theirs.head, ..pair.clone() }, theirs.name))
    }

    fn both_changed(&mut self, pair: &Pair, ours: &Entry, theirs: RemoteEntry) -> Result<(Pair, String), LinkError> {
        // An edit beats a delete, whichever side made it.
        if ours.deleted {
            return self.receive(pair, ours, theirs);
        }
        if theirs.deleted {
            return self.send(pair, ours, theirs);
        }
        if ours.hash == theirs.hash {
            return Ok((Pair { local_rev: ours.head.clone(), remote_rev: theirs.head, ..pair.clone() }, theirs.name));
        }
        // Based on the revision we last agreed on, so the peer can 3-way merge.
        self.send(pair, ours, theirs)
    }

    /// The peer merged our upload with its own edits: bring the result here.
    fn take_merge(&mut self, pair: &Pair, ours: &Entry, merged: RemoteEntry) -> Result<(Pair, String), LinkError> {
        let bytes = self.remote.content(&merged.id)?;
        let (_, now) = self.store.put_content(ours.id, &bytes, Some(&ours.head), &self.device, false)?;
        self.tally.merged += 1;
        Ok((Pair { local_rev: now.head, remote_rev: merged.head, ..pair.clone() }, merged.name))
    }

    /// Edits that can't be merged: the peer's version keeps the name on both
    /// servers and ours is saved next to it as a conflict copy, on both too.
    fn keep_both(&mut self, pair: &Pair, ours: &Entry, theirs: RemoteEntry) -> Result<(Pair, String), LinkError> {
        let bytes = self.local_bytes(ours.id)?;
        let copy = self.remote.create(&conflict_name(&ours.name), &bytes)?;
        let local_copy = self.store.insert_new(&copy.name, &bytes, &self.device)?;
        self.added.push(Pair {
            local: local_copy.id,
            remote: copy.id,
            local_rev: local_copy.head,
            remote_rev: copy.head,
            name: copy.name.clone(),
        });
        tracing::warn!("{}: {} changed on both servers; kept both, this server's version is now {}", self.cfg.name, ours.name, copy.name);
        let received = self.tally.received;
        let synced = self.receive(pair, ours, theirs)?;
        self.tally.received = received;
        self.tally.kept_both += 1;
        Ok(synced)
    }

    /// A file renamed on one side gets the same name on the other. If both
    /// renamed it, the peer's name wins.
    fn align_names(&mut self, (mut pair, their_name): (Pair, String)) -> Result<Pair, LinkError> {
        let Some(ours) = self.store.get(pair.local).filter(|e| !e.deleted) else { return Ok(pair) };
        if their_name != pair.name {
            let renamed = self.store.rename(ours.id, &their_name)?;
            if renamed.name != their_name {
                // Taken here by another file; follow our deduped name there.
                self.remote.rename(&pair.remote, &renamed.name)?;
            }
            pair.name = renamed.name;
        } else if ours.name != pair.name {
            let renamed = self.remote.rename(&pair.remote, &ours.name)?;
            if renamed.name != ours.name {
                self.store.rename(ours.id, &renamed.name)?;
            }
            pair.name = renamed.name;
        }
        Ok(pair)
    }

    /// A file of ours the link hasn't paired yet, and the peer's unpaired
    /// file of the same name, if any.
    fn first_meeting(&mut self, ours: Entry, theirs: Option<RemoteEntry>) -> Result<(), LinkError> {
        match theirs {
            Some(theirs) if theirs.hash == ours.hash => {
                self.added.push(Pair {
                    local: ours.id,
                    remote: theirs.id,
                    local_rev: ours.head,
                    remote_rev: theirs.head,
                    name: ours.name,
                });
            }
            Some(theirs) => {
                // Two different files that happen to share a name: theirs
                // keeps it, ours becomes the conflict copy.
                let renamed = self.store.rename(ours.id, &conflict_name(&ours.name))?;
                tracing::warn!("{}: both servers had a different {}; kept both, this server's is now {}", self.cfg.name, ours.name, renamed.name);
                self.send_new(renamed)?;
                self.take_new(theirs)?;
                self.tally.kept_both += 1;
                self.tally.sent -= 1;
                self.tally.received -= 1;
            }
            None => self.send_new(ours)?,
        }
        Ok(())
    }

    fn send_new(&mut self, ours: Entry) -> Result<(), LinkError> {
        let bytes = self.local_bytes(ours.id)?;
        let theirs = self.remote.create(&ours.name, &bytes)?;
        self.tally.sent += 1;
        let pair = Pair { local: ours.id, remote: theirs.id, local_rev: ours.head, remote_rev: theirs.head, name: ours.name };
        let pair = self.align_names((pair, theirs.name))?;
        self.added.push(pair);
        Ok(())
    }

    fn take_new(&mut self, theirs: RemoteEntry) -> Result<(), LinkError> {
        let bytes = self.remote.content(&theirs.id)?;
        let ours = self.store.insert_new(&theirs.name, &bytes, &self.device)?;
        self.tally.received += 1;
        let pair = Pair { local: ours.id, remote: theirs.id, local_rev: ours.head, remote_rev: theirs.head, name: theirs.name.clone() };
        // insert_new may have had to pick a free name here.
        let pair = self.align_names((pair, theirs.name))?;
        self.added.push(pair);
        Ok(())
    }

    /// Recorded for `mapaki-daemon peer list`; written only when it changes.
    fn set_online(&mut self, online: bool, problem: Option<String>) {
        if self.state.online == online && self.state.problem == problem && self.state.since != 0 {
            return;
        }
        if self.state.online != online || self.state.since == 0 {
            self.state.since = now();
        }
        self.state.online = online;
        self.state.problem = problem;
        self.save_state();
    }

    /// Keeps the link going until `stop` is set: a pass whenever either side
    /// changes, quiet once-a-minute checks while the peer is away.
    fn run(mut self, stop: Arc<AtomicBool>, tx: Sender<Msg>, rx: Receiver<Msg>) {
        let name = self.cfg.name.clone();
        // None until the first attempt, so the first outcome is always logged.
        let mut reachable: Option<bool> = None;
        let mut rejected = false;
        let mut listening = false;
        loop {
            if stop.load(Ordering::Relaxed) {
                return;
            }
            match self.pass() {
                Ok(tally) => {
                    if reachable != Some(true) {
                        tracing::info!("{name}: connected; {}", tally.summary());
                    } else if !tally.is_empty() {
                        tracing::info!("{name}: {}", tally.summary());
                    }
                    reachable = Some(true);
                    rejected = false;
                    self.set_online(true, None);
                }
                Err(LinkError::Offline(why)) => {
                    if reachable != Some(false) {
                        tracing::info!("{name} isn't reachable ({why}); checking once a minute until it's back");
                    }
                    reachable = Some(false);
                    self.set_online(false, Some("not reachable".into()));
                }
                Err(LinkError::Rejected) => {
                    if !rejected {
                        tracing::warn!("{name} turned down this server's token; link again with `mapaki-daemon peer add {} <code>`", self.cfg.url);
                    }
                    rejected = true;
                    reachable = Some(false);
                    self.set_online(false, Some("token rejected; link again".into()));
                }
                Err(LinkError::File(e)) => tracing::warn!("{name}: {e}"),
            }
            let online = reachable == Some(true);
            if online && !listening {
                listening = true;
                let (remote, tx, stop) = (self.remote.clone(), tx.clone(), stop.clone());
                std::thread::spawn(move || {
                    let started = Instant::now();
                    remote.listen(&tx, &stop);
                    if let Some(rest) = MIN_STREAM_LIFE.checked_sub(started.elapsed()) {
                        std::thread::sleep(rest);
                    }
                    let _ = tx.send(Msg::StreamEnded);
                });
            }

            let deadline = Instant::now() + if online { SAFETY_PASS } else { OFFLINE_CHECK };
            loop {
                let left = deadline.saturating_duration_since(Instant::now());
                if left.is_zero() {
                    break;
                }
                match rx.recv_timeout(left.min(Duration::from_secs(5))) {
                    Ok(Msg::StreamEnded) => {
                        listening = false;
                        // Gone, or just a dropped connection? A pass tells.
                        if online {
                            break;
                        }
                    }
                    Ok(Msg::Remote) if online => break,
                    Ok(Msg::Local) if online => {
                        std::thread::sleep(LOCAL_QUIET);
                        while let Ok(Msg::Local) = rx.try_recv() {}
                        break;
                    }
                    // While the peer is away, changes wait for the next check.
                    Ok(_) => {}
                    Err(RecvTimeoutError::Timeout) => {
                        if stop.load(Ordering::Relaxed) {
                            return;
                        }
                    }
                    Err(RecvTimeoutError::Disconnected) => return,
                }
            }
        }
    }
}

/// Starts a link for every configured peer and keeps the set in step with
/// `.mapaki-peers.json`, so `peer add`/`peer remove` apply without a restart.
pub fn start(dir: PathBuf, store: Arc<Store>) {
    let runtime = tokio::runtime::Handle::current();
    std::thread::spawn(move || {
        let mut running: Vec<(PeerConfig, Arc<AtomicBool>)> = vec![];
        loop {
            let configs = read_config(&dir);
            running.retain(|(cfg, stop)| {
                let keep = configs.contains(cfg);
                if !keep {
                    stop.store(true, Ordering::Relaxed);
                }
                keep
            });
            for cfg in configs {
                if running.iter().any(|(c, _)| *c == cfg) {
                    continue;
                }
                tracing::info!("syncing with {} at {}", cfg.name, cfg.url);
                let stop = Arc::new(AtomicBool::new(false));
                let (tx, rx) = mpsc::channel();
                let mut changes = store.subscribe();
                let local_tx = tx.clone();
                runtime.spawn(async move {
                    while changes.changed().await.is_ok() {
                        if local_tx.send(Msg::Local).is_err() {
                            break;
                        }
                    }
                });
                let link = Link::new(cfg.clone(), store.clone(), dir.clone());
                let link_stop = stop.clone();
                std::thread::spawn(move || link.run(link_stop, tx, rx));
                running.push((cfg, stop));
            }
            std::thread::sleep(CONFIG_CHECK);
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::Auth;
    use crate::routes::{router, AppState};

    struct Peer {
        store: Arc<Store>,
        url: String,
        token: String,
        _dir: tempdir::TempDir,
    }

    /// A real daemon on a random port, serving its own temporary folder.
    fn serve_peer() -> Peer {
        let dir = tempdir::TempDir::new("mapaki-peer").unwrap();
        let store = Arc::new(Store::load(dir.path().to_path_buf()).unwrap());
        let auth = Arc::new(Auth::load(dir.path().join(".mapaki-tokens.json")));
        let otp = auth.start_pairing();
        let token = auth.redeem_pairing(&otp, Some("test")).unwrap();
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let app = router(AppState { store: store.clone(), auth });
        std::thread::spawn(move || {
            tokio::runtime::Runtime::new().unwrap().block_on(async move {
                listener.set_nonblocking(true).unwrap();
                let listener = tokio::net::TcpListener::from_std(listener).unwrap();
                axum::serve(listener, app).await.unwrap();
            });
        });
        Peer { store, url, token, _dir: dir }
    }

    struct Here {
        store: Arc<Store>,
        link: Link,
        _dir: tempdir::TempDir,
    }

    fn link_to(peer: &Peer) -> Here {
        let dir = tempdir::TempDir::new("mapaki-here").unwrap();
        let store = Arc::new(Store::load(dir.path().to_path_buf()).unwrap());
        let cfg = PeerConfig { name: "peer".into(), url: peer.url.clone(), token: peer.token.clone() };
        let link = Link::new(cfg, store.clone(), dir.path().to_path_buf());
        Here { store, link, _dir: dir }
    }

    /// name → content of every live file.
    fn files(store: &Store) -> Vec<(String, String)> {
        let mut out: Vec<_> = store
            .list()
            .into_iter()
            .map(|e| {
                let (path, _) = store.content_path(e.id).unwrap();
                (e.name, fs::read_to_string(path).unwrap())
            })
            .collect();
        out.sort();
        out
    }

    fn id_of(store: &Store, name: &str) -> Entry {
        store.list().into_iter().find(|e| e.name == name).unwrap()
    }

    fn edit(store: &Store, name: &str, text: &str) {
        let e = id_of(store, name);
        store.put_content(e.id, text.as_bytes(), Some(&e.head), "test", false).unwrap();
    }

    fn pairs(v: &[(&str, &str)]) -> Vec<(String, String)> {
        v.iter().map(|(a, b)| (a.to_string(), b.to_string())).collect()
    }

    #[test]
    fn files_flow_both_ways_and_settle() {
        let peer = serve_peer();
        let mut here = link_to(&peer);
        here.store.insert_new("ours.txt", b"from here", "test").unwrap();
        peer.store.insert_new("theirs.txt", b"from there", "test").unwrap();

        let t = here.link.pass().unwrap();
        assert_eq!((t.sent, t.received), (1, 1));
        let both = pairs(&[("ours.txt", "from here"), ("theirs.txt", "from there")]);
        assert_eq!(files(&here.store), both);
        assert_eq!(files(&peer.store), both);
        // Our own writes show up in both feeds once; after that, nothing to do.
        here.link.pass().unwrap();
        assert!(here.link.pass().unwrap().is_empty());
    }

    #[test]
    fn edits_renames_and_deletes_go_across() {
        let peer = serve_peer();
        let mut here = link_to(&peer);
        here.store.insert_new("a.txt", b"one", "test").unwrap();
        peer.store.insert_new("b.txt", b"two", "test").unwrap();
        here.link.pass().unwrap();

        edit(&here.store, "a.txt", "one, edited here");
        let b = id_of(&peer.store, "b.txt");
        peer.store.rename(b.id, "b-renamed.txt").unwrap();
        here.link.pass().unwrap();
        let expected = pairs(&[("a.txt", "one, edited here"), ("b-renamed.txt", "two")]);
        assert_eq!(files(&peer.store), expected);
        assert_eq!(files(&here.store), expected);

        let a = id_of(&peer.store, "a.txt");
        peer.store.delete(a.id, None, "test").unwrap();
        here.link.pass().unwrap();
        assert_eq!(files(&here.store), pairs(&[("b-renamed.txt", "two")]));
    }

    #[test]
    fn edits_on_both_sides_merge_or_keep_both() {
        let peer = serve_peer();
        let mut here = link_to(&peer);
        here.store.insert_new("list.txt", b"apples\nbread\ncheese\n", "test").unwrap();
        here.store.insert_new("pic.png", b"original", "test").unwrap();
        here.link.pass().unwrap();

        // Different lines of a text file: merged.
        edit(&here.store, "list.txt", "apples\nbread\ncheese\ndates\n");
        edit(&peer.store, "list.txt", "APPLES\nbread\ncheese\n");
        // An image changed on both: nothing to merge, keep both.
        edit(&here.store, "pic.png", "our pic");
        edit(&peer.store, "pic.png", "their pic");
        let t = here.link.pass().unwrap();
        assert_eq!((t.merged, t.kept_both), (1, 1));

        let expected = pairs(&[
            ("list.txt", "APPLES\nbread\ncheese\ndates\n"),
            ("pic (conflict copy).png", "our pic"),
            ("pic.png", "their pic"),
        ]);
        assert_eq!(files(&here.store), expected);
        assert_eq!(files(&peer.store), expected);
        here.link.pass().unwrap();
        assert!(here.link.pass().unwrap().is_empty());
    }

    #[test]
    fn an_edit_beats_a_delete() {
        let peer = serve_peer();
        let mut here = link_to(&peer);
        here.store.insert_new("note.txt", b"v1", "test").unwrap();
        here.link.pass().unwrap();

        let ours = id_of(&here.store, "note.txt");
        here.store.delete(ours.id, None, "test").unwrap();
        edit(&peer.store, "note.txt", "v2");
        here.link.pass().unwrap();
        assert_eq!(files(&here.store), pairs(&[("note.txt", "v2")]));
        assert_eq!(files(&peer.store), pairs(&[("note.txt", "v2")]));
    }

    #[test]
    fn folders_that_existed_before_linking_are_matched_up() {
        let peer = serve_peer();
        let mut here = link_to(&peer);
        here.store.insert_new("same.txt", b"identical", "test").unwrap();
        peer.store.insert_new("same.txt", b"identical", "test").unwrap();
        here.store.insert_new("todo.txt", b"ours", "test").unwrap();
        peer.store.insert_new("todo.txt", b"theirs", "test").unwrap();

        let t = here.link.pass().unwrap();
        assert_eq!(t, Tally { kept_both: 1, ..Tally::default() });
        let expected = pairs(&[("same.txt", "identical"), ("todo (conflict copy).txt", "ours"), ("todo.txt", "theirs")]);
        assert_eq!(files(&here.store), expected);
        assert_eq!(files(&peer.store), expected);
        here.link.pass().unwrap();
        assert!(here.link.pass().unwrap().is_empty());
    }

    #[test]
    fn changes_made_while_the_peer_is_off_wait_for_it() {
        let dir = tempdir::TempDir::new("mapaki-here").unwrap();
        let store = Arc::new(Store::load(dir.path().to_path_buf()).unwrap());
        // Nothing listens on this port.
        let closed = std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap();
        let cfg = PeerConfig { name: "away".into(), url: format!("http://{closed}"), token: "t".into() };
        let mut link = Link::new(cfg, store.clone(), dir.path().to_path_buf());
        store.insert_new("while-away.txt", b"x", "test").unwrap();
        assert!(matches!(link.pass(), Err(LinkError::Offline(_))));
        // Nothing was marked as done, so it goes across once the peer is back.
        assert_eq!(link.state.local_seq, 0);
    }

    #[test]
    fn a_revoked_token_is_reported_as_rejected() {
        let peer = serve_peer();
        let mut here = link_to(&peer);
        here.link.remote.token = "wrong".into();
        assert!(matches!(here.link.pass(), Err(LinkError::Rejected)));
    }

    #[test]
    fn urls_get_a_scheme_and_the_default_port() {
        assert_eq!(normalize_url("lunchbox.local"), "http://lunchbox.local:8420");
        assert_eq!(normalize_url("http://lunchbox.local:9000/"), "http://lunchbox.local:9000");
        assert_eq!(host_of("http://lunchbox.local:8420"), "lunchbox.local");
        assert_eq!(host_of("https://[fd00::1]:8420"), "fd00::1");
        assert_eq!(conflict_name("notes.txt"), "notes (conflict copy).txt");
        assert_eq!(conflict_name("README"), "README (conflict copy)");
    }
}
