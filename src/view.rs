//! A command's transaction: everything it reads, loaded in the first
//! request, and everything it writes, sent with the commit.
//!
//! [`Db::transact`] runs a read-modify-write command as one serializable
//! transaction in two round trips (three when the most recent path is not
//! the one the previous transaction used). Commands are plain functions of
//! a [`View`] that record their writes in [`Changes`], so a conflicting
//! concurrent write simply reruns them.

use std::collections::{BTreeMap, HashMap};
use std::path::Path;
use std::sync::{Arc, Mutex, Weak};

use chrono::{DateTime, Utc};
use serde_json::{Value as Json, json};

use crate::cards::{self, CardRow};
use crate::db::{self, Db, Txn};
use crate::event_log::{self, Event, EventKind};
use crate::graph::Graph;
use crate::overlay::{self, Edit, Overlay};
use crate::path::{self, PathFile};
use crate::progress::PathProgress;
use crate::schema::events_stream;
use crate::{Error, Result};

/// How long a transaction is rerun after conflicts and transient failures.
const RETRY_FOR: std::time::Duration = std::time::Duration::from_secs(30);

/// Rows a query reads per page; the server's maximum.
const PAGE: usize = 1000;

/// Key whose value changes with every overlay write, so a cached overlay
/// can be validated by reading it alone.
const OVERLAY_VERSION_KEY: &str = "overlay_version";

/// Prefix of the keys holding each path's latest activity time. They are
/// written blind, so commands logging events on one path never conflict
/// over them.
const ACTIVITY_PREFIX: &str = "activity/";

fn activity_key(path_id: &str) -> String {
    format!("{ACTIVITY_PREFIX}{path_id}")
}

/// An overlay as of an [`OVERLAY_VERSION_KEY`] value.
pub(crate) type CachedOverlay = (u64, Arc<Overlay>);

#[derive(serde::Serialize, serde::Deserialize)]
struct CacheFile {
    version: u64,
    overlay: Overlay,
}

/// The overlay cached in `path`, if it holds a readable one.
pub(crate) fn read_cached_overlay(path: &Path) -> Option<CachedOverlay> {
    let f: CacheFile = serde_json::from_slice(&std::fs::read(path).ok()?).ok()?;
    Some((f.version, Arc::new(f.overlay)))
}

/// Replace the overlay cached in `path`. Failures only cost a download later.
fn write_cached_overlay(path: &Path, version: u64, overlay: &Overlay) {
    let write = || -> std::io::Result<()> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let tmp = path.with_extension("tmp");
        let f = CacheFile {
            version,
            overlay: overlay.clone(),
        };
        std::fs::write(&tmp, serde_json::to_vec(&f)?)?;
        std::fs::rename(tmp, path)
    };
    if let Err(e) = write() {
        tracing::warn!(path = %path.display(), error = %e, "cannot write overlay cache");
    }
}

/// Which path a command acts on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PathSel {
    /// No path state.
    None,
    /// The named path; [`View::path`] fails with `NoPath` if it does not exist.
    Id(String),
    /// The path that most recently logged an event.
    Recent,
}

impl PathSel {
    /// `Id` when the caller named a path, else `Recent`.
    pub fn from(explicit: Option<&str>) -> Self {
        explicit.map_or(Self::Recent, |id| Self::Id(id.to_string()))
    }
}

/// What a command reads.
#[derive(Debug, Clone)]
pub struct Want {
    pub path: PathSel,
    /// The user's authored lessons and quizzes.
    pub overlay: bool,
    /// Every path, with its cards and taught lessons.
    pub all_paths: bool,
}

impl Want {
    /// The overlay and the state of the selected path.
    pub fn path(explicit: Option<&str>) -> Self {
        Self {
            path: PathSel::from(explicit),
            overlay: true,
            all_paths: false,
        }
    }

    /// The overlay alone.
    pub fn overlay() -> Self {
        Self {
            path: PathSel::None,
            overlay: true,
            all_paths: false,
        }
    }
}

/// One path's stored state.
#[derive(Debug, Clone)]
pub struct PathState {
    pub file: PathFile,
    /// Cards by quiz id.
    pub cards: HashMap<String, CardRow>,
    /// Lessons taught on this path, by atom id.
    pub taught: HashMap<String, Taught>,
}

