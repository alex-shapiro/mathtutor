//! Migration from a libSQL database to Baserunna for `mt import-turso`.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use chrono::{DateTime, Utc};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value as Json};

use crate::answer::atom_from_quiz_id;
use crate::cards::{self, CardState};
use crate::db::client::api::PAGE_MAX;
use crate::db::{Db, Op, Txn, run_txn, ts};
use crate::event_log::{self, Event};
use crate::graph::Quiz;
use crate::overlay::{Edit, Overlay};
use crate::path::PathFile;
use crate::schema::{IMPORT_STATE_KEY, ImportMarker, OAuthClient, activity_key, events_stream};
use crate::types::{Difficulty, QuizType, Rating, Strategy};
use crate::view::{OVERLAY_VERSION_KEY, Want};
use crate::{Error, Result};

const SCHEMA_VERSION: i64 = 4;

/// Number of legacy rows written per transaction, well below the server's operation limit.
const IMPORT_WRITE_BATCH: usize = 500;

/// Relative tolerance for comparing `f32` FSRS parameters against `f64` values stored in libSQL.
const FSRS_TOLERANCE: f64 = 1e-4;

pub struct Turso {
    http: reqwest::Client,
    pipeline_url: String,
    token: Option<String>,
}

impl Turso {
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
        let mut results = r.results.into_iter();
        if let Some(StreamResult::Error { error }) = results.next() {
            return Err(Error::Turso(format!("BEGIN: {}", error.message)));
        }
        let mut sets = Vec::with_capacity(queries.len());
        for (sql, result) in queries.iter().zip(results) {
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

/// Hrana wire value where 64-bit integers are encoded as strings to avoid JSON float precision loss.
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

fn decode_rows<T: DeserializeOwned>(table: &str, rows: Vec<Map<String, Json>>) -> Result<Vec<T>> {
    rows.into_iter()
        .map(|r| {
            serde_json::from_value(Json::Object(r))
                .map_err(|e| Error::BadRow(format!("libSQL {table}: {e}")))
        })
        .collect()
}

const QUERIES: [&str; 10] = [
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
    "SELECT client_id, client_name, redirect_uris, created_at FROM oauth_clients",
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

#[derive(Deserialize)]
struct PathAtom {
    path_id: String,
    id: String,
}

#[derive(Deserialize)]
struct EventRow {
    #[serde(with = "ts")]
    ts: DateTime<Utc>,
    kind: String,
    path_id: String,
    atom_id: Option<String>,
    quiz_id: Option<String>,
    rating: Option<i64>,
    /// JSON object with the fields of the event that have no column.
    payload: Option<String>,
}

impl TryFrom<EventRow> for Event {
    type Error = Error;

    /// Decode every column and payload field of `r`, and reject a field that the kind of `r` does not record.
    fn try_from(r: EventRow) -> Result<Self> {
        let bad = |what: String| {
            Error::BadRow(format!(
                "libSQL events: {what} in {} event of {} at {}",
                r.kind,
                r.path_id,
                ts::format(r.ts)
            ))
        };
        let mut fields = Map::new();
        fields.insert("kind".into(), Json::String(r.kind.clone()));
        if let Some(atom) = &r.atom_id {
            fields.insert("atom".into(), Json::String(atom.clone()));
        }
        if let Some(quiz) = &r.quiz_id {
            fields.insert("quiz".into(), Json::String(quiz.clone()));
        }
        if let Some(rating) = r.rating {
            fields.insert(
                "rating".into(),
                serde_json::to_value(Rating::try_from(rating)?)?,
            );
        }
        if let Some(payload) = &r.payload {
            let parsed = serde_json::from_str(payload).map_err(|e| bad(format!("payload {e}")))?;
            let Json::Object(extra) = parsed else {
                return Err(bad("a payload that is not an object".into()));
            };
            for (name, value) in extra {
                if fields.insert(name.clone(), value).is_some() {
                    return Err(bad(format!("payload field `{name}` that repeats a column")));
                }
            }
        }
        let kind = serde_json::from_value(Json::Object(fields)).map_err(|e| bad(e.to_string()))?;
        Ok(Event {
            ts: r.ts,
            path: r.path_id,
            kind,
        })
    }
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
    atom_id: Option<String>,
}

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

#[derive(Deserialize)]
struct OAuthClientRow {
    client_id: String,
    client_name: Option<String>,
    redirect_uris: String,
    #[serde(with = "ts")]
    created_at: DateTime<Utc>,
}

impl TryFrom<OAuthClientRow> for OAuthClient {
    type Error = Error;

    fn try_from(r: OAuthClientRow) -> Result<Self> {
        let redirect_uris = serde_json::from_str(&r.redirect_uris)
            .map_err(|e| Error::BadRow(format!("libSQL oauth_clients.redirect_uris: {e}")))?;
        Ok(Self {
            client_id: r.client_id,
            client_name: r.client_name,
            redirect_uris,
            created_at: r.created_at,
        })
    }
}

struct Legacy {
    paths: Vec<PathFile>,
    events: HashMap<String, Vec<Event>>,
    overlay: Vec<Edit>,
    cards: HashMap<String, Vec<CardRow>>,
    oauth_clients: Vec<OAuthClient>,
}

async fn read_legacy(turso: &Turso) -> Result<Legacy> {
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
        oauth_clients,
    ]: [_; 10] = turso
        .read(&QUERIES)
        .await?
        .try_into()
        .map_err(|_| Error::Turso("missing statement results".into()))?;
    let version = decode_rows::<Version>("schema_migrations", version)?
        .first()
        .map_or(0, |v| v.version);
    if version != SCHEMA_VERSION {
        return Err(Error::Turso(format!(
            "schema is at migration {version}, not {SCHEMA_VERSION}; run the last libSQL release of mt against it first"
        )));
    }

    let mut targets = group_by_path(decode_rows::<PathAtom>("path_targets", targets)?);
    let mut subpaths = group_by_path(decode_rows::<PathAtom>("path_subpath", subpaths)?);
    let mut paths: Vec<PathFile> = decode_rows::<PathRow>("paths", paths)?
        .into_iter()
        .map(|p| PathFile {
            targets: targets.remove(&p.id).unwrap_or_default(),
            subpath: subpaths.remove(&p.id).unwrap_or_default(),
            id: p.id,
            goal: p.goal,
            created_at: p.created_at,
            strategy: p.strategy,
        })
        .collect();
    paths.sort_by(|a, b| {
        a.created_at
            .cmp(&b.created_at)
            .then_with(|| a.id.cmp(&b.id))
    });

    let events = group_events(decode_rows("events", events)?)?;
    let overlay = overlay_edits(
        decode_rows("overlay_lessons", lessons)?,
        decode_rows("overlay_quizzes", quizzes)?,
        decode_rows("overlay_removed_quizzes", removed)?,
    );
    let mut cards_by_path: HashMap<String, Vec<CardRow>> = HashMap::new();
    for c in decode_rows::<CardRow>("cards", cards)? {
        cards_by_path.entry(c.path_id.clone()).or_default().push(c);
    }
    let oauth_clients = decode_rows::<OAuthClientRow>("oauth_clients", oauth_clients)?
        .into_iter()
        .map(OAuthClient::try_from)
        .collect::<Result<_>>()?;
    Ok(Legacy {
        paths,
        events,
        overlay,
        cards: cards_by_path,
        oauth_clients,
    })
}

fn group_events(rows: Vec<EventRow>) -> Result<HashMap<String, Vec<Event>>> {
    let mut by_path: HashMap<String, Vec<Event>> = HashMap::new();
    for row in rows {
        let e = Event::try_from(row)?;
        by_path.entry(e.path.clone()).or_default().push(e);
    }
    Ok(by_path)
}

/// Order quiz edits before tombstones because upserting a quiz deletes its tombstone row.
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

fn group_by_path(atoms: Vec<PathAtom>) -> HashMap<String, Vec<String>> {
    let mut out: HashMap<String, Vec<String>> = HashMap::new();
    for a in atoms {
        out.entry(a.path_id).or_default().push(a.id);
    }
    out
}

#[derive(Debug, Default)]
pub struct Report {
    pub paths: usize,
    pub skipped: usize,
    pub events: usize,
    pub overlay_edits: usize,
    pub oauth_clients: usize,
    pub mismatches: Vec<String>,
}

/// Mark `db` as an import target, or fail if an import would overwrite newer data.
async fn claim_import_target(db: &Db) -> Result<()> {
    run_txn!(db, |txn| claim_import_target_in(&mut txn).await)
}

async fn claim_import_target_in(txn: &mut Txn) -> Result<()> {
    let results = txn
        .run(&[
            Op::get(IMPORT_STATE_KEY),
            Op::get(OVERLAY_VERSION_KEY),
            Op::query_all::<PathFile>(),
        ])
        .await?;
    let [marker, overlay_version, paths]: [_; 3] = results
        .try_into()
        .map_err(|_| Error::BadRow("import target read returned the wrong result count".into()))?;
    match ImportMarker::from_entry(marker.entry()?)? {
        Some(ImportMarker::Open) => return Ok(()),
        Some(ImportMarker::Complete) => return Err(Error::ImportAlreadyComplete),
        None => {}
    }
    if overlay_version.entry()?.is_some() || !paths.rows::<PathFile>()?.is_empty() {
        return Err(Error::ImportTargetNotEmpty);
    }
    txn.write([ImportMarker::Open.write_op()]);
    Ok(())
}

pub async fn import(turso: &Turso, db: &Db) -> Result<Report> {
    let db = &db.for_import();
    let legacy = read_legacy(turso).await?;
    for (path_id, events) in &legacy.events {
        cards::fold_history(path_id, events)?;
    }
    claim_import_target(db).await?;
    let mut report = Report {
        overlay_edits: legacy.overlay.len(),
        oauth_clients: legacy.oauth_clients.len(),
        ..Report::default()
    };
    for edits in legacy.overlay.chunks(IMPORT_WRITE_BATCH) {
        db.transact(&Want::overlay(), |_, changes| {
            edits.iter().cloned().for_each(|e| changes.overlay(e));
            Ok(())
        })
        .await?;
    }
    for clients in legacy.oauth_clients.chunks(IMPORT_WRITE_BATCH) {
        db.write(clients.iter().map(Op::upsert)).await?;
    }
    for p in &legacy.paths {
        let events = legacy.events.get(&p.id).map_or(&[][..], Vec::as_slice);
        if import_path(db, p, events).await? {
            report.paths += 1;
            report.events += events.len();
        } else {
            report.skipped += 1;
        }
        if !event_log::load(db, &p.id).await?.is_empty() {
            cards::recompute(db, &p.id).await?;
        }
    }
    let source = read_legacy(turso).await?;
    report
        .mismatches
        .extend(verify_overlay(db, &source.overlay).await?);
    for p in &source.paths {
        let events = source.events.get(&p.id).map_or(&[][..], Vec::as_slice);
        let cards = source.cards.get(&p.id).map_or(&[][..], Vec::as_slice);
        report
            .mismatches
            .extend(verify_path(db, p, events, cards).await?);
    }
    if report.mismatches.is_empty() {
        db.write([ImportMarker::Complete.write_op()]).await?;
    }
    Ok(report)
}

/// Compare the stored overlay, read without the cache, with the legacy edits.
async fn verify_overlay(db: &Db, edits: &[Edit]) -> Result<Vec<String>> {
    let mut legacy = Overlay::default();
    for e in edits {
        legacy.apply(e);
    }
    let stored = db
        .without_overlay_cache()
        .snapshot(&Want::overlay())
        .await?
        .overlay;
    let atoms: BTreeSet<&String> = legacy.atoms.keys().chain(stored.atoms.keys()).collect();
    let mut out = Vec::new();
    for atom in atoms {
        let same = serde_json::to_value(legacy.atoms.get(atom))?
            == serde_json::to_value(stored.atoms.get(atom))?;
        if !same {
            out.push(format!(
                "overlay/{atom}: stored content differs from libSQL"
            ));
        }
    }
    Ok(out)
}

async fn import_path(db: &Db, p: &PathFile, events: &[Event]) -> Result<bool> {
    let existing = db.read_one(Op::get_row::<PathFile>(&p.id)).await?;
    if existing.row::<PathFile>()?.is_some() {
        return Ok(false);
    }
    let latest = events.iter().map(|e| e.ts).max().unwrap_or(p.created_at);
    let stream = events_stream(&p.id);
    let mut ops = vec![
        Op::insert(p),
        Op::put(activity_key(&p.id), ts::format(latest)),
    ];
    ops.extend(
        events
            .chunks(PAGE_MAX)
            .map(|c| Op::append(stream.clone(), c)),
    );
    db.write(ops).await?;
    Ok(true)
}

async fn verify_path(
    db: &Db,
    path: &PathFile,
    events: &[Event],
    legacy: &[CardRow],
) -> Result<Vec<String>> {
    let path_id = path.id.as_str();
    let mut out = Vec::new();
    let view = db.snapshot(&Want::path(Some(path_id))).await?;
    let Ok(path_state) = view.path() else {
        out.push(format!("{path_id}: path missing from Baserunna"));
        return Ok(out);
    };
    let stored = event_log::load(db, path_id).await?;
    if serde_json::to_value(&stored)? != serde_json::to_value(events)? {
        out.push(format!(
            "{path_id}: {} events stored and {} in libSQL, or their contents differ",
            stored.len(),
            events.len()
        ));
    }
    let stored_path = &path_state.file;
    if stored_path.goal != path.goal
        || stored_path.strategy != path.strategy
        || stored_path.targets != path.targets
        || stored_path.subpath != path.subpath
        || ts::format(stored_path.created_at) != ts::format(path.created_at)
    {
        out.push(format!("{path_id}: stored path differs from libSQL"));
    }
    let mut answered: BTreeMap<&str, (&cards::CardRow, CardState)> = path_state
        .cards
        .values()
        .filter_map(|c| Some((c.quiz_id.as_str(), (c, c.state?))))
        .collect();
    for l in legacy {
        let Some((c, s)) = answered.remove(l.quiz_id.as_str()) else {
            out.push(format!("{path_id}/{}: answered card missing", l.quiz_id));
            continue;
        };
        let close = |a: f32, b: f64| (f64::from(a) - b).abs() <= FSRS_TOLERANCE * b.abs().max(1.0);
        if c.reps != l.reps
            || c.lapses != l.lapses
            || ts::format(s.due) != ts::format(l.due_at)
            || ts::format(s.last_review) != ts::format(l.last_reviewed_at)
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

pub async fn cmd_import_turso(db: &Db, url: &str, token: Option<String>) -> Result<()> {
    let r = import(&Turso::new(url, token)?, db).await?;
    eprintln!(
        "imported {} paths ({} events), {} overlay edits, and {} OAuth clients; {} paths were already imported",
        r.paths, r.events, r.overlay_edits, r.oauth_clients, r.skipped
    );
    for m in &r.mismatches {
        eprintln!("mismatch: {m}");
    }
    if r.mismatches.is_empty() {
        eprintln!("every path, event, card, and overlay entry matches libSQL");
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
