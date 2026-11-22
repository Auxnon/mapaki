//! `mapaki run`: the long-lived sync agent.
//!
//! Remote changes arrive over the server's /events stream, so a connected
//! client costs one idle TCP connection rather than a poll loop. When the
//! server can't be reached (laptop away from home, server down) the
//! reconnect delay quadruples each failed attempt — 1 min, 4, 16, 64, capped
//! at an hour — and snaps back to the minimum on a local edit or when the
//! machine wakes from sleep. Local edits come from the OS file watcher.

use std::path::Path;
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::thread;
use std::time::{Duration, Instant, SystemTime};

use notify::{RecursiveMode, Watcher};

use crate::api::{self, Api, Entry};
use crate::config::{Config, State};
use crate::sync::{is_synced_name, BoxError, Choice, Syncer};

const MIN_RETRY: Duration = Duration::from_secs(60);
const MAX_RETRY: Duration = Duration::from_secs(60 * 60);
const BACKOFF_FACTOR: u32 = 4;
/// Quiet period after the last local file event before syncing, so a burst
/// of saves (or a big copy) becomes one pass.
const DEBOUNCE: Duration = Duration::from_secs(2);
/// Wall-clock vs. monotonic check; a gap means the machine was suspended.
const WAKE_CHECK: Duration = Duration::from_secs(60);
/// A connection held this long is re-established so a preferred URL (LAN)
/// gets picked up again after roaming in on the fallback.
const MAX_STREAM_AGE: Duration = Duration::from_secs(30 * 60);

enum Msg {
    Remote(u64),
    Local,
    Woke,
}

pub fn next_retry(current: Duration) -> Duration {
    (current * BACKOFF_FACTOR).min(MAX_RETRY)
}

fn notify_desktop(summary: &str, body: &str) {
    #[cfg(target_os = "linux")]
    let _ = std::process::Command::new("notify-send").args(["-a", "mapaki", summary, body]).spawn();
    #[cfg(target_os = "macos")]
    let _ = std::process::Command::new("osascript")
        .args(["-e", &format!("display notification {body:?} with title {summary:?}")])
        .spawn();
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    let _ = (summary, body);
}

fn pick(cfg: &Config) -> Option<Api> {
    cfg.urls.iter().find(|u| api::reachable(u)).map(|u| Api::new(u, &cfg.token))
}

/// Holds the event stream open, reconnecting with backoff. `reset_rx` cuts a
/// backoff wait short (local edit, wake from sleep).
fn stream_loop(cfg: Config, tx: Sender<Msg>, reset_rx: Receiver<()>) {
    let mut delay = MIN_RETRY;
    loop {
        if let Some(api) = pick(&cfg) {
            let started = Instant::now();
            let base = api.base.clone();
            let tx2 = tx.clone();
            match api.listen(MAX_STREAM_AGE, |seq| {
                let _ = tx2.send(Msg::Remote(seq));
            }) {
                Ok(()) => continue,
                Err(e) => eprintln!("mapaki: lost {base}: {e}"),
            }
            // A stream that stayed up a while means the server is fine; only
            // back off for repeated quick failures.
            delay = if started.elapsed() > MIN_RETRY { MIN_RETRY } else { next_retry(delay) };
        } else {
            eprintln!("mapaki: no server reachable; retrying in {}s", delay.as_secs());
            delay = next_retry(delay);
        }
        match reset_rx.recv_timeout(delay) {
            Ok(()) => {
                while reset_rx.try_recv().is_ok() {}
                delay = MIN_RETRY;
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => return,
        }
    }
}

/// Suspend stops the monotonic clock but not the wall clock.
fn wake_loop(tx: Sender<Msg>) {
    loop {
        let wall = SystemTime::now();
        let mono = Instant::now();
        thread::sleep(WAKE_CHECK);
        let wall_elapsed = wall.elapsed().unwrap_or_default();
        if wall_elapsed > mono.elapsed() + Duration::from_secs(30) && tx.send(Msg::Woke).is_err() {
            return;
        }
    }
}

fn is_relevant(event: &notify::Event, folder: &Path) -> bool {
    if event.kind.is_access() {
        return false;
    }
    event.paths.iter().any(|p| {
        p.parent() == Some(folder)
            && p.file_name().and_then(|n| n.to_str()).map(is_synced_name).unwrap_or(false)
    })
}

fn sync_once(cfg: &Config, state: &mut State) -> Result<(), BoxError> {
    let api = pick(cfg).ok_or("no server reachable")?;
    let mut defer = |name: &str, _: &Entry| -> Option<Choice> {
        notify_desktop("mapaki: conflict", &format!("\"{name}\" changed on two devices. Run `mapaki resolve`."));
        None
    };
    let report = Syncer::new(&api, &cfg.folder, state, &mut defer).run()?;
    for e in &report.errors {
        eprintln!("mapaki: {e}");
    }
    if report.changed_anything() || !report.conflicts.is_empty() {
        println!("mapaki: {} ({})", report.summary(), api.base);
    }
    Ok(())
}

pub fn run(cfg: Config) -> Result<(), BoxError> {
    std::fs::create_dir_all(&cfg.folder)?;
    let folder = cfg.folder.canonicalize()?;
    println!("mapaki {}: syncing {}", env!("CARGO_PKG_VERSION"), folder.display());

    let (tx, rx) = mpsc::channel::<Msg>();
    let (reset_tx, reset_rx) = mpsc::channel::<()>();

    let watch_tx = tx.clone();
    let watch_folder = folder.clone();
    let mut watcher = notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
        if let Ok(event) = res {
            if is_relevant(&event, &watch_folder) {
                let _ = watch_tx.send(Msg::Local);
            }
        }
    })?;
    watcher.watch(&folder, RecursiveMode::NonRecursive)?;

    {
        let cfg = cfg.clone();
        let tx = tx.clone();
        thread::spawn(move || stream_loop(cfg, tx, reset_rx));
    }
    {
        let tx = tx.clone();
        thread::spawn(move || wake_loop(tx));
    }

    let mut state = State::load();
    if let Err(e) = sync_once(&cfg, &mut state) {
        eprintln!("mapaki: initial sync failed: {e}");
    }

    let mut pending_local: Option<Instant> = None;
    loop {
        let msg = match pending_local {
            Some(at) => match rx.recv_timeout(at.saturating_duration_since(Instant::now())) {
                Ok(m) => Some(m),
                Err(RecvTimeoutError::Timeout) => None,
                Err(RecvTimeoutError::Disconnected) => return Ok(()),
            },
            None => Some(rx.recv()?),
        };
        let should_sync = match msg {
            Some(Msg::Remote(seq)) => seq != state.seq,
            Some(Msg::Local) => {
                pending_local = Some(Instant::now() + DEBOUNCE);
                let _ = reset_tx.send(());
                false
            }
            Some(Msg::Woke) => {
                let _ = reset_tx.send(());
                true
            }
            None => {
                pending_local = None;
                true
            }
        };
        if should_sync {
            if let Err(e) = sync_once(&cfg, &mut state) {
                eprintln!("mapaki: sync failed: {e}");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retry_quadruples_up_to_an_hour() {
        let mut d = MIN_RETRY;
        let mut seen = vec![d.as_secs()];
        for _ in 0..4 {
            d = next_retry(d);
            seen.push(d.as_secs());
        }
        assert_eq!(seen, vec![60, 240, 960, 3600, 3600]);
    }
}