/// A lesson taught on a path: by authoring it, or by presenting it `count` times.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Taught {
    pub count: u32,
    pub last_at: Option<DateTime<Utc>>,
}

impl PathState {
    pub fn progress(&self) -> PathProgress {
        PathProgress {
            taught_atoms: self.taught.keys().cloned().collect(),
            correct_quizzes: self
                .cards
                .values()
                .filter(|c| c.reps > c.lapses)
                .map(|c| c.quiz_id.clone())
                .collect(),
        }
    }

    /// Quizzes due at `now`, oldest-due first.
    pub fn due(&self, now: DateTime<Utc>) -> Vec<(String, DateTime<Utc>)> {
        let mut due: Vec<_> = self
            .cards
            .values()
            .filter_map(|c| Some((c.quiz_id.clone(), c.state?.due)))
            .filter(|(_, d)| *d <= now)
            .collect();
        due.sort_by(|a, b| a.1.cmp(&b.1).then_with(|| a.0.cmp(&b.0)));
        due
    }
}

/// The database as a transaction read it.
#[derive(Debug, Default)]
pub struct View {
    pub overlay: Arc<Overlay>,
    path: Option<PathState>,
    /// Every path, oldest first, when [`Want::all_paths`] was set.
    pub paths: Vec<PathState>,
}

impl View {
    /// The shipped curriculum merged with this view's overlay. Merges with
    /// the embedded curriculum are cached per overlay, so commands that
    /// read an unchanged overlay share one.
    pub fn graph(&self, graph_dir: Option<&Path>) -> Result<Arc<Graph>> {
        static MERGED: Mutex<Vec<(Weak<Overlay>, Arc<Graph>)>> = Mutex::new(Vec::new());
        const KEEP: usize = 64;
        if graph_dir.is_some() || std::env::var_os("MT_GRAPH").is_some() {
            return Ok(Arc::new(Graph::merged(&self.overlay, graph_dir)?));
        }
        let hit = |m: &[(Weak<Overlay>, Arc<Graph>)]| {
            m.iter()
                .find(|(o, _)| o.upgrade().is_some_and(|o| Arc::ptr_eq(&o, &self.overlay)))
                .map(|(_, g)| g.clone())
        };
        if let Some(g) = hit(&MERGED
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner))
        {
            return Ok(g);
        }
        let g = Arc::new(Graph::merged(&self.overlay, None)?);
        let mut m = MERGED
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        m.retain(|(o, _)| o.strong_count() > 0);
        if m.len() >= KEEP {
            m.remove(0);
        }
        m.push((Arc::downgrade(&self.overlay), g.clone()));
        Ok(g)
    }

    /// The selected path, or `NoPath` if there is none.
    pub fn path(&self) -> Result<&PathState> {
        self.path.as_ref().ok_or(Error::NoPath)
    }
}

/// Writes a command makes, applied atomically at commit.
#[derive(Debug, Default)]
pub struct Changes {
    ops: Vec<Json>,
    overlay: Vec<Edit>,
    cards: BTreeMap<(String, String), CardRow>,
    taught: BTreeMap<(String, String), Taught>,
    events: BTreeMap<String, Vec<Json>>,
}

impl Changes {
    pub fn is_empty(&self) -> bool {
        self.ops.is_empty() && self.overlay.is_empty() && self.events.is_empty()
    }

    /// Add a raw write op.
    pub fn op(&mut self, op: Json) {
        self.ops.push(op);
    }

    /// Change the overlay.
    pub fn overlay(&mut self, e: Edit) {
        self.overlay.push(e);
    }

