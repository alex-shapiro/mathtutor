//! `mt import-turso`: one-time import of the libSQL (Turso) database used
//! before Baserunna.
//!
//! Reads a database at the last libSQL migration (4) over Turso's HTTP API
//! and writes its paths, authored lessons and quizzes, and each path's
//! events, with their original timestamps, into the user database. Cards
//! and lessons taught are then rebuilt from the events and checked against
//! the libSQL `cards` rows. Paths already imported are skipped, so an
//! interrupted import can be rerun.

use std::collections::{BTreeMap, HashMap};

use chrono::{DateTime, Utc};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value as Json};

use crate::answer::atom_from_quiz_id;
use crate::cards::{self, CardState};
use crate::db::{Db, Op, PAGE, ts};
use crate::event_log::{Event, EventKind, EventPayload};
use crate::graph::Quiz;
use crate::overlay::Edit;
use crate::path::PathFile;
use crate::schema::{activity_key, events_stream};
use crate::types::{Difficulty, QuizType, Rating, Strategy};
use crate::view::Want;
use crate::{Error, Result};

/// The last migration the libSQL schema had.
const SCHEMA_VERSION: i64 = 4;

/// Relative difference within which FSRS parameters count as equal: the
/// libSQL rows hold them as doubles widened from `f32`.
const FSRS_TOLERANCE: f64 = 1e-4;

// ── Turso HTTP API ──────────────────────────────────────────────────

/// A libSQL server reached over its HTTP API (Hrana over HTTP, v2).
pub struct Turso {
    http: reqwest::Client,
    pipeline_url: String,
    token: Option<String>,
}

impl Turso {
    /// `url` is the database URL; `libsql://` is read as `https://`.
    pub fn new(url: &str, token: Option<String>) -> Result<Self> {
        let base = match url.strip_prefix("libsql://") {
            Some(host) => format!("https://{host}"),
            None => url.to_string(),
        };
        Ok(Self {
            http: reqwest::Client::builder()
                .timeout(std::time::Duration::from_mins(5))
                .build()?,
            pipeline_url: format!("{}/v2/pipeline", base.trim_end_matches('/')),
            token,
        })
    }

    /// Run `queries` in one read transaction and return each one's rows as
    /// objects keyed by column name.
    async fn read(&self, queries: &[&str]) -> Result<Vec<Vec<Map<String, Json>>>> {
        let stmts = std::iter::once("BEGIN")
            .chain(queries.iter().copied())
            .chain(std::iter::once("ROLLBACK"));
        let mut requests: Vec<PipelineRequest> = stmts
            .map(|sql| PipelineRequest::Execute { stmt: Stmt { sql } })
            .collect();
        requests.push(PipelineRequest::Close);
        let mut req = self
            .http
            .post(&self.pipeline_url)
            .json(&Pipeline { requests });
        if let Some(t) = &self.token {
            req = req.bearer_auth(t);
        }
        let resp = req.send().await?;
        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            return Err(Error::Turso(format!("{status}: {body}")));
        }
        let r: PipelineResponse = resp.json().await?;
        let mut sets = Vec::with_capacity(queries.len());
        for (sql, result) in queries.iter().zip(r.results.into_iter().skip(1)) {
            match result {
                StreamResult::Ok {
                    response: Response::Execute { result },
                } => sets.push(result.into_objects()),
                StreamResult::Ok { .. } => {
                    return Err(Error::Turso(format!("{sql}: unexpected response")));
                }
                StreamResult::Error { error } => {
                    return Err(Error::Turso(format!("{sql}: {}", error.message)));
                }
            }
        }
        if sets.len() != queries.len() {
            return Err(Error::Turso("missing statement results".into()));
        }
        Ok(sets)
    }
}

#[derive(Serialize)]
struct Pipeline<'a> {
    requests: Vec<PipelineRequest<'a>>,
}

#[derive(Serialize)]
#[serde(tag = "type", rename_all = "lowercase")]
enum PipelineRequest<'a> {
    Execute { stmt: Stmt<'a> },
    Close,
}

