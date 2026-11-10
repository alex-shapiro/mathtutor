#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss,
    clippy::too_many_lines
)]
//! Load test runner simulating concurrent learners against a Baserunna server.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use mathtutor::db::client::api::{self, Endpoint as _, Histogram, Metrics};
use mathtutor::db::client::bytes::Bytes;
use mathtutor::db::client::{BoxFuture, Route, Transport, TransportError, http};
use mathtutor::db::{Db, DbConfig};
use mathtutor::event_log::{self, EventKind};
use mathtutor::graph::{Graph, Quiz};
use mathtutor::overlay;
use mathtutor::scheduler::Action;
use mathtutor::types::{Difficulty, QuizType, Rating, Strategy};
use mathtutor::view::Want;
use mathtutor::{answer, cards, path, scheduler, state, store, syllabus};

/// Baserunna request kind, derived from its endpoint.
type RequestKind = &'static str;

fn request_kind(method: &http::Method, route: &str) -> RequestKind {
    match route {
        r if r == api::Txn::ROUTE => "txn",
        r if r == api::BeginTxn::ROUTE => "begin",
        r if r == api::TxnOps::ROUTE => "ops",
        r if r == api::CommitTxn::ROUTE => "commit",
        r if r == api::AbortTxn::ROUTE && method == http::Method::DELETE => "abort",
        r if r == api::GetCommit::ROUTE => "status",
        r if r == api::CreateDb::ROUTE || r == api::CreateTable::ROUTE => "create",
        _ => "other",
    }
}

/// Measured request to the server.
#[derive(Debug, Clone)]
struct Sample {
    kind: RequestKind,
    status: u16,
    elapsed: Duration,
    server: Option<ServerTiming>,
    sent_bytes: usize,
    received_bytes: usize,
}

/// Durations from a `Server-Timing` response header.
#[derive(Debug, Clone, Copy, Default)]
struct ServerTiming {
    total: Duration,
    store: Duration,
    store_calls: u32,
    commit: Duration,
    wait: Duration,
}