    /// Append `e` to its path's event log and fold it into the path's
    /// derived `cards` and `taught` rows, starting from their state in `view`.
    pub fn append(&mut self, view: &View, e: &Event) -> Result<()> {
        let base = view
            .path
            .as_ref()
            .filter(|p| p.file.id == e.path)
            .or_else(|| view.paths.iter().find(|p| p.file.id == e.path));
        match e.kind {
            EventKind::LessonTaught | EventKind::LessonAuthored => {
                let atom = e
                    .atom
                    .clone()
                    .ok_or_else(|| Error::BadEvent("lesson event without atom"))?;
                let key = (e.path.clone(), atom.clone());
                let t = self.taught.entry(key).or_insert_with(|| {
                    base.and_then(|p| p.taught.get(&atom).cloned())
                        .unwrap_or_default()
                });
                if e.kind == EventKind::LessonTaught {
                    t.count += 1;
                    t.last_at = Some(e.ts);
                }
            }
            EventKind::QuizPresented | EventKind::QuizAnswered => {
                let quiz = e
                    .quiz
                    .clone()
                    .ok_or_else(|| Error::BadEvent("quiz event without quiz"))?;
                let key = (e.path.clone(), quiz.clone());
                let c = self.cards.entry(key).or_insert_with(|| {
                    base.and_then(|p| p.cards.get(&quiz).cloned())
                        .unwrap_or_else(|| CardRow::new(&e.path, &quiz))
                });
                cards::apply_event(c, e)?;
            }
            _ => {}
        }
        self.events
            .entry(e.path.clone())
            .or_default()
            .push(event_log::encode(e));
        Ok(())
    }

    /// The commit's ops: raw writes, then derived rows, then event appends
    /// and the touched paths' activity times.
    fn into_ops(self) -> Vec<Json> {
        let mut ops = self.ops;
        if !self.overlay.is_empty() {
            ops.extend(self.overlay.iter().flat_map(Edit::ops));
            ops.push(json!({ "op": "increment", "key": OVERLAY_VERSION_KEY }));
        }
        ops.extend(self.cards.into_values().map(|c| cards::upsert_op(&c)));
        ops.extend(self.taught.into_iter().map(|((path_id, atom_id), t)| {
            json!({ "op": "upsert_row", "table": "taught", "row": {
                "path_id": path_id, "atom_id": atom_id, "count": t.count,
                "last_at": t.last_at.map(db::format_ts),
            }})
        }));
        for (path_id, records) in self.events {
            ops.push(json!({ "op": "put", "key": activity_key(&path_id), "value": db::format_ts(Utc::now()) }));
            let records: Vec<Json> = records.into_iter().map(|r| json!({ "json": r })).collect();
            ops.push(json!({ "op": "append", "stream": events_stream(&path_id),
                "records": records, "create": true }));
        }
        ops
    }
}

impl Db {
    /// Read `want` from a strong snapshot without a transaction.
    pub async fn snapshot(&self, want: &Want) -> Result<View> {
        let mut plan = Plan::new(want, self.hint(), self.cached_overlay());
        let mut results = self.read(&plan.ops()).await?;
        while let Some(more) = plan.absorb(results)? {
            results = self.read(&more).await?;
        }
        self.remember(&plan);
        Ok(plan.view)
    }

    /// Run `f` on `want` and commit its changes as one serializable
    /// transaction, rerunning it when a concurrent write conflicts.
    pub async fn transact<T>(
        &self,
        want: &Want,
        mut f: impl FnMut(&View, &mut Changes) -> Result<T>,
    ) -> Result<T> {
        let started = std::time::Instant::now();
        let mut attempt = 0;
        loop {
            match self.attempt(want, &mut f).await {
                Err(e) if db::is_retryable(&e) && started.elapsed() < RETRY_FOR => {
                    attempt += 1;
                    tokio::time::sleep(db::backoff(attempt)).await;
                }
                r => return r,
            }
        }
    }

    async fn attempt<T>(
        &self,
        want: &Want,
        f: &mut impl FnMut(&View, &mut Changes) -> Result<T>,
    ) -> Result<T> {
        let mut plan = Plan::new(want, self.hint(), self.cached_overlay());
        let (txn, mut results) = self.begin(&plan.ops()).await?;
        loop {
            match plan.absorb(results) {
                Ok(Some(more)) => results = txn.ops(&more).await?,
                Ok(None) => break,
                Err(e) => return abort(txn, e).await,
            }
        }
        let mut changes = Changes::default();
        let out = match f(&plan.view, &mut changes) {
            Ok(out) => out,
            Err(e) => return abort(txn, e).await,
        };
        let edits = changes.overlay.clone();
        txn.commit(&changes.into_ops()).await?;
        self.remember(&plan);
        if !edits.is_empty() {
            self.remember_edits(&plan, &edits);
        }
        Ok(out)
    }

