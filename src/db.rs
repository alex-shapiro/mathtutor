//! Baserunna client.
//!
//! All per-user state lives in one Baserunna database: `BASERUNNA_DB`
//! (default `mathtutor`) on the server at `BASERUNNA_URL`, reached with
//! the API key or session token in `BASERUNNA_TOKEN`.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use chrono::{DateTime, Utc};
use reqwest::{Method, StatusCode};
use serde_json::{Value as Json, json};

use crate::{Error, Result};

pub const DEFAULT_URL: &str = "https://baserunna.fly.dev";
pub const DEFAULT_DB: &str = "mathtutor";

/// Attempts a transaction gets before a conflict or transient failure is
/// returned to the caller.
const MAX_ATTEMPTS: u32 = 10;

/// How long a client waits before asking whether an ambiguous commit
/// applied: past the server's commit deadline, no copy of it can still land.
const AMBIGUOUS_COMMIT_WAIT: Duration = Duration::from_secs(15);

/// Canonical timestamp format. The `Z` suffix and fixed width keep
/// lexicographic order aligned with chronological order, which index
/// range queries over timestamp columns rely on.
pub fn format_ts(ts: DateTime<Utc>) -> String {
    ts.format("%Y-%m-%dT%H:%M:%S%.3fZ").to_string()
}

/// Parse a timestamp written by [`format_ts`] (or any RFC 3339 form).
pub fn parse_ts(s: &str) -> Result<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(s)
        .map(|dt| dt.with_timezone(&Utc))
        .map_err(|e| Error::BadTimestamp(format!("{s}: {e}")))
}

/// Where the user database lives.
#[derive(Debug, Clone)]
pub struct DbConfig {
    pub url: String,
    pub token: String,
    pub db: String,
    /// Directory keeping a copy of the overlay between processes, so a
    /// command downloads it only after it changed. `None` keeps it in memory.
    pub cache_dir: Option<PathBuf>,
}

impl DbConfig {
    /// Reads `BASERUNNA_URL`, `BASERUNNA_TOKEN` (required) and `BASERUNNA_DB`.
    pub fn from_env() -> Result<Self> {
        let var = |k: &str| std::env::var(k).ok().filter(|v| !v.is_empty());
        Ok(Self {
            url: var("BASERUNNA_URL").unwrap_or_else(|| DEFAULT_URL.into()),
            token: var("BASERUNNA_TOKEN").ok_or(Error::MissingToken)?,
            db: var("BASERUNNA_DB").unwrap_or_else(|| DEFAULT_DB.into()),
            cache_dir: crate::path::mt_home().ok(),
        })
    }
}

/// One HTTP request to the server, as an [`Observer`] sees it.
#[derive(Debug, Clone)]
pub struct Sample {
    /// `begin`, `ops`, `commit`, `abort`, `txn` (one-shot), `status` or `other`.
    pub kind: &'static str,
    pub status: u16,
    /// Time from sending the request to reading the whole response.
    pub elapsed: Duration,
    /// The server's own account of the request, from its `Server-Timing` header.
    pub server: Option<ServerTiming>,
    pub sent_bytes: usize,
    pub received_bytes: usize,
}

/// Where a request spent its time on the server.
#[derive(Debug, Clone, Copy, Default)]
pub struct ServerTiming {
    pub total: Duration,
    /// Object-store calls, summed.
    pub store: Duration,
    pub store_calls: u32,
    /// Waiting for commits.
    pub commit: Duration,
    /// Waiting for an in-flight manifest write before a strong read.
    pub wait: Duration,
}

impl ServerTiming {
    /// Parse a `Server-Timing` header value.
    pub fn parse(v: &str) -> Self {
        let mut t = Self::default();
        for metric in v.split(',') {
            let mut parts = metric.trim().split(';');
            let name = parts.next().unwrap_or_default();
            let (mut dur, mut desc) = (Duration::ZERO, 0);
            for p in parts {
                if let Some(ms) = p.strip_prefix("dur=").and_then(|d| d.parse::<f64>().ok()) {
                    dur = Duration::from_secs_f64(ms / 1000.0);
                } else if let Some(n) = p.strip_prefix("desc=").and_then(|d| d.parse().ok()) {
                    desc = n;
                }
            }
            match name {
                "total" => t.total = dur,
                "store" => (t.store, t.store_calls) = (dur, desc),
                "commit" => t.commit = dur,
                "wait" => t.wait = dur,
                _ => {}
            }
        }
        t
    }
}