impl ServerTiming {
    fn parse(v: &str) -> Self {
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

/// Transport that reports each response to an observer.
struct Observed<T> {
    inner: T,
    observe: Arc<dyn Fn(Sample) + Send + Sync>,
}

impl<T: Transport> Transport for Observed<T> {
    fn send(
        &self,
        req: http::Request<Bytes>,
    ) -> BoxFuture<'_, Result<http::Response<Bytes>, TransportError>> {
        Box::pin(async move {
            let route = req.extensions().get::<Route>().map_or("", |r| r.0);
            let kind = request_kind(req.method(), route);
            let sent_bytes = req.body().len();
            let started = Instant::now();
            let resp = self.inner.send(req).await?;
            let server = resp
                .headers()
                .get("server-timing")
                .and_then(|v| v.to_str().ok())
                .map(ServerTiming::parse);
            (self.observe)(Sample {
                kind,
                status: resp.status().as_u16(),
                elapsed: started.elapsed(),
                server,
                sent_bytes,
                received_bytes: resp.body().len(),
            });
            Ok(resp)
        })
    }
}

fn env_or<T: std::str::FromStr>(k: &str, default: T) -> T {
    std::env::var(k)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

#[derive(Default)]
struct Series {
    samples: Vec<Duration>,
    errors: u64,
    last_error: Option<String>,
}

impl Series {
    fn row(&mut self, name: &str, secs: f64) -> String {
        self.samples.sort();
        let q = |p: f64| -> f64 {
            if self.samples.is_empty() {
                return 0.0;
            }
            let i =
                ((self.samples.len() as f64 * p).ceil() as usize).clamp(1, self.samples.len()) - 1;
            self.samples[i].as_secs_f64() * 1000.0
        };
        format!(
            "{name:<16} {:>7} {:>8.1} {:>8.1} {:>8.1} {:>8.1} {:>8.1} {:>6}",
            self.samples.len(),
            self.samples.len() as f64 / secs,
            q(0.5),
            q(0.95),
            q(0.99),
            q(1.0),
            self.errors,
        )
    }
}

#[derive(Default)]
struct Requests {
    elapsed: Vec<Duration>,
    server: Vec<Duration>,
    store: Vec<Duration>,
    commit: Vec<Duration>,
    network: Vec<Duration>,
    sent: usize,
    received: usize,
    statuses: BTreeMap<u16, u64>,
}

fn percentile_ms(v: &mut [Duration], p: f64) -> f64 {
    if v.is_empty() {
        return 0.0;
    }
    v.sort();
    let i = ((v.len() as f64 * p).ceil() as usize).clamp(1, v.len()) - 1;
    v[i].as_secs_f64() * 1000.0
}

#[derive(Default)]
struct Stats {
    ops: BTreeMap<&'static str, Series>,
    requests: BTreeMap<RequestKind, Requests>,
}

impl Stats {
    fn record(&mut self, op: &'static str, took: Duration, r: Result<(), String>) {
        let s = self.ops.entry(op).or_default();
        match r {
            Ok(()) => s.samples.push(took),
            Err(e) => {
                s.errors += 1;
                s.last_error = Some(e);
            }
        }
    }

    fn observe(&mut self, x: &Sample) {
        let r = self.requests.entry(x.kind).or_default();
        r.elapsed.push(x.elapsed);
        if let Some(t) = x.server {
            r.server.push(t.total);
            r.store.push(t.store);
            r.commit.push(t.commit);
            r.network.push(x.elapsed.saturating_sub(t.total));
        }
        r.sent += x.sent_bytes;
        r.received += x.received_bytes;
        *r.statuses.entry(x.status).or_default() += 1;
    }

    fn print(&mut self, learners: usize, secs: f64) {
        println!("\n== {learners} learners, {secs:.0} s ==");
        println!(
            "{:<16} {:>7} {:>8} {:>8} {:>8} {:>8} {:>8} {:>6}",
            "command", "count", "per s", "p50 ms", "p95", "p99", "max", "errors"
        );
        for (name, s) in &mut self.ops {
            println!("{}", s.row(name, secs));
            if let Some(e) = &s.last_error {
                println!("    last error: {e}");
            }
        }
        println!(
            "\n{:<8} {:>7} {:>9} {:>9} {:>9} {:>9} {:>9} {:>9} {:>9} {:>8} {:>8}  statuses",
            "request",
            "count",
            "p50 ms",
            "p99",
            "srv p50",
            "srv p99",
            "store p50",
            "commit p50",
            "net p50",
            "KB out",
            "KB in"
        );
        for (kind, r) in &mut self.requests {
            println!(
                "{kind:<8} {:>7} {:>9.1} {:>9.1} {:>9.1} {:>9.1} {:>9.1} {:>9.1} {:>9.1} {:>8.0} {:>8.0}  {:?}",
                r.elapsed.len(),
                percentile_ms(&mut r.elapsed, 0.5),
                percentile_ms(&mut r.elapsed, 0.99),
                percentile_ms(&mut r.server, 0.5),
                percentile_ms(&mut r.server, 0.99),
                percentile_ms(&mut r.store, 0.5),
                percentile_ms(&mut r.commit, 0.5),
                percentile_ms(&mut r.network, 0.5),
                r.sent as f64 / 1024.0,
                r.received as f64 / 1024.0,
                r.statuses,
            );
        }
    }
}

fn print_server(m: &Metrics, before: Option<&Metrics>, secs: f64) {
    let delta = |f: fn(&api::Cpu) -> Option<u64>| {
        Some((f(&m.cpu)? as f64 - f(&before?.cpu)? as f64) / (secs * 10.0))
    };
    if let (Some(cpu), Some(steal)) = (delta(|c| c.process_ms), delta(|c| c.steal_ms)) {
        println!("\nserver cpu: process {cpu:.0}% of a core, host steal {steal:.0}%");
    }
    println!("\nserver {}", m.node);
    println!(
        "{:<34} {:>7} {:>8} {:>8} {:>8} {:>8} {:>8} {:>6}",
        "histogram", "count", "mean", "p50", "p90", "p99", "max", "errors"
    );
    let ms = |v: Option<f64>| v.map_or(String::new(), |v| v.to_string());
    let row = |name: &str, h: &Histogram| {
        println!(
            "{name:<34} {:>7} {:>8} {:>8} {:>8} {:>8} {:>8} {:>6}",
            h.count,
            ms(h.mean_ms),
            ms(h.p50_ms),
            ms(h.p90_ms),
            ms(h.p99_ms),
            ms(h.max_ms),
            h.errors.map_or(String::new(), |e| e.to_string()),
        );
    };
    for (k, h) in &m.store {
        row(&format!("store {k}"), h);
    }
    row("commit batches", &m.commits.latency);
    row("scheduler lag", &m.lag);
    println!(
        "    txns per batch: mean {} max {}",
        m.commits.txns_per_batch, m.commits.max_txns_per_batch
    );
    for (k, h) in &m.routes {
        if h.count >= 10 {
            row(k, h);
        }
    }
}

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    fn below(&mut self, n: u64) -> u64 {
        self.next() % n.max(1)
    }
}

fn sample_text(rng: &mut Rng, bytes: usize) -> String {
    const WORDS: [&str; 12] = [
        "vector",
        "space",
        "linear",
        "map",
        "basis",
        "kernel",
        "image",
        "rank",
        "x^2",
        "proof",
        "therefore",
        "lemma",
    ];
    let mut s = String::with_capacity(bytes + 16);
    while s.len() < bytes {
        s.push_str(WORDS[rng.below(WORDS.len() as u64) as usize]);
        s.push(' ');
    }
    s
}

type Ledger = Arc<Mutex<BTreeMap<(String, String), (u64, u64)>>>;

struct Learner {
    db: Db,
    db_name: String,
    ledger: Ledger,
    path: Option<String>,
    shared: Option<Arc<Mutex<Option<String>>>>,
    rng: Rng,
    targets: Arc<Vec<String>>,
    lesson_bytes: usize,
}

impl Learner {
    async fn new_path(&mut self) -> Result<(), String> {
        let n = self.targets.len() as u64;
        let target = self.targets[self.rng.below(n) as usize].clone();
        let strategy = if self.rng.below(4) == 0 {
            Strategy::TopDown
        } else {
            Strategy::BottomUp
        };
        let id = path::cmd_path_new(&self.db, "load test", &[target], strategy, None)
            .await
            .map_err(|e| e.to_string())?;
        if let Some(shared) = &self.shared {
            shared.lock().unwrap().get_or_insert(id);
        } else {
            self.path = Some(id);
        }
        Ok(())
    }