    fn hint(&self) -> Option<String> {
        self.path_hint
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    /// Cache `next`, and keep it on disk when it is a version not stored yet.
    fn cache_overlay(&self, next: Option<CachedOverlay>) {
        let mut cache = self
            .overlay_cache
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let changed = cache.as_ref().map(|c| c.0) != next.as_ref().map(|n| n.0);
        if changed && let (Some(file), Some((v, o))) = (&self.cache_file, &next) {
            write_cached_overlay(file, *v, o);
        }
        *cache = next;
    }

    fn cached_overlay(&self) -> Option<CachedOverlay> {
        self.overlay_cache
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    /// Cache the overlay as this transaction's commit left it. The commit
    /// validated the overlay version it read, so it moved that version on
    /// by exactly one.
    fn remember_edits(&self, plan: &Plan, edits: &[Edit]) {
        let next = plan.overlay_version.map(|v| {
            let mut o = (*plan.view.overlay).clone();
            for e in edits {
                o.apply(e);
            }
            (v + 1, Arc::new(o))
        });
        self.cache_overlay(next);
    }

    fn remember(&self, plan: &Plan) {
        if let Some(p) = &plan.view.path {
            *self
                .path_hint
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(p.file.id.clone());
        }
        if let Some(v) = plan.overlay_version {
            self.cache_overlay(Some((v, plan.view.overlay.clone())));
        }
    }
}

async fn abort<T>(txn: Txn, e: Error) -> Result<T> {
    txn.abort().await;
    Err(e)
}

/// The selected path's row, as far as it has been read.
#[derive(Debug)]
enum PathRow {
    Unread,
    Missing,
    Found(PathFile),
}

/// Which query a result belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Slot {
    OverlayVersion,
    Lessons,
    Quizzes,
    Removed,
    Paths,
    AllCards,
    AllTaught,
    /// Every path's activity time.
    Activity,
    /// The selected path's activity time.
    PathActivity,
    PathRow,
    Cards,
    Taught,
}

/// The reads a [`Want`] takes, issued in rounds: the first round reads
/// everything it can, later rounds read the most recent path when the hint
/// was wrong and fetch further pages of long queries.
struct Plan {
    view: View,
    /// The cached overlay, until the stored version is known.
    cache: Option<CachedOverlay>,
    /// The overlay version the view's overlay was read at.
    overlay_version: Option<u64>,
    overlay_cached: bool,
    pending: Vec<(Slot, Json)>,
    /// Path whose cards and taught rows the current round reads.
    path_id: Option<String>,
    /// Whether the path to read is the most recently active one, still to
    /// be decided once every activity key is read.
    recent: bool,
    activity: HashMap<String, DateTime<Utc>>,
    /// Path state as it accumulates.
    path_row: PathRow,
    cards: Vec<Json>,
    taught: Vec<Json>,
    lessons: Vec<Json>,
    quizzes: Vec<Json>,
    removed: Vec<Json>,
    paths: Vec<Json>,
    all_cards: Vec<Json>,
    all_taught: Vec<Json>,
}

fn query(table: &str, eq: &[&str]) -> Json {
    json!({ "op": "query_rows", "table": table, "eq": eq, "limit": PAGE })
}

fn path_reads(id: &str) -> [(Slot, Json); 3] {
    [
        (
            Slot::PathRow,
            json!({ "op": "get_row", "table": "paths", "pk": [id] }),
        ),
        (Slot::Cards, query("cards", &[id])),
        (Slot::Taught, query("taught", &[id])),
    ]
}

fn activity_scan() -> (Slot, Json) {
    (
        Slot::Activity,
        json!({ "op": "scan", "prefix": ACTIVITY_PREFIX, "limit": PAGE }),
    )
}

fn overlay_reads() -> [(Slot, Json); 3] {
    [
        (Slot::Lessons, query("lessons", &[])),
        (Slot::Quizzes, query("quizzes", &[])),
        (Slot::Removed, query("removed_quizzes", &[])),
    ]
}

impl Plan {
    fn new(want: &Want, hint: Option<String>, cache: Option<CachedOverlay>) -> Self {
        let mut pending = Vec::new();
        if want.overlay {
            pending.push((
                Slot::OverlayVersion,
                json!({ "op": "get", "key": OVERLAY_VERSION_KEY }),
            ));
            // Without a cached overlay, read it speculatively at once.
            if cache.is_none() {
                pending.extend(overlay_reads());
            }
        }
        let recent = want.path == PathSel::Recent;
        if want.all_paths {
            pending.push((Slot::Paths, query("paths", &[])));
            pending.push((Slot::AllCards, query("cards", &[])));
            pending.push((Slot::AllTaught, query("taught", &[])));
        }
        if want.all_paths || recent {
            pending.push(activity_scan());
        }
        let path_id = match &want.path {
            PathSel::None => None,
            PathSel::Id(id) => {
                pending.push((
                    Slot::PathActivity,
                    json!({ "op": "get", "key": activity_key(id) }),
                ));
                Some(id.clone())
            }
            // Read the hinted path speculatively; the activity keys decide.
            PathSel::Recent => hint,
        };
        if let Some(id) = &path_id {
            pending.extend(path_reads(id));
        }
        Self {
            view: View::default(),
            cache,
            overlay_version: None,
            overlay_cached: false,
            pending,
            path_id,
            recent,
            activity: HashMap::new(),
            path_row: PathRow::Unread,
            cards: Vec::new(),
            taught: Vec::new(),
            lessons: Vec::new(),
            quizzes: Vec::new(),
            removed: Vec::new(),
            paths: Vec::new(),
            all_cards: Vec::new(),
            all_taught: Vec::new(),
        }
    }

