use std::convert::Infallible;
use std::sync::Arc;
use std::time::Duration;

use axum::body::{Body, Bytes};
use axum::extract::{DefaultBodyLimit, Multipart, Path, Query, State};
use axum::http::{header, HeaderMap, HeaderValue, Request, StatusCode};
use axum::middleware::{self, Next};
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post, put};
use axum::{Extension, Json, Router};
use futures_util::Stream;
use serde::{Deserialize, Serialize};
use tokio_stream::wrappers::WatchStream;
use tokio_stream::StreamExt;
use tokio_util::io::ReaderStream;
use uuid::Uuid;

use crate::auth::Auth;
use crate::store::{Entry, PutResult, Revision, Store, StoreError};

/// Phones share full-size photos and videos; axum's 2 MB default is far too small.
const MAX_UPLOAD_BYTES: usize = 1024 * 1024 * 1024;

#[derive(Clone)]
pub struct AppState {
    pub store: Arc<Store>,
    pub auth: Arc<Auth>,
}

/// Label of the authenticated device, recorded on the revisions it makes.
#[derive(Clone)]
struct Device(String);

pub fn router(state: AppState) -> Router {
    let protected = Router::new()
        .route("/files", get(list_files).post(upload_file))
        .route("/files/new", post(upload_raw))
        .route("/files/:id", axum::routing::patch(rename_file).delete(delete_file))
        .route("/files/:id/content", get(get_content).put(put_content))
        .route("/files/:id/history", get(get_history))
        .route("/order", put(set_order))
        .route("/changes", get(changes))
        .route("/events", get(events))
        .route_layer(middleware::from_fn_with_state(state.clone(), require_auth));

    let public = Router::new()
        .route("/health", get(health))
        .route("/version", get(version))
        .route("/pair", post(pair));

    public
        .merge(protected)
        .layer(DefaultBodyLimit::max(MAX_UPLOAD_BYTES))
        .with_state(state)
}

async fn require_auth(
    State(state): State<AppState>,
    mut req: Request<Body>,
    next: Next,
) -> Response {
    let device = req
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .and_then(|tok| state.auth.check_token(tok));

    match device {
        Some(device) => {
            req.extensions_mut().insert(Device(device));
            next.run(req).await
        }
        None => (StatusCode::UNAUTHORIZED, "unauthorized").into_response(),
    }
}

type ApiError = (StatusCode, Response);

#[derive(Serialize)]
struct ConflictBody {
    error: &'static str,
    /// The server's current version (may be a tombstone).
    entry: Entry,
}

fn store_error(e: StoreError) -> ApiError {
    let (status, resp) = match e {
        StoreError::NotFound => (StatusCode::NOT_FOUND, "not found".into_response()),
        StoreError::Invalid(m) => (StatusCode::BAD_REQUEST, m.into_response()),
        StoreError::Conflict(entry) => (
            StatusCode::CONFLICT,
            Json(ConflictBody { error: "conflict", entry }).into_response(),
        ),
        StoreError::Io(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string().into_response()),
    };
    (status, resp)
}

async fn health() -> &'static str {
    "ok"
}

async fn version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

#[derive(Deserialize)]
struct PairRequest {
    otp: String,
    #[serde(default)]
    device: Option<String>,
}

#[derive(Serialize)]
struct PairResponse {
    token: String,
}

async fn pair(
    State(state): State<AppState>,
    Json(body): Json<PairRequest>,
) -> Result<Json<PairResponse>, (StatusCode, String)> {
    state
        .auth
        .redeem_pairing(&body.otp, body.device.as_deref())
        .map(|token| Json(PairResponse { token }))
        .map_err(|e| (StatusCode::UNAUTHORIZED, e.to_string()))
}

async fn list_files(State(state): State<AppState>) -> Json<Vec<Entry>> {
    Json(state.store.list())
}

async fn upload_file(
    State(state): State<AppState>,
    Extension(Device(device)): Extension<Device>,
    mut multipart: Multipart,
) -> Result<(StatusCode, Json<Entry>), ApiError> {
    let mut bytes: Option<Vec<u8>> = None;
    let mut name: Option<String> = None;

    while let Ok(Some(field)) = multipart.next_field().await {
        match field.name().unwrap_or("") {
            "name" => {
                if let Ok(v) = field.text().await {
                    name = Some(v);
                }
            }
            "file" => {
                let field_name = field.file_name().map(|s| s.to_string());
                if name.is_none() {
                    name = field_name;
                }
                bytes = field
                    .bytes()
                    .await
                    .ok()
                    .map(|b| b.to_vec());
            }
            _ => {}
        }
    }

    let (Some(bytes), Some(name)) = (bytes, name) else {
        return Err((StatusCode::BAD_REQUEST, "missing file or name".into_response()));
    };

    state
        .store
        .insert_new(&name, &bytes, &device)
        .map(|e| (StatusCode::CREATED, Json(e)))
        .map_err(store_error)
}

