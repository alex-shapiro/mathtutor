//! Transactional database snapshots and atomic write sets.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};

use chrono::{DateTime, Utc};

use crate::cards::{Card, Projection, Taught};
use crate::db::client::api::{ApiError, ErrorCode, PAGE_MAX};
use crate::db::client::{self, Entry};
use crate::db::{Db, Op, OpResult, Txn, run_txn, ts};
use crate::event_log::{Event, EventKind};
use crate::graph::{Curriculum, Graph};
use crate::overlay::{Overlay, RowChange};
use crate::path::PathRow;
use crate::progress::PathProgress;
use crate::schema::{
    ACTIVITY_PREFIX, CardRow, LessonRow, QuizRow, RemovedQuizRow, TaughtRow, activity_key,
    events_stream,
};
use crate::{Error, Result};

pub(crate) const OVERLAY_VERSION_KEY: &str = "overlay_version";

/// Version string of a database before any overlay writes.
const UNWRITTEN_OVERLAY_VERSION: &str = "";

/// Overlay at a stored version, with the curriculum graph that it amends merged on first use.
#[derive(Debug)]
pub(crate) struct CachedOverlay {
    version: String,
    overlay: Overlay,
    merged: OnceLock<Arc<Graph>>,
}

impl CachedOverlay {
    fn new(version: String, overlay: Overlay) -> Arc<Self> {
        Arc::new(Self {
            version,
            overlay,
            merged: OnceLock::new(),
        })
    }
}

/// Overlay row changes queued by a transaction and the version that they commit under.
pub(crate) struct OverlayWrite {
    version: String,
    changes: Vec<RowChange>,
}

fn new_overlay_version() -> String {
    format!("{:032x}", rand::random::<u128>())
}

/// Content of the overlay cache file, which a write borrows and a read owns.
#[derive(serde::Serialize, serde::Deserialize)]
struct CacheFile<V, O> {
    version: V,
    overlay: O,
}

/// Overlay cache of a database handle.
#[derive(Debug, Default)]
pub(crate) struct OverlayCache {
    memory: Option<Arc<CachedOverlay>>,
    /// The handle has read its cache file or does not need to read it.
    file_read: bool,
}

fn read_cached_overlay(path: &Path) -> Option<Arc<CachedOverlay>> {
    let f: CacheFile<String, Overlay> = serde_json::from_slice(&std::fs::read(path).ok()?).ok()?;
    Some(CachedOverlay::new(f.version, f.overlay))
}

/// Return a temporary sibling of `path` that no other write of the cache uses.
fn cache_temp_path(path: &Path) -> PathBuf {
    let mut name = path.file_name().unwrap_or_default().to_os_string();
    name.push(format!(
        ".{}.{:016x}.tmp",
        std::process::id(),
        rand::random::<u64>()
    ));
    path.with_file_name(name)
}

fn write_cached_overlay(path: &Path, version: &str, overlay: &Overlay) {
    let write = || -> std::io::Result<()> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let tmp = cache_temp_path(path);
        let f = CacheFile { version, overlay };
        std::fs::write(&tmp, serde_json::to_vec(&f)?)?;
        std::fs::rename(tmp, path)
    };
    if let Err(e) = write() {
        tracing::warn!(path = %path.display(), error = %e, "cannot write overlay cache");
    }
}

/// Paths that a [`ViewRequest`] loads.
#[derive(Debug, Clone, PartialEq, Eq)]
enum PathSel {
    None,
    Id(String),
    Recent,
    All,
}

/// Specification of data to load into a [`View`].
#[derive(Debug, Clone)]
pub struct ViewRequest {
    path: PathSel,
    overlay: bool,
}

impl ViewRequest {
    /// Request only the database and import checks.
    pub fn nothing() -> Self {
        Self {
            path: PathSel::None,
            overlay: false,
        }
    }

    /// Request only the curriculum overlay.
    pub fn overlay() -> Self {
        Self {
            overlay: true,
            ..Self::nothing()
        }
    }

    /// Request the curriculum overlay and the state of path `explicit`, or of the most recent path if `explicit` is `None`.
    pub fn path(explicit: Option<&str>) -> Self {
        let path = explicit.map_or(PathSel::Recent, |id| PathSel::Id(id.to_string()));
        Self {
            path,
            ..Self::overlay()
        }
    }

