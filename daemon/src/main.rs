mod auth;
mod routes;
mod store;

use std::net::{IpAddr, TcpStream, UdpSocket};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use routes::AppState;

/// Files edited or dropped into the server folder by hand are noticed by the
/// OS file watcher right away. This slower full check is the fallback for
/// anything the watcher can't see (network filesystems, a full event queue).
/// Only a stat per file unless something actually changed.
const RESCAN_INTERVAL: Duration = Duration::from_secs(30);

/// Watcher events are gathered for this long before rescanning, so a burst
/// (a copy writing many blocks) becomes one rescan.
const EVENT_QUIET: Duration = Duration::from_millis(300);

const USAGE: &str = "\
mapaki-daemon — central store for mapaki clients

USAGE:
    mapaki-daemon              serve (MAPAKI_DIR, MAPAKI_PORT from the environment)
    mapaki-daemon pair-code    print a fresh one-time pairing code for a new device
    mapaki-daemon status       is it running, where to reach it, and the files it holds
    mapaki-daemon devices      list paired devices
    mapaki-daemon revoke <number|name>
                               unpair a device; its access stops right away
    mapaki-daemon --version

ENVIRONMENT:
    MAPAKI_DIR   folder to store files in (default: ~/mapaki)
    MAPAKI_PORT  port to listen on (default: 8420)";

fn mapaki_dir() -> PathBuf {
    if let Ok(dir) = std::env::var("MAPAKI_DIR") {
        return PathBuf::from(dir);
    }
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
    PathBuf::from(home).join("mapaki")
}

fn auth_for(dir: &Path) -> auth::Auth {
    auth::Auth::load(dir.join(".mapaki-tokens.json"))
}

fn port() -> u16 {
    std::env::var("MAPAKI_PORT").ok().and_then(|p| p.parse().ok()).unwrap_or(8420)
}

/// This machine's address on the local network: the one other devices pair
/// with. Connecting a UDP socket only picks a route; nothing is sent.
fn lan_ip() -> Option<IpAddr> {
    let socket = UdpSocket::bind("0.0.0.0:0").ok()?;
    socket.connect("192.0.2.1:80").ok()?;
    Some(socket.local_addr().ok()?.ip())
}

fn server_url() -> String {
    let host = lan_ip().map_or_else(|| "<this-machine>".to_string(), |ip| ip.to_string());
    format!("http://{host}:{}", port())
}

fn is_serving() -> bool {
    TcpStream::connect_timeout(&([127, 0, 0, 1], port()).into(), Duration::from_millis(500)).is_ok()
}

fn unix_now() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs() as i64)
}

fn ago(unix: i64) -> String {
    if unix <= 0 {
        return "unknown".into();
    }
    let secs = (unix_now() - unix).max(0);
    let (n, unit) = match secs {
        0..60 => return "just now".into(),
        60..3600 => (secs / 60, "minute"),
        3600..86400 => (secs / 3600, "hour"),
        _ => (secs / 86400, "day"),
    };
    format!("{n} {unit}{} ago", if n == 1 { "" } else { "s" })
}

fn human_size(n: u64) -> String {
    match n {
        0..1024 => format!("{n} B"),
        1024..1_048_576 => format!("{:.0} KB", n as f64 / 1024.0),
        1_048_576..1_073_741_824 => format!("{:.1} MB", n as f64 / 1_048_576.0),
        _ => format!("{:.1} GB", n as f64 / 1_073_741_824.0),
    }
}

fn print_pair_code(otp: &str) {
    println!("pairing code: {otp}   (valid 10 minutes, works once)");
    println!("  app:      enter the code; the server is {}", server_url());
    println!("  desktop:  mapaki pair {} {otp}", server_url());
}

fn cmd_status(dir: &Path) {
    let entries = store::Store::read_entries(dir);
    let total: u64 = entries.iter().map(|e| e.size).sum();
    let devices = auth_for(dir).devices().len();
    println!("mapaki-daemon {}", env!("CARGO_PKG_VERSION"));
    println!("folder:   {}", dir.display());
    if is_serving() {
        println!("server:   running at {}", server_url());
    } else {
        println!("server:   not running (port {} is free)", port());
    }
    println!("devices:  {devices} paired (mapaki-daemon devices)");
    println!("files:    {}, {}", entries.len(), human_size(total));
    if entries.is_empty() {
        return;
    }
    let width = entries.iter().map(|e| e.name.chars().count()).max().unwrap_or(0).clamp(4, 48);
    println!();
    println!("  {:<width$}  {:>8}  CHANGED", "NAME", "SIZE");
    for e in &entries {
        let name: String = if e.name.chars().count() > width {
            e.name.chars().take(width - 1).chain(['…']).collect()
        } else {
            e.name.clone()
        };
        let changed = std::fs::metadata(dir.join(&e.name))
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
            .map_or_else(|| "missing".into(), |d| ago(d.as_secs() as i64));
        println!("  {name:<width$}  {:>8}  {changed}", human_size(e.size));
    }
}

