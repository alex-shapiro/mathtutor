#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss,
    clippy::too_many_lines
)]
//! Load test a Baserunna server with simulated learners.
//!
//! Each learner owns a database (`<LT_PREFIX><n>`) and works through
//! learning paths the way the tutor agent does: it asks for the next action,
//! authors the lessons and quizzes it is asked for, answers quizzes, and now
//! and then reads its state, tree or syllabus. Learners never pause between
//! actions. Each concurrency step reports latency per command and per HTTP
//! request, splitting request latency into the server's own time (from its
//! `Server-Timing` header) and the network.
//!
//! ```text
//! BASERUNNA_TOKEN=<api key> cargo run --release --example loadtest
//! LT_STEPS=1,8,32 LT_SECS=30 cargo run --release --example loadtest
//! ```
//!
//! | variable      | default                     |                                              |
//! | ------------- | --------------------------- | -------------------------------------------- |
//! | `BASERUNNA_URL`   | `https://baserunna.fly.dev` | server                                   |
//! | `BASERUNNA_TOKEN` | required                    | API key                                  |
//! | `LT_PREFIX`   | `lt-`                       | learner database name prefix                 |
//! | `LT_STEPS`    | `1,4,16`                    | concurrent learners in each step             |
//! | `LT_SECS`     | 20                          | duration of each step                        |
//! | `LT_LESSON`   | 1500                        | bytes per authored lesson                    |
//! | `LT_THINK_MS` | 0                           | pause between a learner's actions            |
//! | `LT_SEED`     | 0                           | atoms each learner database starts with authored (a lesson and 3 quizzes each) |
//! | `LT_METRICS`  | 1                           | print the server's histograms (needs an admin key) |
//! | `LT_SHARE`    | none                        | `db`: one database, a path per learner; `path`: one path for all |

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use mathtutor::db::{Db, DbConfig, Sample};
use mathtutor::event_log::{self, EventKind};
use mathtutor::graph::{Graph, Quiz};
use mathtutor::overlay;
use mathtutor::types::{Difficulty, QuizType, Rating, Strategy};
use mathtutor::view::Want;
use mathtutor::{answer, cards, path, scheduler, state, store, syllabus};
use serde_json::Value;

fn env<T: std::str::FromStr>(k: &str, default: T) -> T {
    std::env::var(k)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

/// Latencies of one kind of operation in one step.
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

/// Request-level samples of one kind in one step.
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

fn pct(v: &mut [Duration], p: f64) -> f64 {
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
    requests: BTreeMap<&'static str, Requests>,
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
                pct(&mut r.elapsed, 0.5),
                pct(&mut r.elapsed, 0.99),
                pct(&mut r.server, 0.5),
                pct(&mut r.server, 0.99),
                pct(&mut r.store, 0.5),
                pct(&mut r.commit, 0.5),
                pct(&mut r.network, 0.5),
                r.sent as f64 / 1024.0,
                r.received as f64 / 1024.0,
                r.statuses,
            );
        }
    }
}

fn print_server(m: &Value, before: Option<&Value>, secs: f64) {
    let delta = |k: &str| {
        let now = m["cpu"][k].as_f64()?;
        Some((now - before?["cpu"][k].as_f64()?) / (secs * 10.0))
    };
    if let (Some(cpu), Some(steal)) = (delta("process_ms"), delta("steal_ms")) {
        println!("\nserver cpu: process {cpu:.0}% of a core, host steal {steal:.0}%");
    }
    println!("\nserver {}", m["node"].as_str().unwrap_or("?"));
    println!(
        "{:<34} {:>7} {:>8} {:>8} {:>8} {:>8} {:>8} {:>6}",
        "histogram", "count", "mean", "p50", "p90", "p99", "max", "errors"
    );
    let row = |name: &str, h: &Value| {
        println!(
            "{name:<34} {:>7} {:>8} {:>8} {:>8} {:>8} {:>8} {:>6}",
            h["count"],
            h["mean_ms"],
            h["p50_ms"],
            h["p90_ms"],
            h["p99_ms"],
            h["max_ms"],
            h.get("errors").map_or(String::new(), Value::to_string),
        );
    };
    for (k, h) in m["store"].as_object().into_iter().flatten() {
        row(&format!("store {k}"), h);
    }
    row("commit batches", &m["commits"]);
    row("scheduler lag", &m["lag"]);
    println!(
        "    txns per batch: mean {} max {}",
        m["commits"]["txns_per_batch"], m["commits"]["max_txns_per_batch"]
    );
    for (k, h) in m["routes"].as_object().into_iter().flatten() {
        if h["count"].as_u64().unwrap_or(0) >= 10 {
            row(k, h);
        }
    }
}

/// Deterministic per-learner randomness.
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