    /// Request the curriculum overlay and the state of every path.
    pub fn all_paths() -> Self {
        Self {
            path: PathSel::All,
            ..Self::overlay()
        }
    }

    /// Remove the curriculum overlay from this request.
    #[must_use]
    pub fn without_overlay(self) -> Self {
        Self {
            overlay: false,
            ..self
        }
    }
}

/// Loaded state for a single learning path.
#[derive(Debug, Clone)]
pub struct PathState {
    pub row: PathRow,
    /// Time of the latest write for the path, or its creation time if the path has no recorded write.
    pub updated_at: DateTime<Utc>,
    pub projection: Projection,
}

impl PathState {
    /// Return the [`PathProgress`] derived from this path state.
    pub fn progress(&self) -> PathProgress {
        self.projection.progress()
    }

    /// Return quiz cards due at or before `now`, ordered from earliest to latest due timestamp.
    pub fn due(&self, now: DateTime<Utc>) -> Vec<(String, DateTime<Utc>)> {
        let mut due: Vec<_> = self
            .projection
            .cards
            .values()
            .filter_map(|c| Some((c.quiz_id.clone(), c.state?.due)))
            .filter(|(_, d)| *d <= now)
            .collect();
        due.sort_by(|a, b| a.1.cmp(&b.1).then_with(|| a.0.cmp(&b.0)));
        due
    }
}

/// Database snapshot read by a transaction or [`Db::snapshot`].
#[derive(Debug)]
pub struct View {
    /// Overlay that the view loaded, or `None` if the request did not want it.
    overlay: Option<Arc<CachedOverlay>>,
    curriculum: Arc<Curriculum>,
    /// State of each loaded path, keyed by path ID.
    paths: HashMap<String, PathState>,
    /// ID of the path that [`ViewRequest::path`] selected.
    selected: Option<String>,
}

impl View {
    fn new(curriculum: Arc<Curriculum>, selected: Option<String>) -> Self {
        Self {
            overlay: None,
            curriculum,
            paths: HashMap::new(),
            selected,
        }
    }

    fn loaded_overlay(&self) -> Result<&Arc<CachedOverlay>> {
        self.overlay.as_ref().ok_or(Error::OverlayNotLoaded)
    }

    /// Return the user overlay, or [`Error::OverlayNotLoaded`] if the view did not load it.
    pub fn overlay(&self) -> Result<&Overlay> {
        Ok(&self.loaded_overlay()?.overlay)
    }

    /// Return the curriculum graph merged with the user overlay, or [`Error::OverlayNotLoaded`] if the view did not load the overlay.
    pub fn graph(&self) -> Result<Arc<Graph>> {
        let loaded = self.loaded_overlay()?;
        if let Some(g) = loaded.merged.get() {
            return Ok(g.clone());
        }
        let g = self.curriculum.merged(&loaded.overlay)?;
        Ok(loaded.merged.get_or_init(|| g).clone())
    }

    /// Return the selected [`PathState`], or [`Error::NoPath`] if not found.
    pub fn path(&self) -> Result<&PathState> {
        self.selected
            .as_ref()
            .and_then(|id| self.paths.get(id))
            .ok_or_else(|| Error::NoPath(self.selected.clone()))
    }

    /// Return every loaded path, ordered by creation time.
    pub fn paths(&self) -> Vec<&PathState> {
        let mut paths: Vec<&PathState> = self.paths.values().collect();
        paths.sort_by(|a, b| (a.row.created_at, &a.row.id).cmp(&(b.row.created_at, &b.row.id)));
        paths
    }
}

/// Pending write operations accumulated during a transaction.
#[derive(Debug, Default)]
pub struct WriteSet {
    /// Writes of `paths` rows.
    ops: Vec<Op>,
    overlay: Vec<RowChange>,
    /// Derived rows that the appended events change, keyed by path ID.
    projections: BTreeMap<String, Projection>,
    events: BTreeMap<String, Vec<Event>>,
    /// IDs of the paths that this write set creates.
    created: BTreeSet<String>,
    /// Activity time of each path that this write set touches, keyed by path ID.
    activity: BTreeMap<String, DateTime<Utc>>,
}