#[derive(Serialize)]
struct Stmt<'a> {
    sql: &'a str,
}

#[derive(Deserialize)]
struct PipelineResponse {
    results: Vec<StreamResult>,
}

#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
enum StreamResult {
    Ok { response: Response },
    Error { error: HranaError },
}

#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
enum Response {
    Execute { result: StmtResult },
    Close,
}

#[derive(Deserialize)]
struct HranaError {
    message: String,
}

#[derive(Deserialize)]
struct StmtResult {
    cols: Vec<Col>,
    rows: Vec<Vec<Value>>,
}

#[derive(Deserialize)]
struct Col {
    name: Option<String>,
}

/// A `SQLite` value; integers arrive as strings to keep 64 bits exact.
#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
enum Value {
    Null,
    Integer { value: String },
    Float { value: f64 },
    Text { value: String },
    Blob {},
}

impl StmtResult {
    fn into_objects(self) -> Vec<Map<String, Json>> {
        let names: Vec<String> = self
            .cols
            .into_iter()
            .map(|c| c.name.unwrap_or_default())
            .collect();
        self.rows
            .into_iter()
            .map(|row| {
                names
                    .iter()
                    .cloned()
                    .zip(row.into_iter().map(Value::into_json))
                    .collect()
            })
            .collect()
    }
}

impl Value {
    fn into_json(self) -> Json {
        match self {
            Value::Null | Value::Blob {} => Json::Null,
            Value::Integer { value } => {
                value.parse::<i64>().map_or(Json::String(value), Json::from)
            }
            Value::Float { value } => Json::from(value),
            Value::Text { value } => Json::String(value),
        }
    }
}

fn rows<T: DeserializeOwned>(table: &str, rows: Vec<Map<String, Json>>) -> Result<Vec<T>> {
    rows.into_iter()
        .map(|r| {
            serde_json::from_value(Json::Object(r))
                .map_err(|e| Error::BadRow(format!("libSQL {table}: {e}")))
        })
        .collect()
}

// ── libSQL rows ─────────────────────────────────────────────────────

const QUERIES: [&str; 9] = [
    "SELECT COALESCE(MAX(version), 0) AS version FROM schema_migrations",
    "SELECT id, goal, created_at, strategy FROM paths",
    "SELECT path_id, target_id AS id FROM path_targets ORDER BY path_id, position",
    "SELECT path_id, atom_id AS id FROM path_subpath ORDER BY path_id, position",
    "SELECT ts, kind, path_id, atom_id, quiz_id, rating, payload FROM events ORDER BY id",
    "SELECT atom_id, body FROM overlay_lessons ORDER BY atom_id",
    "SELECT atom_id, quiz_id, difficulty, kind, question, answer, rubric \
     FROM overlay_quizzes ORDER BY quiz_id",
    "SELECT r.quiz_id, q.atom_id FROM overlay_removed_quizzes r \
     LEFT JOIN overlay_quizzes q ON q.quiz_id = r.quiz_id ORDER BY r.quiz_id",
    "SELECT path_id, quiz_id, stability, difficulty, due_at, last_reviewed_at, reps, lapses \
     FROM cards",
];

#[derive(Deserialize)]
struct Version {
    version: i64,
}

#[derive(Deserialize)]
struct PathRow {
    id: String,
    goal: String,
    #[serde(with = "ts")]
    created_at: DateTime<Utc>,
    strategy: Strategy,
}

/// A row of `path_targets` or `path_subpath`, in position order.
#[derive(Deserialize)]
struct PathAtom {
    path_id: String,
    id: String,
}

#[derive(Deserialize)]
struct EventRow {
    #[serde(with = "ts")]
    ts: DateTime<Utc>,
    kind: EventKind,
    path_id: String,
    atom_id: Option<String>,
    quiz_id: Option<String>,
    rating: Option<i64>,
    /// JSON with the event's `reason` and `user_answer`.
    payload: Option<String>,
}

#[derive(Deserialize, Default)]
struct EventText {
    reason: Option<String>,
    user_answer: Option<String>,
}