    async fn turn(&mut self, out: &mut Vec<(&'static str, Duration, Result<(), String>)>) {
        if let Some(shared) = &self.shared {
            self.path.clone_from(&shared.lock().unwrap());
        }
        let Some(p) = self.path.clone() else {
            let t = Instant::now();
            let r = self.new_path().await;
            out.push(("path new", t.elapsed(), r));
            return;
        };
        let roll = self.rng.below(100);
        let t = Instant::now();
        if roll < 4 {
            let r = state::compute_state(&self.db, Some(&p), None)
                .await
                .map(drop);
            out.push(("path state", t.elapsed(), r.map_err(|e| e.to_string())));
            return;
        }
        if roll < 6 {
            let r = syllabus::compute_syllabus(&self.db, Some(&p), 10, None)
                .await
                .map(drop);
            out.push(("path syllabus", t.elapsed(), r.map_err(|e| e.to_string())));
            return;
        }
        if roll < 7 {
            let r = path::list_summaries(&self.db, None).await.map(drop);
            out.push(("path list", t.elapsed(), r.map_err(|e| e.to_string())));
            return;
        }
        let envelope =
            match scheduler::compute_next(&self.db, Some(&p), None, scheduler::NextMode::Default)
                .await
            {
                Ok(e) => e,
                Err(e) => {
                    out.push(("path next", t.elapsed(), Err(e.to_string())));
                    return;
                }
            };
        out.push(("path next", t.elapsed(), Ok(())));
        let t = Instant::now();
        match envelope.action() {
            Action::CreateLesson { atom_id } => {
                let body = sample_text(&mut self.rng, self.lesson_bytes);
                let r = store::cmd_lesson_upsert(&self.db, atom_id, body, Some(&p), None).await;
                out.push(("lesson upsert", t.elapsed(), r.map_err(|e| e.to_string())));
            }
            Action::CreateQuiz {
                atom_id,
                difficulty,
            } => {
                let r = store::cmd_quiz_create(
                    &self.db,
                    atom_id,
                    *difficulty,
                    sample_text(&mut self.rng, 250),
                    sample_text(&mut self.rng, 300),
                    Some(sample_text(&mut self.rng, 200)),
                    QuizType::FreeText,
                    Some(&p),
                    None,
                )
                .await
                .map(drop);
                out.push(("quiz create", t.elapsed(), r.map_err(|e| e.to_string())));
            }
            Action::PresentQuiz { quiz_id, .. } => {
                let rating = match self.rng.below(100) {
                    0..15 => Rating::Again,
                    15..30 => Rating::Hard,
                    30..80 => Rating::Good,
                    _ => Rating::Easy,
                };
                let answer = Some(sample_text(&mut self.rng, 200));
                let r = answer::cmd_quiz_answer(&self.db, quiz_id, rating, answer, Some(&p), None)
                    .await;
                let mut ledger = self.ledger.lock().unwrap();
                let entry = ledger.entry((self.db_name.clone(), p.clone())).or_default();
                if r.is_ok() {
                    entry.0 += 1;
                } else {
                    entry.1 += 1;
                }
                drop(ledger);
                out.push(("quiz answer", t.elapsed(), r.map_err(|e| e.to_string())));
            }
            Action::Done => {
                if let Some(shared) = &self.shared {
                    let mut current = shared.lock().unwrap();
                    if current.as_deref() == Some(p.as_str()) {
                        *current = None;
                    }
                }
                self.path = None;
            }
            Action::PresentLesson { .. } => {}
        }
    }
}

fn targets() -> Vec<String> {
    let g = Graph::load_embedded().expect("graph");
    let mut out: Vec<String> = g
        .by_id
        .values()
        .filter(|c| c.id.split('.').count() == 3 && !c.children_ids.is_empty())
        .map(|c| c.id.clone())
        .collect();
    out.sort();
    out
}

async fn seed(db: &Db, atoms: usize, lesson_bytes: usize) {
    const BATCH: usize = 100;
    let have = db
        .snapshot(&Want::overlay())
        .await
        .expect("snapshot")
        .overlay
        .atoms
        .len();
    if have >= atoms {
        return;
    }
    let g = Graph::load_embedded().expect("graph");
    let mut ids: Vec<&String> = g
        .by_id
        .iter()
        .filter(|(_, c)| c.children_ids.is_empty())
        .map(|(id, _)| id)
        .collect();
    ids.sort();
    let mut rng = Rng(7);
    let started = Instant::now();
    for chunk in ids[..atoms.min(ids.len())].chunks(BATCH) {
        let lessons: Vec<(String, String)> = chunk
            .iter()
            .map(|id| ((*id).clone(), sample_text(&mut rng, lesson_bytes)))
            .collect();
        db.transact(&Want::overlay(), |_, changes| {
            for (id, body) in &lessons {
                overlay::upsert_lesson(changes, id, body);
                for (i, d) in [Difficulty::Easy, Difficulty::Medium, Difficulty::Hard]
                    .into_iter()
                    .enumerate()
                {
                    let q = Quiz {
                        id: format!("{id}.q{}", i + 1),
                        difficulty: d,
                        kind: None,
                        question: "q ".repeat(120),
                        answer: "a ".repeat(150),
                        rubric: Some("r ".repeat(100)),
                    };
                    overlay::add_quiz(changes, id, &q);
                }
            }
            Ok(())
        })
        .await
        .expect("seed");
    }
    println!(
        "seeded {atoms} atoms in {:.1} s",
        started.elapsed().as_secs_f64()
    );
}

async fn verify(base: &DbConfig, ledger: &Ledger) {
    let entries: Vec<_> = ledger.lock().unwrap().clone().into_iter().collect();
    let mut bad = 0;
    for ((db_name, path_id), (ok, failed)) in &entries {
        let cfg = DbConfig {
            db: db_name.clone(),
            cache_dir: None,
            ..base.clone()
        };
        let db = Db::connect(&cfg).expect("connect");
        let events = event_log::load(&db, path_id).await.expect("load events");
        let logged = events
            .iter()
            .filter(|e| e.kind == EventKind::QuizAnswered)
            .count() as u64;
        if logged < *ok || logged > ok + failed {
            bad += 1;
            println!(
                "VERIFY {db_name}/{path_id}: {logged} answers logged, {ok} acknowledged, {failed} failed"
            );
        }
        let (replayed, _) = cards::fold_history(path_id, &events).expect("fold");
        let view = db
            .snapshot(&Want::path(Some(path_id)))
            .await
            .expect("snapshot");
        let stored = &view.path().expect("path").cards;
        for c in &replayed {
            let s = stored
                .get(&c.quiz_id)
                .map(|s| (s.reps, s.lapses, s.presented));
            if s != Some((c.reps, c.lapses, c.presented)) {
                bad += 1;
                println!(
                    "VERIFY {db_name}/{path_id}: card {} differs from its replay",
                    c.quiz_id
                );
            }
        }
    }
    let verdict = if bad == 0 {
        "ok".to_string()
    } else {
        format!("{bad} problems")
    };
    println!("verified {} paths: {verdict}", entries.len());
}

#[tokio::main]
async fn main() {
    let base = DbConfig::from_env().expect("BASERUNNA_TOKEN");
    let prefix: String = env_or("LT_PREFIX", "lt-".to_string());
    let steps: Vec<usize> = env_or("LT_STEPS", "1,4,16".to_string())
        .split(',')
        .map(|s| s.trim().parse().expect("LT_STEPS"))
        .collect();
    let secs: u64 = env_or("LT_SECS", 20);
    let think = Duration::from_millis(env_or("LT_THINK_MS", 0));
    let lesson_bytes: usize = env_or("LT_LESSON", 1500);
    let seed_atoms: usize = env_or("LT_SEED", 0);
    let targets = Arc::new(targets());
    let stats = Arc::new(Mutex::new(Stats::default()));

    let observer: Arc<dyn Fn(Sample) + Send + Sync> = {
        let stats = stats.clone();
        Arc::new(move |x: Sample| stats.lock().unwrap().observe(&x))
    };

    let share: String = env_or("LT_SHARE", String::new());
    let shared_path = (share == "path").then(|| Arc::new(Mutex::new(None)));
    let max = steps.iter().copied().max().unwrap_or(1);
    println!(
        "opening {max} learners on {} (sharing: {})",
        base.url,
        if share.is_empty() { "nothing" } else { &share }
    );
    let ledger: Ledger = Arc::default();
    let mut learners = Vec::new();
    for i in 0..max {
        let db = if share.is_empty() {
            format!("{prefix}{i}")
        } else {
            format!("{prefix}shared")
        };
        let cfg = DbConfig {
            db: db.clone(),
            cache_dir: None,
            ..base.clone()
        };
        let db_name = db;
        Db::open(&cfg).await.expect("open");
        let transport = Observed {
            inner: cfg.transport().expect("transport"),
            observe: observer.clone(),
        };
        let db = Db::with_transport(&cfg, transport);
        if seed_atoms > 0 {
            seed(&db, seed_atoms, lesson_bytes).await;
        }
        learners.push(Arc::new(tokio::sync::Mutex::new(Learner {
            db,
            db_name,
            ledger: ledger.clone(),
            path: None,
            shared: shared_path.clone(),
            rng: Rng(0x9E37_79B9_7F4A_7C15 ^ ((i as u64 + 1) * 0x2545_F491_4F6C_DD1D)),
            targets: targets.clone(),
            lesson_bytes,
        })));
    }

    let metrics: bool = env_or("LT_METRICS", 1u8) == 1;
    let admin = Db::connect(&base).expect("connect").client().clone();
    let server_metrics = |reset: bool| {
        let admin = admin.clone();
        async move {
            let m = if reset {
                admin.reset_metrics().await
            } else {
                admin.metrics().await
            };
            m.inspect_err(|e| println!("server metrics: {e}")).ok()
        }
    };
    for &n in &steps {
        let mut before = None;
        *stats.lock().unwrap() = Stats::default();
        if metrics {
            before = server_metrics(true).await;
        }
        let stop = Arc::new(AtomicBool::new(false));
        let turns = Arc::new(AtomicU64::new(0));
        let started = Instant::now();
        let tasks: Vec<_> = learners[..n]
            .iter()
            .map(|l| {
                let (l, stop, stats, turns) =
                    (l.clone(), stop.clone(), stats.clone(), turns.clone());
                tokio::spawn(async move {
                    let mut l = l.lock().await;
                    let mut out = Vec::new();
                    while !stop.load(Ordering::Relaxed) {
                        l.turn(&mut out).await;
                        turns.fetch_add(1, Ordering::Relaxed);
                        {
                            let mut s = stats.lock().unwrap();
                            for (op, took, r) in out.drain(..) {
                                s.record(op, took, r);
                            }
                        }
                        if !think.is_zero() {
                            tokio::time::sleep(think).await;
                        }
                    }
                })
            })
            .collect();
        tokio::time::sleep(Duration::from_secs(secs)).await;
        stop.store(true, Ordering::Relaxed);
        for t in tasks {
            let _ = t.await;
        }
        let elapsed = started.elapsed().as_secs_f64();
        stats.lock().unwrap().print(n, elapsed);
        if metrics && let Some(m) = server_metrics(false).await {
            print_server(&m, before.as_ref(), elapsed);
        }
    }
    verify(&base, &ledger).await;
}

#[cfg(test)]
mod tests {
    use super::*;

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

    #[test]
    fn request_kind_matches_endpoint_route() {
        let post = http::Method::POST;
        assert_eq!(request_kind(&post, api::CommitTxn::ROUTE), "commit");
        assert_eq!(
            request_kind(&http::Method::DELETE, api::AbortTxn::ROUTE),
            "abort"
        );
        assert_eq!(request_kind(&post, api::Txn::ROUTE), "txn");
        assert_eq!(
            request_kind(&http::Method::GET, api::GetCommit::ROUTE),
            "status"
        );
    }
}