impl WriteSet {
    /// Insert path `p` and append its `path_created` event.
    pub fn create_path(&mut self, view: &View, p: &PathRow) -> Result<()> {
        let created = Event {
            ts: p.created_at,
            path: p.id.clone(),
            kind: EventKind::PathCreated {},
        };
        self.create_path_with_history(view, p, std::slice::from_ref(&created), Utc::now())
    }

    /// Insert path `p` with the events of its `history`, and record `active_at` as the time of its latest activity.
    ///
    /// The path becomes the most recent path, whatever the value of `active_at`.
    pub fn create_path_with_history(
        &mut self,
        view: &View,
        p: &PathRow,
        history: &[Event],
        active_at: DateTime<Utc>,
    ) -> Result<()> {
        self.ops.push(Op::insert(p));
        self.created.insert(p.id.clone());
        for e in history {
            self.append(view, e)?;
        }
        self.activity.insert(p.id.clone(), active_at);
        Ok(())
    }

    /// Replace the stored row of path `p.id` with `p`, and make `p` the most recent path.
    pub fn replace_path(&mut self, p: &PathRow) {
        self.ops.push(Op::upsert(p));
        self.touch_path(&p.id);
    }

    /// Make `path_id` the most recent path.
    pub fn touch_path(&mut self, path_id: &str) {
        self.activity
            .entry(path_id.to_string())
            .or_insert_with(Utc::now);
    }

    /// Record the overlay row change `change`.
    ///
    /// Fails with [`Error::OverlayNotLoaded`] unless `view` loaded the overlay, so that the commit checks the overlay version that the edit is based on.
    pub fn overlay(&mut self, view: &View, change: RowChange) -> Result<()> {
        view.loaded_overlay()?;
        self.overlay.push(change);
        Ok(())
    }

    /// Append `e` to its path event stream and update derived `cards` and `taught` rows.
    ///
    /// Fails with [`Error::PathNotLoaded`] unless `view` holds the path of `e` or this write set creates the path, and with [`Error::PathCreatedOutsideCreation`] for a `path_created` event of a path that this write set does not create.
    pub fn append(&mut self, view: &View, e: &Event) -> Result<()> {
        let base = view.paths.get(&e.path);
        let creates_path = self.created.contains(&e.path);
        if matches!(e.kind, EventKind::PathCreated {}) && !creates_path {
            return Err(Error::PathCreatedOutsideCreation(e.path.clone()));
        }
        if base.is_none() && !creates_path {
            return Err(Error::PathNotLoaded(e.path.clone()));
        }
        self.projections
            .entry(e.path.clone())
            .or_default()
            .apply(e, base.map(|p| &p.projection))?;
        self.touch_path(&e.path);
        self.events
            .entry(e.path.clone())
            .or_default()
            .push(e.clone());
        Ok(())
    }

    /// Return the write operations of this set, and its overlay row changes with the new overlay version if it has changes.
    pub(crate) fn into_ops(self) -> (Vec<Op>, Option<OverlayWrite>) {
        let mut ops = self.ops;
        let overlay_write = (!self.overlay.is_empty()).then(|| OverlayWrite {
            version: new_overlay_version(),
            changes: self.overlay,
        });
        if let Some(write) = &overlay_write {
            ops.extend(write.changes.iter().map(RowChange::op));
            ops.push(Op::put(OVERLAY_VERSION_KEY, write.version.clone()));
        }
        for (path_id, projection) in self.projections {
            ops.extend(
                projection
                    .cards
                    .values()
                    .map(|c| Op::upsert(&CardRow::from(c))),
            );
            ops.extend(
                projection
                    .taught
                    .into_iter()
                    .map(|(atom_id, t)| Op::upsert(&t.into_row(&path_id, atom_id))),
            );
        }
        for (path_id, at) in &self.activity {
            ops.push(Op::put(activity_key(path_id), ts::format(*at)));
        }
        for (path_id, events) in self.events {
            let stream = events_stream(&path_id);
            ops.extend(
                events
                    .chunks(PAGE_MAX)
                    .map(|chunk| Op::append(stream.clone(), chunk)),
            );
        }
        (ops, overlay_write)
    }
}

