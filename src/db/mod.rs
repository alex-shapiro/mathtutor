//! Baserunna client.
//!
//! All per-user state lives in one Baserunna database: `BASERUNNA_DB`
//! (default `mathtutor`) on the server at `BASERUNNA_URL`, reached with
//! the API key or session token in `BASERUNNA_TOKEN`.

mod op;
pub mod ts;

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use reqwest::{Method, StatusCode};
use serde::de::{DeserializeOwned, IgnoredAny};
use serde::{Deserialize, Serialize};

pub use op::{KvItem, Op, OpResult, PAGE, Row};

use crate::{Error, Result};

pub const DEFAULT_URL: &str = "https://baserunna.fly.dev";
pub const DEFAULT_DB: &str = "mathtutor";

/// Attempts a transaction gets before a conflict or transient failure is
/// returned to the caller.
const MAX_ATTEMPTS: u32 = 10;

/// How long a client waits before asking whether an ambiguous commit
/// applied: past the server's commit deadline, no copy of it can still land.
const AMBIGUOUS_COMMIT_WAIT: Duration = Duration::from_secs(15);

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

/// What a request does.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum RequestKind {
    /// A one-shot transaction.
    Txn,
    /// Beginning an interactive transaction.
    Begin,
    /// More ops in an interactive transaction.
    Ops,
    Commit,
    Abort,
    /// Asking whether a commit applied.
    Status,
    /// Creating the database or a table.
    Create,
}

impl std::fmt::Display for RequestKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.pad(match self {
            Self::Txn => "txn",
            Self::Begin => "begin",
            Self::Ops => "ops",
            Self::Commit => "commit",
            Self::Abort => "abort",
            Self::Status => "status",
            Self::Create => "create",
        })
    }
}

/// One HTTP request to the server, as an [`Observer`] sees it.
#[derive(Debug, Clone)]
pub struct Sample {
    pub kind: RequestKind,
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
    name: String,
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
            .field("url", &self.url)
            .field("name", &self.name)
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

    fn from_response(status: StatusCode, body: &[u8]) -> Self {
        #[derive(Deserialize)]
        struct Body {
            error: Detail,
        }
        #[derive(Deserialize)]
        struct Detail {
            code: String,
            message: String,
            retryable: bool,
            #[serde(default)]
            details: Details,
        }
        #[derive(Deserialize, Default)]
        struct Details {
            outcome: Option<String>,
        }
        match serde_json::from_slice::<Body>(body) {
            Ok(Body { error: e }) => Self {
                status: status.as_u16(),
                code: e.code,
                message: e.message,
                retryable: e.retryable,
                outcome_unknown: e.details.outcome.as_deref() == Some("unknown"),
                from_server: true,
            },
            Err(_) => Self {
                status: status.as_u16(),
                code: "unknown".into(),
                message: String::from_utf8_lossy(body).into_owned(),
                retryable: status.is_server_error(),
                outcome_unknown: false,
                from_server: false,
            },
        }
    }

    /// A retryable failure of a request that certainly did not apply.
    fn not_applied(message: &str) -> Error {
        Error::Api(Self {
            status: 503,
            code: "unavailable".into(),
            message: message.into(),
            retryable: true,
            outcome_unknown: false,
            from_server: true,
        })
    }
}

#[derive(Serialize)]
struct Ops<'a> {
    ops: &'a [Op],
}

#[derive(Deserialize)]
struct Results {
    #[serde(default)]
    results: Option<Vec<OpResult>>,
}

#[derive(Deserialize)]
struct Begun {
    txn_id: String,
    commit_id: String,
    results: Vec<OpResult>,
}

#[derive(Deserialize)]
struct CommitStatus {
    status: CommitState,
}

#[derive(Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum CommitState {
    Committed,
    NotCommitted,
}

impl Db {
    /// Connect to the configured database, creating it and its schema if
    /// they do not exist yet.
    pub async fn open(cfg: &DbConfig) -> Result<Self> {
        let db = Self::connect(cfg)?;
        crate::schema::ensure(&db).await?;
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
            url,
            name: cfg.db.clone(),
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

    /// Run read-only `ops` in one request against a strong snapshot.
    pub async fn read(&self, ops: &[Op]) -> Result<Vec<OpResult>> {
        let r: Results = retry(|| self.run(RequestKind::Txn, "/txn", ops)).await?;
        one_per_op(ops, r.results.unwrap_or_default())
    }

    /// Run one read-only op against a strong snapshot.
    pub async fn read_one(&self, op: Op) -> Result<OpResult> {
        let mut r = self.read(std::slice::from_ref(&op)).await?;
        Ok(r.swap_remove(0))
    }

    /// Run `ops`, writes included, as one transaction in one request.
    /// Retried only while certainly not applied.
    pub async fn write(&self, ops: &[Op]) -> Result<()> {
        retry(|| self.run::<IgnoredAny>(RequestKind::Txn, "/txn", ops)).await?;
        Ok(())
    }

    /// Begin an interactive transaction, running `ops` in it.
    pub async fn begin(&self, ops: &[Op]) -> Result<(Txn, Vec<OpResult>)> {
        let r: Begun = self.run(RequestKind::Begin, "/transactions", ops).await?;
        let txn = Txn {
            db: self.clone(),
            id: r.txn_id,
            commit_id: r.commit_id,
        };
        Ok((txn, one_per_op(ops, r.results)?))
    }

    /// Create this database. Succeeds if it already exists.
    pub(crate) async fn create_database(&self) -> Result<()> {
        #[derive(Serialize)]
        struct NewDb<'a> {
            name: &'a str,
            #[serde(skip_serializing_if = "Option::is_none")]
            org: Option<&'a str>,
        }
        let body = match self.name.split_once('.') {
            Some((org, name)) => NewDb {
                name,
                org: Some(org),
            },
            None => NewDb {
                name: &self.name,
                org: None,
            },
        };
        let url = format!("{}/v1/dbs", self.url);
        exists_ok(
            self.send(RequestKind::Create, Method::POST, &url, Some(&body))
                .await,
        )
    }