#[derive(Deserialize)]
struct LessonRow {
    atom_id: String,
    body: String,
}

#[derive(Deserialize)]
struct QuizRow {
    atom_id: String,
    quiz_id: String,
    difficulty: Difficulty,
    kind: Option<QuizType>,
    question: String,
    answer: String,
    rubric: Option<String>,
}

#[derive(Deserialize)]
struct RemovedRow {
    quiz_id: String,
    /// The tombstoned quiz's atom, when it was an authored quiz.
    atom_id: Option<String>,
}

/// A libSQL `cards` row: the FSRS state of an answered quiz.
#[derive(Deserialize)]
struct CardRow {
    path_id: String,
    quiz_id: String,
    stability: f64,
    difficulty: f64,
    #[serde(with = "ts")]
    due_at: DateTime<Utc>,
    #[serde(with = "ts")]
    last_reviewed_at: DateTime<Utc>,
    reps: u32,
    lapses: u32,
}

/// Everything the import reads, as of one snapshot.
struct Legacy {
    paths: Vec<PathFile>,
    /// Each path's events, in logged order.
    events: HashMap<String, Vec<Event>>,
    overlay: Vec<Edit>,
    cards: HashMap<String, Vec<CardRow>>,
}

async fn read(turso: &Turso) -> Result<Legacy> {
    let [
        version,
        paths,
        targets,
        subpaths,
        events,
        lessons,
        quizzes,
        removed,
        cards,
    ]: [_; 9] = turso
        .read(&QUERIES)
        .await?
        .try_into()
        .map_err(|_| Error::Turso("missing statement results".into()))?;
    let version = rows::<Version>("schema_migrations", version)?
        .first()
        .map_or(0, |v| v.version);
    if version != SCHEMA_VERSION {
        return Err(Error::Turso(format!(
            "schema is at migration {version}, not {SCHEMA_VERSION}; run the last libSQL release of mt against it first"
        )));
    }

    let mut targets = group(rows::<PathAtom>("path_targets", targets)?);
    let mut subpaths = group(rows::<PathAtom>("path_subpath", subpaths)?);
    let mut paths: Vec<PathFile> = rows::<PathRow>("paths", paths)?
        .into_iter()
        .map(|p| PathFile {
            targets: targets.remove(&p.id).unwrap_or_default(),
            subpath: subpaths.remove(&p.id).unwrap_or_default(),
            id: p.id,
            goal: p.goal,
            created_at: p.created_at,
            updated_at: p.created_at,
            strategy: p.strategy,
        })
        .collect();
    paths.sort_by(|a, b| {
        a.created_at
            .cmp(&b.created_at)
            .then_with(|| a.id.cmp(&b.id))
    });

    let events = group_events(rows("events", events)?)?;
    let overlay = overlay_edits(
        rows("overlay_lessons", lessons)?,
        rows("overlay_quizzes", quizzes)?,
        rows("overlay_removed_quizzes", removed)?,
    );
    let mut cards_by_path: HashMap<String, Vec<CardRow>> = HashMap::new();
    for c in rows::<CardRow>("cards", cards)? {
        cards_by_path.entry(c.path_id.clone()).or_default().push(c);
    }
    Ok(Legacy {
        paths,
        events,
        overlay,
        cards: cards_by_path,
    })
}

/// Each path's events, in logged order.
fn group_events(rows: Vec<EventRow>) -> Result<HashMap<String, Vec<Event>>> {
    let mut by_path: HashMap<String, Vec<Event>> = HashMap::new();
    for e in rows {
        let text: EventText = match &e.payload {
            Some(p) => serde_json::from_str(p)
                .map_err(|err| Error::BadRow(format!("libSQL events.payload: {err}")))?,
            None => EventText::default(),
        };
        by_path.entry(e.path_id.clone()).or_default().push(Event {
            ts: e.ts,
            kind: e.kind,
            path: e.path_id,
            atom: e.atom_id,
            quiz: e.quiz_id,
            payload: EventPayload {
                rating: e.rating.map(Rating::try_from).transpose()?,
                reason: text.reason,
                user_answer: text.user_answer,
            },
        });
    }
    Ok(by_path)
}