impl Db {
    /// Load `want` from a strong read snapshot without starting a write transaction.
    pub async fn snapshot(&self, want: &ViewRequest) -> Result<View> {
        let path_id = self.selected_path_id(&want.path).await?;
        let overlay = if want.overlay {
            self.cached_overlay()
                .map_or(OverlayPlan::Read, OverlayPlan::Check)
        } else {
            OverlayPlan::Skip
        };
        self.read_view(want, path_id.as_deref(), overlay).await
    }

    /// Read `want` and get the overlay as `overlay` plans without a transaction, and cache the overlay if the read loaded its rows.
    ///
    /// Reads the overlay rows again if the stored version differs from the cached overlay or changes between pages.
    async fn read_view(
        &self,
        want: &ViewRequest,
        path_id: Option<&str>,
        mut overlay: OverlayPlan,
    ) -> Result<View> {
        for _ in 0..OVERLAY_READ_ATTEMPTS {
            let reads_rows = matches!(overlay, OverlayPlan::Read);
            let mut plan = self.plan(want, path_id, overlay);
            let mut results = self.read(&plan.ops()).await?;
            while let Some(more) = plan.absorb(results)? {
                results = self.read(&more).await?;
            }
            if !plan.overlay_stale {
                if reads_rows {
                    self.cache_overlay(plan.view.overlay.clone());
                }
                return Ok(plan.view);
            }
            overlay = OverlayPlan::Read;
        }
        Err(overlay_conflict())
    }

    /// Execute `f` against `want` in a serializable transaction, retrying on conflict.
    pub async fn transact<T>(
        &self,
        want: &ViewRequest,
        mut f: impl FnMut(&View, &mut WriteSet) -> Result<T>,
    ) -> Result<T> {
        let path_id = self.selected_path_id(&want.path).await?;
        let done = run_txn!(self, |txn| {
            self.attempt(&mut txn, want, path_id.as_deref(), &mut f)
                .await
        })?;
        if let Some(write) = done.overlay_write {
            self.remember_overlay_write(done.overlay, write);
        }
        Ok(done.out)
    }

    /// Read `want` in `txn` and queue the writes of `f`.
    ///
    /// Fails with a conflict that restarts the transaction if the stored overlay differs from the cached overlay, and refreshes the cache for the next attempt.
    async fn attempt<T>(
        &self,
        txn: &mut Txn,
        want: &ViewRequest,
        path_id: Option<&str>,
        f: &mut impl FnMut(&View, &mut WriteSet) -> Result<T>,
    ) -> Result<Attempted<T>> {
        let overlay = if want.overlay {
            Some(self.overlay_or_load().await?)
        } else {
            None
        };
        let check = overlay
            .clone()
            .map_or(OverlayPlan::Skip, OverlayPlan::Check);
        let mut plan = self.plan(want, path_id, check);
        let mut results = txn.run(&plan.ops()).await?;
        while let Some(more) = plan.absorb(results)? {
            results = txn.run(&more).await?;
        }
        if plan.overlay_stale {
            self.load_overlay().await?;
            return Err(overlay_conflict());
        }
        let mut changes = WriteSet::default();
        let out = f(&plan.view, &mut changes)?;
        if let PathSel::Id(explicit_id) = &want.path {
            changes.touch_path(explicit_id);
        }
        let overlay_write = txn.queue_write_set(changes);
        Ok(Attempted {
            out,
            overlay,
            overlay_write,
        })
    }

    fn plan(&self, want: &ViewRequest, path_id: Option<&str>, overlay: OverlayPlan) -> Plan {
        Plan::new(self.curriculum.clone(), want, path_id, overlay)
    }

    /// Return the cached overlay, or load it if the cache is empty.
    async fn overlay_or_load(&self) -> Result<Arc<CachedOverlay>> {
        match self.cached_overlay() {
            Some(cached) => Ok(cached),
            None => self.load_overlay().await,
        }
    }

    /// Read the overlay rows and their version without a transaction and cache them.
    async fn load_overlay(&self) -> Result<Arc<CachedOverlay>> {
        let view = self
            .read_view(&ViewRequest::overlay(), None, OverlayPlan::Read)
            .await?;
        view.loaded_overlay().cloned()
    }