fn cmd_devices(dir: &Path) {
    let devices = auth_for(dir).devices();
    if devices.is_empty() {
        println!("no devices paired; run `mapaki-daemon pair-code` to add one");
        return;
    }
    let width = devices.iter().map(|d| d.label.chars().count()).max().unwrap_or(0).max(6);
    println!("  #  {:<width$}  {:<16}  LAST SEEN", "DEVICE", "PAIRED");
    for (i, d) in devices.iter().enumerate() {
        println!("{:>3}  {:<width$}  {:<16}  {}", i + 1, d.label, ago(d.paired_at), ago(d.last_seen));
    }
    println!("\nunpair one with `mapaki-daemon revoke <#>`");
}

/// Wakes the rescan loop whenever something in the folder (not our own dotfiles) changes.
fn watch_folder(dir: &Path, wake: tokio::sync::mpsc::UnboundedSender<()>) -> notify::Result<notify::RecommendedWatcher> {
    use notify::Watcher;
    let mut watcher = notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
        let Ok(event) = res else { return };
        let ours = |p: &PathBuf| {
            p.file_name().and_then(|n| n.to_str()).is_some_and(|n| n.starts_with('.') || n.ends_with(".mapaki-tmp"))
        };
        if !event.kind.is_access() && !event.paths.iter().all(ours) {
            let _ = wake.send(());
        }
    })?;
    watcher.watch(dir, notify::RecursiveMode::NonRecursive)?;
    Ok(watcher)
}

#[tokio::main]
async fn main() {
    match std::env::args().nth(1).as_deref() {
        None | Some("serve") => {}
        Some("pair-code") => {
            let dir = mapaki_dir();
            std::fs::create_dir_all(&dir).expect("failed to create mapaki dir");
            print_pair_code(&auth_for(&dir).start_pairing());
            if !is_serving() {
                eprintln!("note: the server isn't running; start it before pairing");
            }
            return;
        }
        Some("status") => return cmd_status(&mapaki_dir()),
        Some("devices") => return cmd_devices(&mapaki_dir()),
        Some("revoke") => {
            let Some(which) = std::env::args().nth(2) else {
                eprintln!("usage: mapaki-daemon revoke <number|name>   (numbers from `mapaki-daemon devices`)");
                std::process::exit(2);
            };
            match auth_for(&mapaki_dir()).revoke(&which) {
                Ok(label) => println!("unpaired {label}; it can't sync until it pairs again"),
                Err(e) => {
                    eprintln!("{e}");
                    std::process::exit(1);
                }
            }
            return;
        }
        Some("--version" | "-V" | "version") => {
            println!("mapaki-daemon {}", env!("CARGO_PKG_VERSION"));
            return;
        }
        Some(_) => {
            println!("{USAGE}");
            return;
        }
    }

    tracing_subscriber::fmt::init();

    let dir = mapaki_dir();
    let port = port();

    let store = Arc::new(store::Store::load(dir.clone()).expect("failed to load mapaki dir"));
    let auth = Arc::new(auth_for(&dir));

    println!("mapaki-daemon {} serving {}", env!("CARGO_PKG_VERSION"), dir.display());
    print_pair_code(&auth.start_pairing());
    println!("more devices later: mapaki-daemon pair-code");

    let (wake, mut woken) = tokio::sync::mpsc::unbounded_channel();
    let watcher = watch_folder(&dir, wake)
        .inspect_err(|e| tracing::warn!("can't watch {}: {e}; checking every {}s instead", dir.display(), RESCAN_INTERVAL.as_secs()))
        .ok();
    let rescan_store = store.clone();
    tokio::spawn(async move {
        let _watcher = watcher;
        let mut fallback = tokio::time::interval(RESCAN_INTERVAL);
        fallback.tick().await;
        // Set while a file is still being written: look again once it may have settled.
        let mut retry: Option<tokio::time::Instant> = None;
        loop {
            let retry_due = async {
                match retry {
                    Some(at) => tokio::time::sleep_until(at).await,
                    None => std::future::pending().await,
                }
            };
            tokio::select! {
                _ = fallback.tick() => {}
                _ = retry_due => {}
                Some(()) = woken.recv() => {
                    tokio::time::sleep(EVENT_QUIET).await;
                    while woken.try_recv().is_ok() {}
                }
            }
            let s = rescan_store.clone();
            retry = match tokio::task::spawn_blocking(move || s.rescan(false)).await {
                Ok(Ok(r)) => {
                    if r.changed {
                        tracing::info!("picked up changes made in the folder");
                    }
                    r.unsettled.then(|| tokio::time::Instant::now() + store::SETTLE)
                }
                Ok(Err(e)) => {
                    tracing::warn!("rescan failed: {e}");
                    None
                }
                Err(e) => {
                    tracing::warn!("rescan task failed: {e}");
                    None
                }
            };
        }
    });

    let app = routes::router(AppState { store, auth });

    let addr = std::net::SocketAddr::from(([0, 0, 0, 0], port));
    println!("listening on {addr}");
    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .expect("failed to bind");
    axum::serve(listener, app).await.expect("server error");
}
