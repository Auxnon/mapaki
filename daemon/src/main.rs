mod auth;
mod routes;
mod store;

use std::path::PathBuf;
use std::sync::Arc;

use routes::AppState;

fn mapaki_dir() -> PathBuf {
    if let Ok(dir) = std::env::var("MAPAKI_DIR") {
        return PathBuf::from(dir);
    }
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
    PathBuf::from(home).join("mapaki")
}

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt::init();

    let dir = mapaki_dir();
    let port: u16 = std::env::var("MAPAKI_PORT")
        .ok()
        .and_then(|p| p.parse().ok())
        .unwrap_or(8420);

    let store = Arc::new(store::Store::load(dir.clone()).expect("failed to load mapaki dir"));
    let auth = Arc::new(auth::Auth::load(dir.join(".mapaki-tokens.json")));

    let otp = auth.start_pairing();
    println!("mapaki-daemon serving {}", dir.display());
    println!("pairing code (valid 10 minutes, single use): {otp}");
    println!("enter it once in the app to pair; the app caches the resulting token after that");

    let app = routes::router(AppState { store, auth });

    let addr = std::net::SocketAddr::from(([0, 0, 0, 0], port));
    println!("listening on {addr}");
    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .expect("failed to bind");
    axum::serve(listener, app).await.expect("server error");
}