    /// Resolve `selector` to a path ID, choosing the most recent path at most once per command.
    async fn selected_path_id(&self, selector: &PathSel) -> Result<Option<String>> {
        match selector {
            PathSel::None | PathSel::All => Ok(None),
            PathSel::Id(id) => Ok(Some(id.clone())),
            PathSel::Recent => self.most_recent_path_id().await,
        }
    }

    /// Return the ID of the path whose activity key the latest commit wrote.
    ///
    /// The server assigns the version of each commit, so a writer clock that runs ahead cannot keep its path the most recent.
    async fn most_recent_path_id(&self) -> Result<Option<String>> {
        let mut latest: Option<(Option<u64>, String)> = None;
        for page in self.read_pages(Op::scan(ACTIVITY_PREFIX)).await? {
            for e in page.scan_page()?.entries {
                let (id, _) = parse_activity(&e)?;
                if latest
                    .as_ref()
                    .is_none_or(|l| (e.version, id) > (l.0, l.1.as_str()))
                {
                    latest = Some((e.version, id.to_string()));
                }
            }
        }
        Ok(latest.map(|(_, id)| id))
    }

    fn lock_overlay_cache(&self) -> std::sync::MutexGuard<'_, OverlayCache> {
        self.overlay_cache
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Replace the cached overlay with `next`, and write `next` to the cache file if its version is new.
    fn cache_overlay(&self, next: Option<Arc<CachedOverlay>>) {
        let new_version = {
            let mut cache = self.lock_overlay_cache();
            let changed =
                cache.memory.as_ref().map(|c| &c.version) != next.as_ref().map(|n| &n.version);
            let new_version = next.clone().filter(|_| changed);
            cache.memory = next;
            cache.file_read = true;
            new_version
        };
        if let (Some(file), Some(n)) = (&self.cache_file, new_version) {
            write_cached_overlay(file, &n.version, &n.overlay);
        }
    }

    /// Return the cached overlay, and read the cache file on the first call that finds the memory cache empty.
    fn cached_overlay(&self) -> Option<Arc<CachedOverlay>> {
        {
            let mut cache = self.lock_overlay_cache();
            if cache.memory.is_some() || cache.file_read {
                return cache.memory.clone();
            }
            cache.file_read = true;
        }
        let from_file = self.cache_file.as_deref().and_then(read_cached_overlay);
        let mut cache = self.lock_overlay_cache();
        if cache.memory.is_none() {
            cache.memory = from_file;
        }
        cache.memory.clone()
    }

    /// Cache the overlay `base` that a transaction checked with its committed `write` applied, or clear the cache if the transaction did not check an overlay.
    fn remember_overlay_write(&self, base: Option<Arc<CachedOverlay>>, write: OverlayWrite) {
        let next = base.map(|base| {
            let mut overlay = base.overlay.clone();
            for change in write.changes {
                overlay.apply(change);
            }
            CachedOverlay::new(write.version, overlay)
        });
        self.cache_overlay(next);
    }
}

/// Transaction attempt whose writes are queued for the commit.
struct Attempted<T> {
    out: T,
    /// Overlay that the attempt read.
    overlay: Option<Arc<CachedOverlay>>,
    overlay_write: Option<OverlayWrite>,
}

/// Number of times a read without a transaction starts again after the stored overlay version changes.
const OVERLAY_READ_ATTEMPTS: usize = 10;

/// Return the conflict that restarts a transaction or a read whose stored overlay differs from the overlay that it uses.
fn overlay_conflict() -> Error {
    let error = ApiError {
        code: ErrorCode::Conflict,
        message: format!("{OVERLAY_VERSION_KEY} changed while mt read the overlay"),
        retryable: true,
        details: None,
    };
    Error::Baserunna(client::Error::Api {
        status: 409,
        error,
        retry_after_ms: None,
    })
}

/// How a [`Plan`] gets the overlay.
enum OverlayPlan {
    /// The view does not load the overlay.
    Skip,
    /// Use the cached overlay if the stored version matches it.
    Check(Arc<CachedOverlay>),
    /// Read the overlay rows, and read the version again with each page.
    Read,
}