/// Called with every request a [`Db`] makes.
pub type Observer = Arc<dyn Fn(Sample) + Send + Sync>;

/// A handle on the user database. Cheap to clone; clones share one
/// connection pool.
#[derive(Clone)]
pub struct Db {
    http: reqwest::Client,
    url: String,
    base: String,
    token: String,
    observer: Option<Observer>,
    /// The path the last transaction resolved, so the next one can read
    /// that path's state speculatively in its first request.
    pub(crate) path_hint: Arc<Mutex<Option<String>>>,
    /// The overlay as last read, validated by its version in later transactions.
    pub(crate) overlay_cache: Arc<Mutex<Option<crate::view::CachedOverlay>>>,
    /// Where the overlay cache is kept between processes.
    pub(crate) cache_file: Option<PathBuf>,
}

impl std::fmt::Debug for Db {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Db")
            .field("base", &self.base)
            .finish_non_exhaustive()
    }
}

/// A failure reported by the server.
#[derive(Debug, Clone)]
pub struct ApiError {
    pub status: u16,
    pub code: String,
    pub message: String,
    pub retryable: bool,
    /// A commit that may or may not have applied.
    pub outcome_unknown: bool,
    /// The answer came from Baserunna, not from a proxy in front of it, so
    /// its status says whether the request applied.
    pub from_server: bool,
}

impl std::fmt::Display for ApiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} {}: {}", self.status, self.code, self.message)
    }
}

impl ApiError {
    /// True when the request was certainly not applied and can be retried.
    fn retry_not_applied(&self) -> bool {
        matches!(self.status, 404 if self.code == "txn_not_found")
            || (self.retryable && !self.outcome_unknown)
    }
}

impl Db {
    /// Connect to the configured database, creating it and its schema if
    /// they do not exist yet.
    pub async fn open(cfg: &DbConfig) -> Result<Self> {
        let db = Self::connect(cfg)?;
        crate::schema::ensure(&db, &cfg.db).await?;
        Ok(db)
    }

    /// A handle without any round trip; the database must already exist.
    pub fn connect(cfg: &DbConfig) -> Result<Self> {
        let http = reqwest::Client::builder()
            .user_agent(concat!("mathtutor/", env!("CARGO_PKG_VERSION")))
            .connect_timeout(Duration::from_secs(10))
            .timeout(Duration::from_mins(1))
            .pool_idle_timeout(Duration::from_secs(50))
            .build()?;
        let url = cfg.url.trim_end_matches('/').to_string();
        let cache_file = cfg
            .cache_dir
            .as_ref()
            .map(|d| d.join(cache_name(&url, &cfg.db)));
        let cached = cache_file
            .as_deref()
            .and_then(crate::view::read_cached_overlay);
        Ok(Self {
            base: format!("{url}/v1/dbs/{}", cfg.db),
            url,
            http,
            token: cfg.token.clone(),
            observer: None,
            path_hint: Arc::default(),
            overlay_cache: Arc::new(Mutex::new(cached)),
            cache_file,
        })
    }

    /// Report every request this handle and its clones make to `observer`.
    #[must_use]
    pub fn observed(mut self, observer: Observer) -> Self {
        self.observer = Some(observer);
        self
    }

    /// Send one request to `path` under the server root and return its JSON body.
    pub(crate) async fn root_call(
        &self,
        method: Method,
        path: &str,
        body: Option<&Json>,
    ) -> Result<Json> {
        self.send(method, &format!("{}{path}", self.url), body)
            .await
    }

    /// Send one request to `path` under this database and return its JSON body.
    pub(crate) async fn call(
        &self,
        method: Method,
        path: &str,
        body: Option<&Json>,
    ) -> Result<Json> {
        self.send(method, &format!("{}{path}", self.base), body)
            .await
    }