    /// Create a table from its schema. Succeeds if it already exists.
    pub(crate) async fn create_table(&self, schema: &impl Serialize) -> Result<()> {
        exists_ok(
            self.call(RequestKind::Create, Method::POST, "/tables", Some(schema))
                .await,
        )
    }

    /// Send `body` to `path` under this database.
    async fn call<T: DeserializeOwned>(
        &self,
        kind: RequestKind,
        method: Method,
        path: &str,
        body: Option<&(impl Serialize + ?Sized)>,
    ) -> Result<T> {
        let url = format!("{}/v1/dbs/{}{path}", self.url, self.name);
        self.send(kind, method, &url, body).await
    }

    /// Run `ops` at `path` under this database.
    async fn run<T: DeserializeOwned>(
        &self,
        kind: RequestKind,
        path: &str,
        ops: &[Op],
    ) -> Result<T> {
        self.call(kind, Method::POST, path, Some(&Ops { ops }))
            .await
    }

    async fn send<T: DeserializeOwned>(
        &self,
        kind: RequestKind,
        method: Method,
        url: &str,
        body: Option<&(impl Serialize + ?Sized)>,
    ) -> Result<T> {
        let started = std::time::Instant::now();
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
        if !status.is_success() {
            return Err(Error::Api(ApiError::from_response(status, &bytes)));
        }
        let body: &[u8] = if bytes.is_empty() { b"null" } else { &bytes };
        serde_json::from_slice(body).map_err(|e| Error::BadResponse(format!("{kind}: {e}")))
    }
}

fn one_per_op(ops: &[Op], results: Vec<OpResult>) -> Result<Vec<OpResult>> {
    if results.len() == ops.len() {
        Ok(results)
    } else {
        Err(Error::BadResponse(format!(
            "{} results for {} ops",
            results.len(),
            ops.len()
        )))
    }
}

fn exists_ok(r: Result<IgnoredAny>) -> Result<()> {
    match r {
        Err(Error::Api(e)) if e.code == "already_exists" => Ok(()),
        r => r.map(drop),
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
    pub async fn ops(&self, ops: &[Op]) -> Result<Vec<OpResult>> {
        let path = format!("/transactions/{}/ops", self.id);
        let r: Results = self.db.run(RequestKind::Ops, &path, ops).await?;
        one_per_op(ops, r.results.unwrap_or_default())
    }

    /// Run `ops` and commit. A commit whose outcome is unknown is resolved
    /// before returning, so `Ok` means applied and a retryable error
    /// means not applied.
    pub async fn commit(self, ops: &[Op]) -> Result<()> {
        let path = format!("/transactions/{}/commit", self.id);
        match self
            .db
            .run::<IgnoredAny>(RequestKind::Commit, &path, ops)
            .await
        {
            Ok(_) => Ok(()),
            Err(Error::Api(e)) if e.from_server && !e.outcome_unknown => Err(Error::Api(e)),
            Err(Error::Http(e)) if e.is_connect() => Err(Error::Http(e)),
            // A read-only commit applies nothing, so a lost answer is simply retried.
            Err(Error::Http(_)) if ops.is_empty() => {
                Err(ApiError::not_applied("read-only commit lost its answer"))
            }
            // A lost response, a proxy's error or an unknown outcome: wait out any copy of
            // the commit still in flight, then ask whether it applied.
            Err(_) => {
                tokio::time::sleep(AMBIGUOUS_COMMIT_WAIT).await;
                let path = format!("/commits/{}", self.commit_id);
                let s: CommitStatus = retry(|| {
                    self.db
                        .call(RequestKind::Status, Method::GET, &path, None::<&()>)
                })
                .await?;
                match s.status {
                    CommitState::Committed => Ok(()),
                    CommitState::NotCommitted => Err(ApiError::not_applied("commit did not apply")),
                }
            }
        }
    }

    /// Abort the transaction. Failures are ignored: an abandoned
    /// transaction expires on its own.
    pub async fn abort(self) {
        let path = format!("/transactions/{}", self.id);
        let _ = self
            .db
            .call::<IgnoredAny>(RequestKind::Abort, Method::DELETE, &path, None::<&()>)
            .await;
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_baserunna_errors_say_whether_a_request_applied() {
        let body = br#"{"error":{"code":"conflict","message":"m","retryable":true}}"#;
        let server = ApiError::from_response(StatusCode::CONFLICT, body);
        assert!(server.from_server && server.retryable && !server.outcome_unknown);

        let unknown = br#"{"error":{"code":"unavailable","message":"m","retryable":true,"details":{"outcome":"unknown"}}}"#;
        assert!(ApiError::from_response(StatusCode::SERVICE_UNAVAILABLE, unknown).outcome_unknown);

        let proxy =
            ApiError::from_response(StatusCode::BAD_GATEWAY, b"<html>502 Bad Gateway</html>");
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
