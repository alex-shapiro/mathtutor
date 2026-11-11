//! Migration from a libSQL database to Baserunna for `mt import-turso`.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use chrono::{DateTime, Utc};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value as Json};

use crate::cards::{self, CardState};
use crate::db::client::api::Selection;
use crate::db::{Db, Op, Row, Txn, run_txn, ts};
use crate::event_log::{self, Event};
use crate::overlay::{Overlay, RowChange};
use crate::path::PathRow;
use crate::schema::{ImportMarker, LessonRow, OAuthClient, QuizRow, RemovedQuizRow};
use crate::types::{Difficulty, QuizType, Rating, Strategy};
use crate::view::{OVERLAY_VERSION_KEY, PathState, View, ViewRequest};
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

/// Remove the rows of `table` from `rows` and decode them.
fn decode_rows<T: DeserializeOwned>(rows: &mut SourceRows, table: SourceTable) -> Result<Vec<T>> {
    rows.remove(&table)
        .unwrap_or_default()
        .into_iter()
        .map(|r| {
            serde_json::from_value(Json::Object(r))
                .map_err(|e| Error::BadRow(format!("libSQL {}: {e}", table.name())))
        })
        .collect()
}

/// Declare [`SourceTable`] with the name of each source table and the query that reads it.
macro_rules! source_tables {
    ($($table:ident = $name:literal => $query:expr,)+) => {
        /// Table of a libSQL source database, read by one query of [`read_legacy`].
        #[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
        enum SourceTable {
            $($table,)+
        }

        impl SourceTable {
            const ALL: &[SourceTable] = &[$(SourceTable::$table,)+];

            /// Return the name of this table in libSQL.
            fn name(self) -> &'static str {
                match self {
                    $(SourceTable::$table => $name,)+
                }
            }

            /// Return the query that reads this table.
            fn query(self) -> &'static str {
                match self {
                    $(SourceTable::$table => $query,)+
                }
            }
        }
    };
}

source_tables! {
    SchemaMigrations = "schema_migrations" =>
        "SELECT COALESCE(MAX(version), 0) AS version FROM schema_migrations",
    Paths = "paths" => "SELECT id, goal, created_at, strategy FROM paths",
    PathTargets = "path_targets" =>
        "SELECT path_id, target_id AS id FROM path_targets ORDER BY path_id, position",
    PathSubpath = "path_subpath" =>
        "SELECT path_id, atom_id AS id FROM path_subpath ORDER BY path_id, position",
    Events = "events" =>
        "SELECT ts, kind, path_id, atom_id, quiz_id, rating, payload FROM events ORDER BY id",
    OverlayLessons = "overlay_lessons" =>
        "SELECT atom_id, body FROM overlay_lessons ORDER BY atom_id",
    OverlayQuizzes = "overlay_quizzes" =>
        "SELECT atom_id, quiz_id, difficulty, kind, question, answer, rubric \
         FROM overlay_quizzes ORDER BY quiz_id",
    OverlayRemovedQuizzes = "overlay_removed_quizzes" =>
        "SELECT r.quiz_id, q.atom_id FROM overlay_removed_quizzes r \
         LEFT JOIN overlay_quizzes q ON q.quiz_id = r.quiz_id ORDER BY r.quiz_id",
    Cards = "cards" =>
        "SELECT path_id, quiz_id, stability, difficulty, due_at, last_reviewed_at, reps, lapses \
         FROM cards",
    OAuthClients = "oauth_clients" =>
        "SELECT client_id, client_name, redirect_uris, created_at FROM oauth_clients",
}

/// Rows that each source table returned.
type SourceRows = HashMap<SourceTable, Vec<Map<String, Json>>>;

/// Number of rows in each source table.
type RowCounts = BTreeMap<SourceTable, usize>;

/// Row of the libSQL `schema_migrations` query.
#[derive(Deserialize)]
struct LegacyVersion {
    version: i64,
}

/// Row of the libSQL `paths` table.
#[derive(Deserialize)]
struct LegacyPathRow {
    id: String,
    goal: String,
    #[serde(with = "ts")]
    created_at: DateTime<Utc>,
    strategy: Strategy,
}

/// Row of the libSQL `path_targets` or `path_subpath` table.
#[derive(Deserialize)]
struct LegacyPathAtom {
    path_id: String,
    id: String,
}