    async fn send(&self, method: Method, url: &str, body: Option<&Json>) -> Result<Json> {
        let started = std::time::Instant::now();
        let kind = request_kind(&method, url);
        let mut req = self.http.request(method, url).bearer_auth(&self.token);
        let mut sent_bytes = 0;
        if let Some(b) = body {
            let bytes = serde_json::to_vec(b)?;
            sent_bytes = bytes.len();
            req = req
                .header(reqwest::header::CONTENT_TYPE, "application/json")
                .body(bytes);
        }
        let resp = req.send().await?;
        let status = resp.status();
        let server = resp
            .headers()
            .get("server-timing")
            .and_then(|v| v.to_str().ok())
            .map(ServerTiming::parse);
        let bytes = resp.bytes().await?;
        if let Some(observe) = &self.observer {
            observe(Sample {
                kind,
                status: status.as_u16(),
                elapsed: started.elapsed(),
                server,
                sent_bytes,
                received_bytes: bytes.len(),
            });
        }
        if status.is_success() {
            if bytes.is_empty() {
                return Ok(Json::Null);
            }
            return Ok(serde_json::from_slice(&bytes)?);
        }
        Err(Error::Api(api_error(status, &bytes)))
    }

    /// Run read-only `ops` in one request against a strong snapshot.
    pub async fn read(&self, ops: &[Json]) -> Result<Vec<Json>> {
        let body = json!({ "ops": ops });
        let r = retry(|| self.call(Method::POST, "/txn", Some(&body))).await?;
        results(r)
    }

    /// Run `ops`, writes included, as one transaction in one request.
    /// Retried only while certainly not applied.
    pub async fn write(&self, ops: &[Json]) -> Result<Vec<Json>> {
        let body = json!({ "ops": ops });
        results(retry(|| self.call(Method::POST, "/txn", Some(&body))).await?)
    }

    /// Begin an interactive transaction, running `ops` in it.
    pub async fn begin(&self, ops: &[Json]) -> Result<(Txn, Vec<Json>)> {
        let r = self
            .call(Method::POST, "/transactions", Some(&json!({ "ops": ops })))
            .await?;
        let txn = Txn {
            db: self.clone(),
            id: str_field(&r, "txn_id")?,
            commit_id: str_field(&r, "commit_id")?,
        };
        Ok((txn, results(r)?))
    }
}

/// An open interactive transaction. It is aborted server-side when idle;
/// dropping it without committing leaves nothing behind.
pub struct Txn {
    db: Db,
    id: String,
    commit_id: String,
}

impl Txn {
    /// Run more `ops` in the transaction. Any error aborts it.
    pub async fn ops(&self, ops: &[Json]) -> Result<Vec<Json>> {
        let path = format!("/transactions/{}/ops", self.id);
        results(
            self.db
                .call(Method::POST, &path, Some(&json!({ "ops": ops })))
                .await?,
        )
    }

    /// Run `ops` and commit. A commit whose outcome is unknown is resolved
    /// before returning, so `Ok` means applied and a retryable error
    /// means not applied.
    pub async fn commit(self, ops: &[Json]) -> Result<Vec<Json>> {
        let path = format!("/transactions/{}/commit", self.id);
        match self
            .db
            .call(Method::POST, &path, Some(&json!({ "ops": ops })))
            .await
        {
            Ok(r) => results(r),
            Err(Error::Api(e)) if e.from_server && !e.outcome_unknown => Err(Error::Api(e)),
            Err(Error::Http(e)) if e.is_connect() => Err(Error::Http(e)),
            // A read-only commit applies nothing, so a lost answer is simply retried.
            Err(Error::Http(_)) if ops.is_empty() => {
                Err(not_applied("read-only commit lost its answer"))
            }
            // A lost response, a proxy's error or an unknown outcome: wait out any copy of
            // the commit still in flight, then ask whether it applied.
            Err(_) => {
                tokio::time::sleep(AMBIGUOUS_COMMIT_WAIT).await;
                let path = format!("/commits/{}", self.commit_id);
                let status = retry(|| self.db.call(Method::GET, &path, None)).await?;
                if status["status"] == "committed" {
                    // The results of a commit are only in its lost response.
                    Ok(Vec::new())
                } else {
                    Err(not_applied("commit did not apply"))
                }
            }
        }
    }

    /// Abort the transaction. Failures are ignored: an abandoned
    /// transaction expires on its own.
    pub async fn abort(self) {
        let path = format!("/transactions/{}", self.id);
        let _ = self.db.call(Method::DELETE, &path, None).await;
    }
}

/// True for failures worth running a whole transaction again: conflicts,
/// rate limits, lost transactions and unavailability that did not apply.
pub fn is_retryable(e: &Error) -> bool {
    match e {
        Error::Api(a) => a.retry_not_applied(),
        Error::Http(h) => h.is_connect(),
        _ => false,
    }
}