/// Decode the overlay version from the result of a read of [`OVERLAY_VERSION_KEY`].
fn overlay_version(r: OpResult) -> Result<String> {
    Ok(match r.entry()? {
        Some(e) => entry_str(&e)?.1.to_string(),
        None => UNWRITTEN_OVERLAY_VERSION.to_string(),
    })
}

/// Accumulator that receives the result of a planned read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Slot {
    OverlayVersion,
    Lessons,
    Quizzes,
    RemovedQuizzes,
    /// A single `paths` row.
    Path,
    /// A page of `paths` rows.
    Paths,
    Cards,
    Taught,
    /// A single activity key.
    Activity,
    /// A page of activity keys.
    ActivityScan,
}

/// Reads that load a [`View`], accumulated across pages.
struct Plan {
    view: View,
    overlay: OverlayPlan,
    /// Overlay version that the first request read.
    overlay_version: Option<String>,
    /// The stored overlay version differs from the cached overlay, or changed between pages.
    overlay_stale: bool,
    pending: Vec<(Slot, Op)>,
    lessons: Vec<LessonRow>,
    quizzes: Vec<QuizRow>,
    removed_quizzes: Vec<RemovedQuizRow>,
    paths: Vec<PathRow>,
    cards: Vec<CardRow>,
    taught: Vec<TaughtRow>,
    activity: HashMap<String, DateTime<Utc>>,
}

/// Return the planned read of the overlay version.
fn overlay_version_read() -> (Slot, Op) {
    (Slot::OverlayVersion, Op::get(OVERLAY_VERSION_KEY))
}

/// Return the key and value of `e` as UTF-8.
fn entry_str(e: &Entry) -> Result<(&str, &str)> {
    let key = std::str::from_utf8(&e.key)
        .map_err(|_| Error::BadRow(format!("{:?}: key is not UTF-8", e.key)))?;
    let value = e.value.data().and_then(|d| std::str::from_utf8(d).ok());
    Ok((
        key,
        value.ok_or_else(|| Error::BadRow(format!("{key}: not UTF-8")))?,
    ))
}

fn parse_activity(e: &Entry) -> Result<(&str, DateTime<Utc>)> {
    let (key, value) = entry_str(e)?;
    let at = ts::parse(value).map_err(|err| Error::BadRow(format!("{key}: {err}")))?;
    Ok((key.strip_prefix(ACTIVITY_PREFIX).unwrap_or(key), at))
}

impl Plan {
    /// Plan the reads of `want`, and get the overlay as `overlay` plans.
    fn new(
        curriculum: Arc<Curriculum>,
        want: &ViewRequest,
        path_id: Option<&str>,
        overlay: OverlayPlan,
    ) -> Self {
        let mut pending = Vec::new();
        match overlay {
            OverlayPlan::Skip => {}
            OverlayPlan::Check(_) => pending.push(overlay_version_read()),
            OverlayPlan::Read => pending.extend([
                overlay_version_read(),
                (Slot::Lessons, Op::query_all::<LessonRow>()),
                (Slot::Quizzes, Op::query_all::<QuizRow>()),
                (Slot::RemovedQuizzes, Op::query_all::<RemovedQuizRow>()),
            ]),
        }
        if want.path == PathSel::All {
            pending.extend([
                (Slot::Paths, Op::query_all::<PathRow>()),
                (Slot::Cards, Op::query_all::<CardRow>()),
                (Slot::Taught, Op::query_all::<TaughtRow>()),
                (Slot::ActivityScan, Op::scan(ACTIVITY_PREFIX)),
            ]);
        } else if let Some(id) = path_id {
            pending.extend([
                (Slot::Path, Op::get_row::<PathRow>(id)),
                (Slot::Cards, Op::query::<CardRow>(id)),
                (Slot::Taught, Op::query::<TaughtRow>(id)),
                (Slot::Activity, Op::get(activity_key(id))),
            ]);
        }
        Self {
            view: View::new(curriculum, path_id.map(str::to_string)),
            overlay,
            overlay_version: None,
            overlay_stale: false,
            pending,
            lessons: Vec::new(),
            quizzes: Vec::new(),
            removed_quizzes: Vec::new(),
            paths: Vec::new(),
            cards: Vec::new(),
            taught: Vec::new(),
            activity: HashMap::new(),
        }
    }