/// Row of the libSQL `events` table.
#[derive(Deserialize)]
struct LegacyEventRow {
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

impl TryFrom<LegacyEventRow> for Event {
    type Error = Error;

    /// Decode every column and payload field of `r`, and reject a field that the kind of `r` does not record.
    fn try_from(r: LegacyEventRow) -> Result<Self> {
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
                serde_json::to_value(
                    Rating::from_grade(rating).ok_or_else(|| bad(format!("rating {rating}")))?,
                )?,
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

/// Row of the libSQL `overlay_lessons` table.
#[derive(Deserialize)]
struct LegacyLessonRow {
    atom_id: String,
    body: String,
}

/// Row of the libSQL `overlay_quizzes` table.
#[derive(Deserialize)]
struct LegacyQuizRow {
    atom_id: String,
    quiz_id: String,
    difficulty: Difficulty,
    kind: Option<QuizType>,
    question: String,
    answer: String,
    rubric: Option<String>,
}

/// Row of the libSQL `overlay_removed_quizzes` table, with the atom of the quiz if libSQL holds it.
#[derive(Deserialize)]
struct LegacyRemovedRow {
    quiz_id: String,
    atom_id: Option<String>,
}

/// Row of the libSQL `cards` table.
#[derive(Deserialize)]
struct LegacyCardRow {
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

/// Row of the libSQL `oauth_clients` table.
#[derive(Deserialize)]
struct LegacyOAuthClientRow {
    client_id: String,
    client_name: Option<String>,
    redirect_uris: String,
    #[serde(with = "ts")]
    created_at: DateTime<Utc>,
}

impl TryFrom<LegacyOAuthClientRow> for OAuthClient {
    type Error = Error;

    fn try_from(r: LegacyOAuthClientRow) -> Result<Self> {
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

/// Rows of a libSQL database, decoded into the types of this release.
struct Legacy {
    /// Number of rows that each source table returned before decoding.
    row_counts: RowCounts,
    paths: Vec<PathRow>,
    events: HashMap<String, Vec<Event>>,
    overlay: Vec<RowChange>,
    cards: HashMap<String, Vec<LegacyCardRow>>,
    oauth_clients: Vec<OAuthClient>,
}

async fn read_legacy(turso: &Turso) -> Result<Legacy> {
    let queries: Vec<&str> = SourceTable::ALL.iter().map(|t| t.query()).collect();
    let results = turso.read(&queries).await?;
    if results.len() != SourceTable::ALL.len() {
        return Err(Error::Turso("missing statement results".into()));
    }
    let mut rows: SourceRows = SourceTable::ALL.iter().copied().zip(results).collect();
    let row_counts: RowCounts = rows
        .iter()
        .filter(|(table, _)| **table != SourceTable::SchemaMigrations)
        .map(|(table, r)| (*table, r.len()))
        .collect();
    let version = decode_rows::<LegacyVersion>(&mut rows, SourceTable::SchemaMigrations)?
        .first()
        .map_or(0, |v| v.version);
    if version != SCHEMA_VERSION {
        return Err(Error::Turso(format!(
            "schema is at migration {version}, not {SCHEMA_VERSION}; run the last libSQL release of mt against it first"
        )));
    }

    let mut targets = group_by_path(decode_rows::<LegacyPathAtom>(
        &mut rows,
        SourceTable::PathTargets,
    )?);
    let mut subpaths = group_by_path(decode_rows::<LegacyPathAtom>(
        &mut rows,
        SourceTable::PathSubpath,
    )?);
    let mut paths: Vec<PathRow> = decode_rows::<LegacyPathRow>(&mut rows, SourceTable::Paths)?
        .into_iter()
        .map(|p| PathRow {
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

    let events = group_events(decode_rows(&mut rows, SourceTable::Events)?)?;
    let overlay = overlay_changes(
        decode_rows(&mut rows, SourceTable::OverlayLessons)?,
        decode_rows(&mut rows, SourceTable::OverlayQuizzes)?,
        decode_rows(&mut rows, SourceTable::OverlayRemovedQuizzes)?,
    );
    let mut cards_by_path: HashMap<String, Vec<LegacyCardRow>> = HashMap::new();
    for c in decode_rows::<LegacyCardRow>(&mut rows, SourceTable::Cards)? {
        cards_by_path.entry(c.path_id.clone()).or_default().push(c);
    }
    let oauth_clients = decode_rows::<LegacyOAuthClientRow>(&mut rows, SourceTable::OAuthClients)?
        .into_iter()
        .map(OAuthClient::try_from)
        .collect::<Result<_>>()?;
    Ok(Legacy {
        row_counts,
        paths,
        events,
        overlay,
        cards: cards_by_path,
        oauth_clients,
    })
}

fn group_events(rows: Vec<LegacyEventRow>) -> Result<HashMap<String, Vec<Event>>> {
    let mut by_path: HashMap<String, Vec<Event>> = HashMap::new();
    for row in rows {
        let e = Event::try_from(row)?;
        by_path.entry(e.path.clone()).or_default().push(e);
    }
    Ok(by_path)
}

/// Return the overlay row changes that store the legacy overlay rows.
fn overlay_changes(
    lessons: Vec<LegacyLessonRow>,
    quizzes: Vec<LegacyQuizRow>,
    removed: Vec<LegacyRemovedRow>,
) -> Vec<RowChange> {
    let quiz_atoms: HashMap<String, String> = quizzes
        .iter()
        .map(|q| (q.quiz_id.clone(), q.atom_id.clone()))
        .collect();
    let lessons = lessons.into_iter().map(|l| {
        RowChange::PutLesson(LessonRow {
            atom_id: l.atom_id,
            body: l.body,
        })
    });
    let quizzes = quizzes.into_iter().map(|q| {
        RowChange::PutQuiz(QuizRow {
            quiz_id: q.quiz_id,
            atom_id: q.atom_id,
            difficulty: q.difficulty,
            kind: q.kind,
            question: q.question,
            answer: q.answer,
            rubric: q.rubric,
        })
    });
    let removed = removed.into_iter().map(|r| {
        RowChange::PutRemoved(RemovedQuizRow {
            atom_id: r
                .atom_id
                .or_else(|| quiz_atoms.get(&r.quiz_id).cloned())
                .or_else(|| legacy_quiz_atom(&r.quiz_id).map(str::to_string))
                .unwrap_or_else(|| r.quiz_id.clone()),
            quiz_id: r.quiz_id,
        })
    });
    lessons.chain(quizzes).chain(removed).collect()
}

/// Return the atom of `quiz_id` in the `<atom>.q<N>` form that libSQL releases gave every quiz ID.
fn legacy_quiz_atom(quiz_id: &str) -> Option<&str> {
    quiz_id.rfind(".q").map(|pos| &quiz_id[..pos])
}

fn group_by_path(atoms: Vec<LegacyPathAtom>) -> HashMap<String, Vec<String>> {
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
    let first_path = Op::QueryRows {
        table: PathRow::TABLE.into(),
        selection: Selection::default(),
        limit: 1,
        reverse: false,
        cursor: None,
    };
    let results = txn.run(&[Op::get(OVERLAY_VERSION_KEY), first_path]).await?;
    let [overlay_version, paths]: [_; 2] = results
        .try_into()
        .map_err(|_| Error::BadRow("import target read returned the wrong result count".into()))?;
    match txn.import_marker() {
        Some(ImportMarker::Open) => return Ok(()),
        Some(ImportMarker::Complete) => return Err(Error::ImportAlreadyComplete),
        None => {}
    }
    if overlay_version.entry()?.is_some() || !paths.rows::<PathRow>()?.is_empty() {
        return Err(Error::ImportTargetNotEmpty);
    }
    txn.set_import_marker(ImportMarker::Open);
    Ok(())
}

pub async fn import(turso: &Turso, db: &Db) -> Result<Report> {
    let db = &db.for_import();
    let legacy = read_legacy(turso).await?;
    claim_import_target(db).await?;
    let mut report = Report {
        overlay_edits: legacy.overlay.len(),
        oauth_clients: legacy.oauth_clients.len(),
        ..Report::default()
    };
    for batch in legacy.overlay.chunks(IMPORT_WRITE_BATCH) {
        db.transact(&ViewRequest::overlay(), |view, changes| {
            batch
                .iter()
                .try_for_each(|c| changes.overlay(view, c.clone()))
        })
        .await?;
    }
    let auth = db.with_auth_tables();
    for clients in legacy.oauth_clients.chunks(IMPORT_WRITE_BATCH) {
        auth.replace_oauth_clients(clients).await?;
    }
    let history = |p: &PathRow| legacy.events.get(&p.id).map_or(&[][..], Vec::as_slice);
    let mut paths: Vec<&PathRow> = legacy.paths.iter().collect();
    paths.sort_by_key(|p| latest_activity(p, history(p)));
    for p in paths {
        let events = history(p);
        if import_path(db, p, events).await? {
            report.paths += 1;
            report.events += events.len();
        } else {
            report.skipped += 1;
        }
    }
    let source = read_legacy(turso).await?;
    let stored = db
        .without_overlay_cache()
        .snapshot(&ViewRequest::all_paths())
        .await?;
    report
        .mismatches
        .extend(verify_overlay(stored.overlay()?, &source.overlay)?);
    let stored_paths: HashMap<&str, &PathState> = stored
        .paths()
        .into_iter()
        .map(|s| (s.row.id.as_str(), s))
        .collect();
    let mut stored_events = 0;
    for p in &source.paths {
        let events = source.events.get(&p.id).map_or(&[][..], Vec::as_slice);
        let cards = source.cards.get(&p.id).map_or(&[][..], Vec::as_slice);
        let stored_path = stored_paths.get(p.id.as_str()).copied();
        let stored_history = match stored_path {
            Some(_) => event_log::load(db, &p.id).await?,
            None => Vec::new(),
        };
        report
            .mismatches
            .extend(verify_path(p, events, cards, stored_path, &stored_history)?);
        stored_events += stored_history.len();
    }
    report
        .mismatches
        .extend(verify_row_counts(db, &stored, stored_events, &source.row_counts).await?);
    if report.mismatches.is_empty() {
        db.write_import_marker(ImportMarker::Complete).await?;
    }
    Ok(report)
}

/// Compare the `stored` overlay with the overlay that the legacy row `changes` store.
fn verify_overlay(stored: &Overlay, changes: &[RowChange]) -> Result<Vec<String>> {
    let mut legacy = Overlay::default();
    for change in changes {
        legacy.apply(change.clone());
    }
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

/// Write path `p`, its `events`, and the `cards` and `taught` rows that the events produce in one transaction, or return `false` if the path exists.
async fn import_path(db: &Db, p: &PathRow, events: &[Event]) -> Result<bool> {
    let existing = db
        .snapshot(&ViewRequest::path(Some(&p.id)).without_overlay())
        .await?;
    if existing.path().is_ok() {
        return Ok(false);
    }
    db.transact(&ViewRequest::nothing(), |view, changes| {
        changes.create_path_with_history(view, p, events, latest_activity(p, events))
    })
    .await?;
    Ok(true)
}

/// Return the time of the latest event of path `p`, or its creation time if `events` is empty.
fn latest_activity(p: &PathRow, events: &[Event]) -> DateTime<Utc> {
    events.iter().map(|e| e.ts).max().unwrap_or(p.created_at)
}

/// Compare `stored`, the stored state of `path`, and `stored_events` with the legacy `events` and cards of `path`, and return the differences.
fn verify_path(
    path: &PathRow,
    events: &[Event],
    legacy: &[LegacyCardRow],
    stored: Option<&PathState>,
    stored_events: &[Event],
) -> Result<Vec<String>> {
    let path_id = path.id.as_str();
    let mut out = Vec::new();
    let Some(path_state) = stored else {
        out.push(format!("{path_id}: path missing from Baserunna"));
        return Ok(out);
    };
    if serde_json::to_value(stored_events)? != serde_json::to_value(events)? {
        out.push(format!(
            "{path_id}: {} events stored and {} in libSQL, or their contents differ",
            stored_events.len(),
            events.len()
        ));
    }
    let stored_path = &path_state.row;
    if stored_path.goal != path.goal
        || stored_path.strategy != path.strategy
        || stored_path.targets != path.targets
        || stored_path.subpath != path.subpath
        || ts::format(stored_path.created_at) != ts::format(path.created_at)
    {
        out.push(format!("{path_id}: stored path differs from libSQL"));
    }
    let mut answered: BTreeMap<&str, (&cards::Card, CardState)> = path_state
        .projection
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

/// Compare the number of rows in each source table with the number of rows that `stored` and the event streams hold for it.
async fn verify_row_counts(
    db: &Db,
    stored: &View,
    stored_events: usize,
    source: &RowCounts,
) -> Result<Vec<String>> {
    let paths = stored.paths();
    let atoms = stored.overlay()?.atoms.values();
    let mut oauth_clients = 0;
    let auth = db.with_auth_tables();
    for page in auth.read_pages(Op::query_all::<OAuthClient>()).await? {
        oauth_clients += page.rows::<OAuthClient>()?.len();
    }
    let stored = RowCounts::from([
        (SourceTable::Paths, paths.len()),
        (
            SourceTable::PathTargets,
            paths.iter().map(|p| p.row.targets.len()).sum(),
        ),
        (
            SourceTable::PathSubpath,
            paths.iter().map(|p| p.row.subpath.len()).sum(),
        ),
        (SourceTable::Events, stored_events),
        (
            SourceTable::OverlayLessons,
            atoms.clone().filter(|a| a.lesson.is_some()).count(),
        ),
        (
            SourceTable::OverlayQuizzes,
            atoms.clone().map(|a| a.quizzes.len()).sum(),
        ),
        (
            SourceTable::OverlayRemovedQuizzes,
            atoms.map(|a| a.removed.len()).sum(),
        ),
        (
            SourceTable::Cards,
            paths
                .iter()
                .flat_map(|p| p.projection.cards.values())
                .filter(|c| c.state.is_some())
                .count(),
        ),
        (SourceTable::OAuthClients, oauth_clients),
    ]);
    Ok(source
        .iter()
        .filter(|(table, n)| stored.get(*table) != Some(n))
        .map(|(table, n)| {
            let found = stored.get(table).copied().unwrap_or_default();
            let table = table.name();
            format!("{table}: {n} rows in libSQL, but the import stored {found}")
        })
        .collect())
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
        eprintln!(
            "every libSQL row is stored, and every path, event, card, and overlay entry matches libSQL"
        );
        Ok(())
    } else {
        Err(Error::ImportMismatch(r.mismatches.len()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn answered_path() -> (PathRow, Vec<Event>, PathState) {
        let path = PathRow {
            id: "p".into(),
            goal: "g".into(),
            created_at: Utc::now(),
            targets: vec!["fnd.1.1.1".into()],
            strategy: Strategy::BottomUp,
            subpath: Vec::new(),
        };
        let answered = event_log::EventKind::QuizAnswered {
            atom: None,
            quiz: "fnd.1.1.1.q1".into(),
            rating: Rating::Good,
            user_answer: None,
        };
        let events = vec![Event::now("p", answered)];
        let state = PathState {
            row: path.clone(),
            updated_at: Utc::now(),
            projection: cards::Projection::from_events(&events).unwrap(),
        };
        (path, events, state)
    }

    fn legacy_card(state: &PathState) -> LegacyCardRow {
        let card = &state.projection.cards["fnd.1.1.1.q1"];
        let s = card.state.unwrap();
        LegacyCardRow {
            path_id: "p".into(),
            quiz_id: card.quiz_id.clone(),
            stability: f64::from(s.stability),
            difficulty: f64::from(s.difficulty),
            due_at: s.due,
            last_reviewed_at: s.last_review,
            reps: card.reps,
            lapses: card.lapses,
        }
    }

    #[test]
    fn a_path_verifies_against_its_stored_state_without_a_server() {
        let (path, events, state) = answered_path();
        let card = legacy_card(&state);
        let check =
            |stored: Option<&PathState>, stored_events: &[Event], legacy: &LegacyCardRow| {
                verify_path(
                    &path,
                    &events,
                    std::slice::from_ref(legacy),
                    stored,
                    stored_events,
                )
                .unwrap()
            };
        assert_eq!(check(Some(&state), &events, &card), Vec::<String>::new());
        assert_eq!(check(None, &[], &card), ["p: path missing from Baserunna"]);
        let lost = check(Some(&state), &[], &card);
        assert!(
            lost[0].contains("0 events stored and 1 in libSQL"),
            "{lost:?}"
        );
        let other_reps = LegacyCardRow {
            reps: 2,
            ..legacy_card(&state)
        };
        let differs = check(Some(&state), &events, &other_reps);
        assert!(differs[0].contains("libSQL has (reps 2"), "{differs:?}");
    }

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