fn text(rng: &mut Rng, bytes: usize) -> String {
    const WORDS: [&str; 12] = [
        "vector",
        "space",
        "linear",
        "map",
        "basis",
        "kernel",
        "image",
        "rank",
        "$x^2$",
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

/// Acknowledged and failed answers per `(database, path)`, to check at the
/// end that each acknowledged answer was logged exactly once.
type Ledger = Arc<Mutex<BTreeMap<(String, String), (u64, u64)>>>;

struct Learner {
    db: Db,
    db_name: String,
    ledger: Ledger,
    path: Option<String>,
    /// The path every learner works on, under `LT_SHARE=path`.
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

    /// One agent turn. Returns the commands it ran and how long each took.
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
                Ok(e) => serde_json::to_value(&e).expect("envelope serializes"),
                Err(e) => {
                    out.push(("path next", t.elapsed(), Err(e.to_string())));
                    return;
                }
            };
        out.push(("path next", t.elapsed(), Ok(())));
        let payload = &envelope["payload"];
        let s = |v: &Value| v.as_str().unwrap_or_default().to_string();
        let t = Instant::now();
        match envelope["action"].as_str() {
            Some("create_lesson") => {
                let body = text(&mut self.rng, self.lesson_bytes);
                let r = store::cmd_lesson_upsert(
                    &self.db,
                    &s(&payload["atom"]["id"]),
                    body,
                    Some(&p),
                    None,
                )
                .await;
                out.push(("lesson upsert", t.elapsed(), r.map_err(|e| e.to_string())));
            }
            Some("create_quiz") => {
                let difficulty: Difficulty = s(&payload["target_difficulty"])
                    .parse()
                    .expect("difficulty");
                let r = store::cmd_quiz_create(
                    &self.db,
                    &s(&payload["atom"]["id"]),
                    difficulty,
                    text(&mut self.rng, 250),
                    text(&mut self.rng, 300),
                    Some(text(&mut self.rng, 200)),
                    QuizType::FreeText,
                    Some(&p),
                    None,
                )
                .await
                .map(drop);
                out.push(("quiz create", t.elapsed(), r.map_err(|e| e.to_string())));
            }
            Some("present_quiz") => {
                let rating = match self.rng.below(100) {
                    0..15 => Rating::Again,
                    15..30 => Rating::Hard,
                    30..80 => Rating::Good,
                    _ => Rating::Easy,
                };
                let answer = Some(text(&mut self.rng, 200));
                let r = answer::cmd_quiz_answer(
                    &self.db,
                    &s(&payload["quiz"]["id"]),
                    rating,
                    answer,
                    Some(&p),
                    None,
                )
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
            Some("done") => {
                if let Some(shared) = &self.shared {
                    let mut current = shared.lock().unwrap();
                    if current.as_deref() == Some(p.as_str()) {
                        *current = None;
                    }
                }
                self.path = None;
            }
            _ => {}
        }
    }
}

/// Atoms and clusters that make reasonable path targets: second-level
/// clusters, which expand to a handful of atoms each.
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

/// Author a lesson and three quizzes for the first `atoms` atoms of the
/// curriculum, unless the database already holds them, in transactions of
/// `batch` atoms.
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
            .map(|id| ((*id).clone(), text(&mut rng, lesson_bytes)))
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

/// Check that every path's log holds each acknowledged answer once, and
/// that its stored cards equal a replay of the log.
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
    let prefix: String = env("LT_PREFIX", "lt-".to_string());
    let steps: Vec<usize> = env("LT_STEPS", "1,4,16".to_string())
        .split(',')
        .map(|s| s.trim().parse().expect("LT_STEPS"))
        .collect();
    let secs: u64 = env("LT_SECS", 20);
    let think = Duration::from_millis(env("LT_THINK_MS", 0));
    let lesson_bytes: usize = env("LT_LESSON", 1500);
    let seed_atoms: usize = env("LT_SEED", 0);
    let targets = Arc::new(targets());
    let stats = Arc::new(Mutex::new(Stats::default()));

    let observer = {
        let stats = stats.clone();
        Arc::new(move |x: Sample| stats.lock().unwrap().observe(&x))
    };

    let share: String = env("LT_SHARE", String::new());
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
        let db = Db::open(&cfg)
            .await
            .expect("open")
            .observed(observer.clone());
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

    let metrics: bool = env("LT_METRICS", 1u8) == 1;
    let http = reqwest::Client::new();
    let metrics_url = format!("{}/v1/admin/metrics", base.url.trim_end_matches('/'));
    let server_metrics = |reset: bool| {
        let (http, url, token) = (http.clone(), metrics_url.clone(), base.token.clone());
        async move {
            let r = http
                .get(format!("{url}?reset={reset}"))
                .bearer_auth(token)
                .send()
                .await
                .ok()?;
            r.json::<Value>().await.ok()
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