    fn ops(&self) -> Vec<Json> {
        self.pending.iter().map(|(_, op)| op.clone()).collect()
    }

    /// Fold one round's results in. Returns the next round's ops, or `None`
    /// once the view is complete.
    fn absorb(&mut self, results: Vec<Json>) -> Result<Option<Vec<Json>>> {
        let pending = std::mem::take(&mut self.pending);
        if results.len() != pending.len() {
            return Err(Error::BadResponse(format!(
                "{} results for {} ops",
                results.len(),
                pending.len()
            )));
        }
        for ((slot, op), mut r) in pending.into_iter().zip(results) {
            if let Some(cursor) = r["next_cursor"].as_str() {
                let mut next = op.clone();
                next["cursor"] = json!(cursor);
                self.pending.push((slot, next));
            }
            let rows = match r["rows"].take() {
                Json::Array(rows) => rows,
                _ => Vec::new(),
            };
            match slot {
                Slot::OverlayVersion => {
                    let v = r["value"]
                        .as_str()
                        .map_or(Ok(0), str::parse)
                        .map_err(|_| Error::BadRow(format!("{OVERLAY_VERSION_KEY}: {r}")))?;
                    self.overlay_version = Some(v);
                    match self.cache.take() {
                        Some((cached, overlay)) if cached == v => {
                            self.view.overlay = overlay;
                            self.overlay_cached = true;
                        }
                        Some(_) => self.pending.extend(overlay_reads()),
                        None => {}
                    }
                }
                Slot::Lessons => self.lessons.extend(rows),
                Slot::Quizzes => self.quizzes.extend(rows),
                Slot::Removed => self.removed.extend(rows),
                Slot::Paths => self.paths.extend(rows),
                Slot::AllCards => self.all_cards.extend(rows),
                Slot::AllTaught => self.all_taught.extend(rows),
                Slot::Cards => self.cards.extend(rows),
                Slot::Taught => self.taught.extend(rows),
                Slot::PathRow => {
                    self.path_row = if r["found"] == true {
                        PathRow::Found(path::decode(&r["row"])?)
                    } else {
                        PathRow::Missing
                    };
                }
                Slot::Activity => {
                    for item in r["items"].as_array().into_iter().flatten() {
                        let (Some(key), Some(at)) = (item["key"].as_str(), item["value"].as_str())
                        else {
                            return Err(Error::BadRow(format!("activity: {item}")));
                        };
                        let id = key.strip_prefix(ACTIVITY_PREFIX).unwrap_or(key);
                        self.activity.insert(id.to_string(), db::parse_ts(at)?);
                    }
                }
                Slot::PathActivity => {
                    if let (Some(id), Some(at)) = (&self.path_id, r["value"].as_str()) {
                        self.activity.insert(id.clone(), db::parse_ts(at)?);
                    }
                }
            }
        }
        if self.recent && !self.pending.iter().any(|(s, _)| *s == Slot::Activity) {
            self.recent = false;
            let latest = self
                .activity
                .iter()
                .max_by(|a, b| a.1.cmp(b.1).then_with(|| a.0.cmp(b.0)));
            let latest = latest.map(|(id, _)| id.clone());
            if latest != self.path_id {
                // The hint named another path: drop what was read for it.
                self.path_row = PathRow::Unread;
                self.cards.clear();
                self.taught.clear();
                self.pending
                    .retain(|(s, _)| !matches!(s, Slot::PathRow | Slot::Cards | Slot::Taught));
                if let Some(id) = &latest {
                    self.pending.extend(path_reads(id));
                }
                self.path_id = latest;
            }
        }
        if !self.pending.is_empty() {
            return Ok(Some(self.ops()));
        }
        self.finish()?;
        Ok(None)
    }

