use std::sync::Arc;

use axum::body::Body;
use axum::extract::{Multipart, Path, State};
use axum::http::{header, HeaderMap, HeaderValue, Request, StatusCode};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post, put};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use tokio_util::io::ReaderStream;
use uuid::Uuid;

use crate::auth::Auth;
use crate::store::{Entry, Store};

#[derive(Clone)]
pub struct AppState {
    pub store: Arc<Store>,
    pub auth: Arc<Auth>,
}

pub fn router(state: AppState) -> Router {
    let protected = Router::new()
        .route("/files", get(list_files).post(upload_file))
        .route("/files/:id", axum::routing::patch(rename_file).delete(delete_file))
        .route("/files/:id/content", get(get_content))
        .route("/order", put(set_order))
        .route_layer(middleware::from_fn_with_state(state.clone(), require_auth));

    let public = Router::new()
        .route("/health", get(health))
        .route("/pair", post(pair));

    public.merge(protected).with_state(state)
}

async fn require_auth(
    State(state): State<AppState>,
    req: Request<Body>,
    next: Next,
) -> Response {
    let ok = req
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .map(|tok| state.auth.check_token(tok))
        .unwrap_or(false);

    if ok {
        next.run(req).await
    } else {
        (StatusCode::UNAUTHORIZED, "unauthorized").into_response()
    }
}

async fn health() -> &'static str {
    "ok"
}

#[derive(Deserialize)]
struct PairRequest {
    otp: String,
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
        .redeem_pairing(&body.otp)
        .map(|token| Json(PairResponse { token }))
        .map_err(|e| (StatusCode::UNAUTHORIZED, e.to_string()))
}

async fn list_files(State(state): State<AppState>) -> Json<Vec<Entry>> {
    Json(state.store.list())
}

async fn upload_file(
    State(state): State<AppState>,
    mut multipart: Multipart,
) -> Result<(StatusCode, Json<Entry>), (StatusCode, String)> {
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
        return Err((StatusCode::BAD_REQUEST, "missing file or name".into()));
    };

    state
        .store
        .insert_new(&name, &bytes)
        .map(|e| (StatusCode::CREATED, Json(e)))
        .map_err(|e| (StatusCode::BAD_REQUEST, e.to_string()))
}

async fn get_content(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> Result<Response, StatusCode> {
    let (path, name) = state.store.content_path(id).ok_or(StatusCode::NOT_FOUND)?;
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

    Ok((headers, body).into_response())
}

#[derive(Deserialize)]
struct RenameRequest {
    name: String,
}

async fn rename_file(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
    Json(body): Json<RenameRequest>,
) -> Result<Json<Entry>, (StatusCode, String)> {
    state
        .store
        .rename(id, &body.name)
        .map(Json)
        .map_err(|e| (StatusCode::BAD_REQUEST, e.to_string()))
}

async fn delete_file(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> Result<StatusCode, (StatusCode, String)> {
    state
        .store
        .delete(id)
        .map(|_| StatusCode::NO_CONTENT)
        .map_err(|e| (StatusCode::NOT_FOUND, e.to_string()))
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