    fn ops(&self) -> Vec<Op> {
        self.pending.iter().map(|(_, op)| op.clone()).collect()
    }

    /// Accumulate `results` of the pending reads, and return the reads of the next pages.
    ///
    /// Returns `None` when the reads are complete or the overlay is stale.
    fn absorb(&mut self, results: Vec<OpResult>) -> Result<Option<Vec<Op>>> {
        let pending = std::mem::take(&mut self.pending);
        for ((slot, op), r) in pending.into_iter().zip(results) {
            if let Some(next) = op.next_page(&r) {
                self.pending.push((slot, next));
            }
            match slot {
                Slot::OverlayVersion => {
                    let stored = overlay_version(r)?;
                    let cached = match &self.overlay {
                        OverlayPlan::Check(c) => Some(&c.version),
                        _ => self.overlay_version.as_ref(),
                    };
                    if cached.is_some_and(|v| *v != stored) {
                        self.overlay_stale = true;
                        return Ok(None);
                    }
                    self.overlay_version = Some(stored);
                }
                Slot::Lessons => self.lessons.extend(r.rows()?),
                Slot::Quizzes => self.quizzes.extend(r.rows()?),
                Slot::RemovedQuizzes => self.removed_quizzes.extend(r.rows()?),
                Slot::Path => self.paths.extend(r.row()?),
                Slot::Paths => self.paths.extend(r.rows()?),
                Slot::Cards => self.cards.extend(r.rows()?),
                Slot::Taught => self.taught.extend(r.rows()?),
                Slot::Activity => {
                    if let Some(e) = r.entry()? {
                        let (id, at) = parse_activity(&e)?;
                        self.activity.insert(id.to_string(), at);
                    }
                }
                Slot::ActivityScan => {
                    for e in r.scan_page()?.entries {
                        let (id, at) = parse_activity(&e)?;
                        self.activity.insert(id.to_string(), at);
                    }
                }
            }
        }
        if self.pending.is_empty() {
            self.finish()?;
            return Ok(None);
        }
        if matches!(self.overlay, OverlayPlan::Read) {
            self.pending.insert(0, overlay_version_read());
        }
        Ok(Some(self.ops()))
    }

    /// Build the overlay and the path states from the accumulated rows.
    fn finish(&mut self) -> Result<()> {
        use std::mem::take;
        self.view.overlay = match std::mem::replace(&mut self.overlay, OverlayPlan::Skip) {
            OverlayPlan::Skip => None,
            OverlayPlan::Check(cached) => Some(cached),
            OverlayPlan::Read => Some(CachedOverlay::new(
                self.overlay_version.take().unwrap_or_default(),
                Overlay::from_rows(
                    take(&mut self.lessons),
                    take(&mut self.quizzes),
                    take(&mut self.removed_quizzes),
                ),
            )),
        };
        let mut paths: HashMap<String, PathState> = take(&mut self.paths)
            .into_iter()
            .map(|row| {
                let updated_at = self
                    .activity
                    .get(&row.id)
                    .copied()
                    .unwrap_or(row.created_at);
                let state = PathState {
                    updated_at,
                    row,
                    projection: Projection::default(),
                };
                (state.row.id.clone(), state)
            })
            .collect();
        for r in take(&mut self.cards) {
            if let Some(p) = paths.get_mut(&r.path_id) {
                p.projection
                    .cards
                    .insert(r.quiz_id.clone(), Card::try_from(r)?);
            }
        }
        for t in take(&mut self.taught) {
            if let Some(p) = paths.get_mut(&t.path_id) {
                let taught = Taught {
                    count: t.count,
                    last_at: t.last_at,
                };
                p.projection.taught.insert(t.atom_id, taught);
            }
        }
        self.view.paths = paths;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cache_writes_use_distinct_temporary_files_beside_the_cache() {
        let cache = Path::new("/tmp/mt/overlay-host-db.json");
        let (a, b) = (cache_temp_path(cache), cache_temp_path(cache));
        assert_ne!(a, b);
        assert_eq!(a.parent(), cache.parent());
        let name = a.file_name().unwrap().to_string_lossy().into_owned();
        assert!(name.starts_with("overlay-host-db.json."), "{name}");
    }
}