/// The edits that store the libSQL overlay. Quizzes come before
/// tombstones, since storing a quiz clears its tombstone.
fn overlay_edits(
    lessons: Vec<LessonRow>,
    quizzes: Vec<QuizRow>,
    removed: Vec<RemovedRow>,
) -> Vec<Edit> {
    let lessons = lessons.into_iter().map(|l| Edit::Lesson {
        atom_id: l.atom_id,
        body: l.body,
    });
    let quizzes = quizzes.into_iter().map(|q| Edit::Quiz {
        atom_id: q.atom_id,
        quiz: Quiz {
            id: q.quiz_id,
            difficulty: q.difficulty,
            kind: q.kind,
            question: q.question,
            answer: q.answer,
            rubric: q.rubric,
        },
        new: false,
    });
    let removed = removed.into_iter().map(|r| Edit::Remove {
        atom_id: r
            .atom_id
            .or_else(|| atom_from_quiz_id(&r.quiz_id))
            .unwrap_or_else(|| r.quiz_id.clone()),
        quiz_id: r.quiz_id,
    });
    lessons.chain(quizzes).chain(removed).collect()
}

fn group(atoms: Vec<PathAtom>) -> HashMap<String, Vec<String>> {
    let mut out: HashMap<String, Vec<String>> = HashMap::new();
    for a in atoms {
        out.entry(a.path_id).or_default().push(a.id);
    }
    out
}

// ── Import ──────────────────────────────────────────────────────────

/// What an import wrote and how its result compares with the source.
#[derive(Debug, Default)]
pub struct Report {
    pub paths: usize,
    /// Paths found already imported.
    pub skipped: usize,
    pub events: usize,
    pub overlay_edits: usize,
    /// Differences between the imported state and the libSQL rows.
    pub mismatches: Vec<String>,
}

/// Import everything `turso` holds into `db` and check the result.
pub async fn import(turso: &Turso, db: &Db) -> Result<Report> {
    let legacy = read(turso).await?;
    let mut report = Report {
        overlay_edits: legacy.overlay.len(),
        ..Report::default()
    };
    if !legacy.overlay.is_empty() {
        db.transact(&Want::overlay(), |_, changes| {
            legacy
                .overlay
                .iter()
                .cloned()
                .for_each(|e| changes.overlay(e));
            Ok(())
        })
        .await?;
    }
    for p in &legacy.paths {
        let events = legacy.events.get(&p.id).map_or(&[][..], Vec::as_slice);
        if import_path(db, p, events).await? {
            report.paths += 1;
            report.events += events.len();
        } else {
            report.skipped += 1;
        }
        // A path without events has no stream to rebuild from.
        if !events.is_empty() {
            cards::recompute(db, &p.id).await?;
        }
    }
    for p in &legacy.paths {
        let events = legacy.events.get(&p.id).map_or(&[][..], Vec::as_slice);
        let cards = legacy.cards.get(&p.id).map_or(&[][..], Vec::as_slice);
        report
            .mismatches
            .extend(verify(db, &p.id, events, cards).await?);
    }
    Ok(report)
}

/// Write `p` and its events in one transaction, unless it already exists.
async fn import_path(db: &Db, p: &PathFile, events: &[Event]) -> Result<bool> {
    let existing = db.read_one(Op::get_row::<PathFile>(&[&p.id])).await?;
    if existing.row::<PathFile>()?.is_some() {
        return Ok(false);
    }
    let latest = events.iter().map(|e| e.ts).max().unwrap_or(p.created_at);
    let stream = events_stream(&p.id);
    let mut ops = vec![
        Op::insert(p),
        Op::put(activity_key(&p.id), ts::format(latest)),
    ];
    ops.extend(events.chunks(PAGE).map(|c| Op::append(stream.clone(), c)));
    db.write(&ops).await?;
    Ok(true)
}

