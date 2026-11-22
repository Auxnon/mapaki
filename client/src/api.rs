use std::io::{BufRead, BufReader, Read};
use std::time::Duration;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Deserialize)]
pub struct Entry {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub head: String,
    #[serde(default)]
    pub hash: String,
    #[serde(default)]
    pub deleted: bool,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Revision {
    pub id: String,
    pub parents: Vec<String>,
    pub size: u64,
    pub device: String,
    pub at: i64,
    pub name: String,
    #[serde(default)]
    pub deleted: bool,
}

#[derive(Debug, Deserialize)]
pub struct Changes {
    pub seq: u64,
    pub entries: Vec<Entry>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PutResult {
    Unchanged,
    FastForward,
    Merged,
}

#[derive(Deserialize)]
struct PutResponse {
    result: PutResult,
    entry: Entry,
}

#[derive(Deserialize)]
struct ConflictBody {
    entry: Entry,
}

pub enum Outcome<T> {
    Ok(T),
    /// 409: the server's current entry, which the caller must reconcile with.
    Conflict(Entry),
}

#[derive(Debug)]
pub struct ApiError(pub String);

impl std::fmt::Display for ApiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for ApiError {}

pub type Result<T> = std::result::Result<T, ApiError>;

fn err(url: &str, e: ureq::Error) -> ApiError {
    match e {
        ureq::Error::Status(code, resp) => {
            let body = resp.into_string().unwrap_or_default();
            ApiError(format!("HTTP {code} from {url}: {body}"))
        }
        ureq::Error::Transport(t) => ApiError(format!("couldn't reach {url}: {t}")),
    }
}

pub struct Api {
    pub base: String,
    token: String,
    agent: ureq::Agent,
}

fn agent(read_timeout: Duration) -> ureq::Agent {
    ureq::AgentBuilder::new()
        .timeout_connect(Duration::from_secs(5))
        .timeout_read(read_timeout)
        .user_agent(concat!("mapaki/", env!("CARGO_PKG_VERSION")))
        .build()
}

pub fn normalize_url(url: &str) -> String {
    let url = url.trim().trim_end_matches('/');
    if url.contains("://") {
        url.to_string()
    } else {
        format!("http://{url}")
    }
}

/// True if a mapaki daemon answers at `url` within a short timeout.
pub fn reachable(url: &str) -> bool {
    ureq::AgentBuilder::new()
        .timeout(Duration::from_millis(1500))
        .build()
        .get(&format!("{url}/health"))
        .call()
        .map(|r| r.into_string().map(|s| s.trim() == "ok").unwrap_or(false))
        .unwrap_or(false)
}

pub fn pair(url: &str, otp: &str, device: &str) -> Result<String> {
    #[derive(Serialize)]
    struct Req<'a> {
        otp: &'a str,
        device: &'a str,
    }
    #[derive(Deserialize)]
    struct Resp {
        token: String,
    }
    let full = format!("{url}/pair");
    let resp: Resp = agent(Duration::from_secs(20))
        .post(&full)
        .send_json(Req { otp, device })
        .map_err(|e| err(&full, e))?
        .into_json()
        .map_err(|e| ApiError(e.to_string()))?;
    Ok(resp.token)
}

impl Api {
    pub fn new(base: &str, token: &str) -> Api {
        Api {
            base: base.to_string(),
            token: token.to_string(),
            agent: agent(Duration::from_secs(120)),
        }
    }

    fn url(&self, path: &str) -> String {
        format!("{}{path}", self.base)
    }

    fn req(&self, method: &str, path: &str) -> ureq::Request {
        self.agent
            .request(method, &self.url(path))
            .set("Authorization", &format!("Bearer {}", self.token))
    }

    fn json<T: serde::de::DeserializeOwned>(&self, resp: ureq::Response) -> Result<T> {
        resp.into_json().map_err(|e| ApiError(format!("bad response from {}: {e}", self.base)))
    }

    pub fn changes(&self, since: u64) -> Result<Changes> {
        let path = format!("/changes?since={since}");
        let resp = self.req("GET", &path).call().map_err(|e| err(&self.url(&path), e))?;
        self.json(resp)
    }