/// Run `f` until it succeeds, fails permanently, or runs out of attempts,
/// backing off between attempts.
pub async fn retry<T, F, Fut>(mut f: F) -> Result<T>
where
    F: FnMut() -> Fut,
    Fut: Future<Output = Result<T>>,
{
    let mut attempt = 0;
    loop {
        match f().await {
            Err(e) if is_retryable(&e) && attempt + 1 < MAX_ATTEMPTS => {
                attempt += 1;
                tokio::time::sleep(backoff(attempt)).await;
            }
            r => return r,
        }
    }
}

/// Exponential backoff with full jitter: up to 10 ms, 20 ms, ... 1 s.
pub fn backoff(attempt: u32) -> Duration {
    let cap = 10u64 << attempt.min(7);
    Duration::from_millis(rand_below(cap.min(1000)) + 1)
}

fn rand_below(n: u64) -> u64 {
    use std::hash::{BuildHasher, RandomState};
    RandomState::new().hash_one(std::time::Instant::now()) % n.max(1)
}

/// Cache file name for database `db` on the server at `url`.
fn cache_name(url: &str, db: &str) -> String {
    let host: String = url
        .split("://")
        .last()
        .unwrap_or(url)
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '.' || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect();
    format!("overlay-{host}-{db}.json")
}

fn request_kind(method: &Method, url: &str) -> &'static str {
    let path = url.split('?').next().unwrap_or(url);
    if path.ends_with("/txn") {
        "txn"
    } else if path.ends_with("/transactions") {
        "begin"
    } else if path.ends_with("/ops") {
        "ops"
    } else if path.ends_with("/commit") {
        "commit"
    } else if path.contains("/transactions/") && method == Method::DELETE {
        "abort"
    } else if path.contains("/commits/") {
        "status"
    } else {
        "other"
    }
}

/// A retryable failure of a request that certainly did not apply.
fn not_applied(message: &str) -> Error {
    Error::Api(ApiError {
        status: 503,
        code: "unavailable".into(),
        message: message.into(),
        retryable: true,
        outcome_unknown: false,
        from_server: true,
    })
}

fn api_error(status: StatusCode, body: &[u8]) -> ApiError {
    let j: Json = serde_json::from_slice(body).unwrap_or(Json::Null);
    let e = &j["error"];
    ApiError {
        status: status.as_u16(),
        code: e["code"].as_str().unwrap_or("unknown").to_string(),
        message: e["message"].as_str().map_or_else(
            || String::from_utf8_lossy(body).into_owned(),
            str::to_string,
        ),
        retryable: e["retryable"].as_bool().unwrap_or(status.is_server_error()),
        outcome_unknown: e["details"]["outcome"] == "unknown",
        from_server: e.is_object(),
    }
}

fn results(mut r: Json) -> Result<Vec<Json>> {
    match r["results"].take() {
        Json::Array(v) => Ok(v),
        Json::Null => Ok(Vec::new()),
        other => Err(Error::BadResponse(format!("results: {other}"))),
    }
}

fn str_field(r: &Json, k: &str) -> Result<String> {
    r[k].as_str()
        .map(str::to_string)
        .ok_or_else(|| Error::BadResponse(format!("missing {k} in {r}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_baserunna_errors_say_whether_a_request_applied() {
        let body = br#"{"error":{"code":"conflict","message":"m","retryable":true}}"#;
        let server = api_error(StatusCode::CONFLICT, body);
        assert!(server.from_server && server.retryable && !server.outcome_unknown);

        let unknown = br#"{"error":{"code":"unavailable","message":"m","retryable":true,"details":{"outcome":"unknown"}}}"#;
        assert!(api_error(StatusCode::SERVICE_UNAVAILABLE, unknown).outcome_unknown);

        let proxy = api_error(StatusCode::BAD_GATEWAY, b"<html>502 Bad Gateway</html>");
        assert!(
            !proxy.from_server,
            "a proxy's page says nothing about the commit"
        );
        assert!(proxy.retryable);
    }

    #[test]
    fn server_timing_parses_every_metric() {
        let t = ServerTiming::parse(
            "total;dur=80.5, store;dur=75.2;desc=2, commit;dur=74.9, wait;dur=1.5",
        );
        assert_eq!(t.total, Duration::from_micros(80_500));
        assert_eq!((t.store, t.store_calls), (Duration::from_micros(75_200), 2));
        assert_eq!(t.commit, Duration::from_micros(74_900));
        assert_eq!(t.wait, Duration::from_micros(1_500));
    }
}