/// Differences between a path's stored events and cards and the libSQL ones.
async fn verify(
    db: &Db,
    path_id: &str,
    events: &[Event],
    legacy: &[CardRow],
) -> Result<Vec<String>> {
    let mut out = Vec::new();
    let stored = if events.is_empty() {
        Vec::new()
    } else {
        crate::event_log::load(db, path_id).await?
    };
    let stamps = |es: &[Event]| es.iter().map(|e| (e.ts, e.kind)).collect::<Vec<_>>();
    if stamps(&stored) != stamps(events) {
        out.push(format!(
            "{path_id}: {} events stored, {} in libSQL, or their times or kinds differ",
            stored.len(),
            events.len()
        ));
    }
    let view = db.snapshot(&Want::path(Some(path_id))).await?;
    let mut answered: BTreeMap<&str, (&cards::CardRow, CardState)> = view
        .path()?
        .cards
        .values()
        .filter_map(|c| Some((c.quiz_id.as_str(), (c, c.state?))))
        .collect();
    for l in legacy {
        let Some((c, s)) = answered.remove(l.quiz_id.as_str()) else {
            out.push(format!("{path_id}/{}: no answered card", l.quiz_id));
            continue;
        };
        let close = |a: f32, b: f64| (f64::from(a) - b).abs() <= FSRS_TOLERANCE * b.abs().max(1.0);
        if c.reps != l.reps
            || c.lapses != l.lapses
            || s.due != l.due_at
            || s.last_review != l.last_reviewed_at
            || !close(s.stability, l.stability)
            || !close(s.difficulty, l.difficulty)
        {
            out.push(format!(
                "{path_id}/{}: rebuilt (reps {}, lapses {}, due {}, stability {}, difficulty {}) \
                 but libSQL has (reps {}, lapses {}, due {}, stability {}, difficulty {})",
                l.quiz_id,
                c.reps,
                c.lapses,
                ts::format(s.due),
                s.stability,
                s.difficulty,
                l.reps,
                l.lapses,
                ts::format(l.due_at),
                l.stability,
                l.difficulty,
            ));
        }
    }
    out.extend(
        answered
            .into_keys()
            .map(|q| format!("{path_id}/{q}: answered card missing from libSQL")),
    );
    Ok(out)
}

/// `mt import-turso`: import, print a summary, and fail on any mismatch.
pub async fn cmd_import_turso(db: &Db, url: &str, token: Option<String>) -> Result<()> {
    let r = import(&Turso::new(url, token)?, db).await?;
    eprintln!(
        "imported {} paths ({} events) and {} overlay edits; {} paths were already imported",
        r.paths, r.events, r.overlay_edits, r.skipped
    );
    for m in &r.mismatches {
        eprintln!("mismatch: {m}");
    }
    if r.mismatches.is_empty() {
        eprintln!("every path's events and cards match libSQL");
        Ok(())
    } else {
        Err(Error::ImportMismatch(r.mismatches.len()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn libsql_urls_are_reached_over_https() {
        let t = Turso::new("libsql://mt-alex.turso.io", None).unwrap();
        assert_eq!(t.pipeline_url, "https://mt-alex.turso.io/v2/pipeline");
        let local = Turso::new("http://127.0.0.1:8080/", None).unwrap();
        assert_eq!(local.pipeline_url, "http://127.0.0.1:8080/v2/pipeline");
    }

    #[test]
    fn integers_keep_their_type_and_blobs_read_as_null() {
        let r: StmtResult = serde_json::from_str(
            r#"{"cols":[{"name":"n"},{"name":"b"},{"name":"s"}],
                "rows":[[{"type":"integer","value":"9007199254740993"},{"type":"blob","base64":"AA=="},{"type":"text","value":"x"}]]}"#,
        )
        .unwrap();
        let row = &r.into_objects()[0];
        assert_eq!(row["n"], Json::from(9_007_199_254_740_993_i64));
        assert_eq!(row["b"], Json::Null);
        assert_eq!(row["s"], "x");
    }
}