    pub fn content(&self, id: &str, rev: Option<&str>) -> Result<Vec<u8>> {
        let path = match rev {
            Some(rev) => format!("/files/{id}/content?rev={rev}"),
            None => format!("/files/{id}/content"),
        };
        let resp = self.req("GET", &path).call().map_err(|e| err(&self.url(&path), e))?;
        let mut bytes = Vec::new();
        resp.into_reader()
            .read_to_end(&mut bytes)
            .map_err(|e| ApiError(format!("download of {id} failed: {e}")))?;
        Ok(bytes)
    }

    pub fn history(&self, id: &str) -> Result<Vec<Revision>> {
        let path = format!("/files/{id}/history");
        let resp = self.req("GET", &path).call().map_err(|e| err(&self.url(&path), e))?;
        self.json(resp)
    }

    pub fn create(&self, name: &str, bytes: &[u8]) -> Result<Entry> {
        let path = format!("/files/new?name={}", encode(name));
        let resp = self.req("POST", &path).send_bytes(bytes).map_err(|e| err(&self.url(&path), e))?;
        self.json(resp)
    }

    pub fn put(&self, id: &str, bytes: &[u8], base: Option<&str>, force: bool) -> Result<Outcome<(PutResult, Entry)>> {
        let mut path = format!("/files/{id}/content?force={force}");
        if let Some(base) = base {
            path.push_str(&format!("&base={base}"));
        }
        match self.req("PUT", &path).send_bytes(bytes) {
            Ok(resp) => {
                let r: PutResponse = self.json(resp)?;
                Ok(Outcome::Ok((r.result, r.entry)))
            }
            Err(ureq::Error::Status(409, resp)) => {
                let c: ConflictBody = self.json(resp)?;
                Ok(Outcome::Conflict(c.entry))
            }
            Err(e) => Err(err(&self.url(&path), e)),
        }
    }

    pub fn rename(&self, id: &str, name: &str) -> Result<Entry> {
        let path = format!("/files/{id}");
        let resp = self
            .req("PATCH", &path)
            .send_json(serde_json::json!({ "name": name }))
            .map_err(|e| err(&self.url(&path), e))?;
        self.json(resp)
    }

    pub fn delete(&self, id: &str, base: &str) -> Result<Outcome<()>> {
        let path = format!("/files/{id}?base={base}");
        match self.req("DELETE", &path).call() {
            Ok(_) => Ok(Outcome::Ok(())),
            Err(ureq::Error::Status(409, resp)) => {
                let c: ConflictBody = self.json(resp)?;
                Ok(Outcome::Conflict(c.entry))
            }
            Err(ureq::Error::Status(404, _)) => Ok(Outcome::Ok(())),
            Err(e) => Err(err(&self.url(&path), e)),
        }
    }

    /// Blocks reading the server-sent event stream, calling `on_seq` for
    /// every change notification, until the connection drops. The read
    /// timeout is a bit over twice the server's keep-alive interval so a
    /// connection that died silently (e.g. across a suspend) gets noticed.
    /// Returns Ok once the stream has been open for `max_age`.
    pub fn listen(&self, max_age: Duration, mut on_seq: impl FnMut(u64)) -> Result<()> {
        let started = std::time::Instant::now();
        let path = "/events";
        let resp = agent(Duration::from_secs(200))
            .get(&self.url(path))
            .set("Authorization", &format!("Bearer {}", self.token))
            .set("Accept", "text/event-stream")
            .call()
            .map_err(|e| err(&self.url(path), e))?;
        let reader = BufReader::new(resp.into_reader());
        for line in reader.lines() {
            let line = line.map_err(|e| ApiError(format!("event stream closed: {e}")))?;
            if started.elapsed() > max_age {
                return Ok(());
            }
            if let Some(data) = line.strip_prefix("data:") {
                if let Ok(seq) = data.trim().parse() {
                    on_seq(seq);
                }
            }
        }
        Err(ApiError("event stream closed by server".into()))
    }
}

/// Minimal query-string percent-encoding (filenames can contain anything).
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
