mod auth;
mod routes;
mod store;

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use routes::AppState;

/// How often the server folder is checked for files edited or dropped in by
/// hand. Only a stat per file unless something actually changed.
const RESCAN_INTERVAL: Duration = Duration::from_secs(30);

const USAGE: &str = "\
mapaki-daemon — central store for mapaki clients

USAGE:
    mapaki-daemon              serve (MAPAKI_DIR, MAPAKI_PORT from the environment)
    mapaki-daemon pair-code    print a fresh one-time pairing code for a new device
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

fn auth_for(dir: &std::path::Path) -> auth::Auth {
    auth::Auth::load(dir.join(".mapaki-tokens.json"))
}

#[tokio::main]
async fn main() {
    match std::env::args().nth(1).as_deref() {
        None | Some("serve") => {}
        Some("pair-code") => {
            let dir = mapaki_dir();
            std::fs::create_dir_all(&dir).expect("failed to create mapaki dir");
            let otp = auth_for(&dir).start_pairing();
            println!("{otp}");
            eprintln!("valid 10 minutes, single use — enter it in the new device's mapaki app/client");
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
    let port: u16 = std::env::var("MAPAKI_PORT")
        .ok()
        .and_then(|p| p.parse().ok())
        .unwrap_or(8420);

    let store = Arc::new(store::Store::load(dir.clone()).expect("failed to load mapaki dir"));
    let auth = Arc::new(auth_for(&dir));

    let otp = auth.start_pairing();
    println!("mapaki-daemon {} serving {}", env!("CARGO_PKG_VERSION"), dir.display());
    println!("pairing code (valid 10 minutes, single use): {otp}");
    println!("enter it once in the app to pair; run `mapaki-daemon pair-code` later for more devices");

    let rescan_store = store.clone();
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(RESCAN_INTERVAL);
        tick.tick().await;
        loop {
            tick.tick().await;
            let s = rescan_store.clone();
            match tokio::task::spawn_blocking(move || s.rescan(false)).await {
                Ok(Err(e)) => tracing::warn!("rescan failed: {e}"),
                Err(e) => tracing::warn!("rescan task failed: {e}"),
                _ => {}
            }
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