#[derive(Deserialize)]
struct NewQuery {
    name: String,
}

/// Raw-body create, for clients that don't want to build multipart forms.
async fn upload_raw(
    State(state): State<AppState>,
    Extension(Device(device)): Extension<Device>,
    Query(q): Query<NewQuery>,
    body: Bytes,
) -> Result<(StatusCode, Json<Entry>), ApiError> {
    state
        .store
        .insert_new(&q.name, &body, &device)
        .map(|e| (StatusCode::CREATED, Json(e)))
        .map_err(store_error)
}

#[derive(Deserialize)]
struct ContentQuery {
    rev: Option<String>,
}

async fn get_content(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
    Query(q): Query<ContentQuery>,
) -> Result<Response, StatusCode> {
    let found = match &q.rev {
        Some(rev) => state.store.revision_content_path(id, rev),
        None => state.store.content_path(id),
    };
    let (path, name) = found.ok_or(StatusCode::NOT_FOUND)?;
    let file = tokio::fs::File::open(&path)
        .await
        .map_err(|_| StatusCode::NOT_FOUND)?;

    let mime = mime_guess::from_path(&name).first_or_octet_stream();
    let stream = ReaderStream::new(file);
    let body = Body::from_stream(stream);

    let mut headers = HeaderMap::new();
    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_str(mime.as_ref())
            .unwrap_or_else(|_| HeaderValue::from_static("application/octet-stream")),
    );
    if q.rev.is_none() {
        if let Some(entry) = state.store.get(id) {
            if let Ok(v) = HeaderValue::from_str(&entry.head) {
                headers.insert("x-mapaki-rev", v);
            }
        }
    }

    Ok((headers, body).into_response())
}

#[derive(Deserialize)]
struct PutQuery {
    base: Option<String>,
    #[serde(default)]
    force: bool,
}

#[derive(Serialize)]
struct PutResponse {
    result: PutResult,
    entry: Entry,
}

/// Upload new content for an existing file. 200 on fast-forward/merge/no-op,
/// 409 with the server's entry when the caller must choose local/remote/both.
async fn put_content(
    State(state): State<AppState>,
    Extension(Device(device)): Extension<Device>,
    Path(id): Path<Uuid>,
    Query(q): Query<PutQuery>,
    body: Bytes,
) -> Result<Json<PutResponse>, ApiError> {
    state
        .store
        .put_content(id, &body, q.base.as_deref(), &device, q.force)
        .map(|(result, entry)| Json(PutResponse { result, entry }))
        .map_err(store_error)
}

async fn get_history(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> Result<Json<Vec<Revision>>, StatusCode> {
    state.store.get(id).ok_or(StatusCode::NOT_FOUND)?;
    Ok(Json(state.store.history(id)))
}

#[derive(Deserialize)]
struct RenameRequest {
    name: String,
}

async fn rename_file(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
    Json(body): Json<RenameRequest>,
) -> Result<Json<Entry>, ApiError> {
    state
        .store
        .rename(id, &body.name)
        .map(Json)
        .map_err(store_error)
}

#[derive(Deserialize)]
struct DeleteQuery {
    base: Option<String>,
}

async fn delete_file(
    State(state): State<AppState>,
    Extension(Device(device)): Extension<Device>,
    Path(id): Path<Uuid>,
    Query(q): Query<DeleteQuery>,
) -> Result<StatusCode, ApiError> {
    state
        .store
        .delete(id, q.base.as_deref(), &device)
        .map(|_| StatusCode::NO_CONTENT)
        .map_err(store_error)
}

#[derive(Deserialize)]
struct OrderRequest {
    ids: Vec<Uuid>,
}

async fn set_order(
    State(state): State<AppState>,
    Json(body): Json<OrderRequest>,
) -> Json<Vec<Entry>> {
    Json(state.store.set_order(&body.ids))
}

#[derive(Deserialize)]
struct ChangesQuery {
    #[serde(default)]
    since: u64,
}

#[derive(Serialize)]
struct ChangesResponse {
    seq: u64,
    entries: Vec<Entry>,
}

/// Everything (tombstones included) changed after the caller's cursor.
async fn changes(
    State(state): State<AppState>,
    Query(q): Query<ChangesQuery>,
) -> Json<ChangesResponse> {
    let (seq, entries) = state.store.changes(q.since);
    Json(ChangesResponse { seq, entries })
}

/// Server-sent events: one `seq` event immediately, then one per change.
/// Clients hold this open instead of polling; the keep-alive comment is just
/// often enough to stop proxies (Cloudflare drops idle streams at ~100 s)
/// from cutting it.
async fn events(State(state): State<AppState>) -> Sse<impl Stream<Item = Result<Event, Infallible>>> {
    let stream = WatchStream::new(state.store.subscribe())
        .map(|seq| Ok(Event::default().event("seq").data(seq.to_string())));
    Sse::new(stream).keep_alive(KeepAlive::new().interval(Duration::from_secs(75)))
}