    fn finish(&mut self) -> Result<()> {
        if !self.overlay_cached {
            self.view.overlay = Arc::new(overlay::decode(
                &std::mem::take(&mut self.lessons),
                &std::mem::take(&mut self.quizzes),
                &std::mem::take(&mut self.removed),
            )?);
        }
        if let PathRow::Found(mut file) = std::mem::replace(&mut self.path_row, PathRow::Unread) {
            if let Some(at) = self.activity.get(&file.id) {
                file.updated_at = *at;
            }
            self.view.path = Some(PathState {
                file,
                cards: decode_cards(&std::mem::take(&mut self.cards))?,
                taught: decode_taught(&std::mem::take(&mut self.taught))?,
            });
        }
        if !self.paths.is_empty() {
            let mut cards: HashMap<String, HashMap<String, CardRow>> = HashMap::new();
            for c in decode_card_rows(&std::mem::take(&mut self.all_cards))? {
                cards
                    .entry(c.path_id.clone())
                    .or_default()
                    .insert(c.quiz_id.clone(), c);
            }
            let mut taught: HashMap<String, HashMap<String, Taught>> = HashMap::new();
            for (path_id, atom_id, t) in decode_taught_rows(&std::mem::take(&mut self.all_taught))?
            {
                taught.entry(path_id).or_default().insert(atom_id, t);
            }
            let mut paths = std::mem::take(&mut self.paths)
                .iter()
                .map(path::decode)
                .collect::<Result<Vec<_>>>()?;
            for p in &mut paths {
                if let Some(at) = self.activity.get(&p.id) {
                    p.updated_at = *at;
                }
            }
            paths.sort_by(|a, b| {
                a.created_at
                    .cmp(&b.created_at)
                    .then_with(|| a.id.cmp(&b.id))
            });
            self.view.paths = paths
                .into_iter()
                .map(|file| PathState {
                    cards: cards.remove(&file.id).unwrap_or_default(),
                    taught: taught.remove(&file.id).unwrap_or_default(),
                    file,
                })
                .collect();
        }
        Ok(())
    }
}

fn decode_card_rows(rows: &[Json]) -> Result<Vec<CardRow>> {
    rows.iter().map(cards::decode).collect()
}

fn decode_cards(rows: &[Json]) -> Result<HashMap<String, CardRow>> {
    Ok(decode_card_rows(rows)?
        .into_iter()
        .map(|c| (c.quiz_id.clone(), c))
        .collect())
}

fn decode_taught_rows(rows: &[Json]) -> Result<Vec<(String, String, Taught)>> {
    rows.iter()
        .map(|r| {
            let s = |k: &str| {
                r[k].as_str()
                    .map(str::to_string)
                    .ok_or_else(|| Error::BadRow(format!("taught.{k}: {r}")))
            };
            let count = r["count"].as_u64().and_then(|c| u32::try_from(c).ok());
            Ok((
                s("path_id")?,
                s("atom_id")?,
                Taught {
                    count: count.ok_or_else(|| Error::BadRow(format!("taught.count: {r}")))?,
                    last_at: r["last_at"].as_str().map(db::parse_ts).transpose()?,
                },
            ))
        })
        .collect()
}

fn decode_taught(rows: &[Json]) -> Result<HashMap<String, Taught>> {
    Ok(decode_taught_rows(rows)?
        .into_iter()
        .map(|(_, a, t)| (a, t))
        .collect())
}
