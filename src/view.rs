//! Transactional database snapshots and atomic write sets.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};

use chrono::{DateTime, Utc};

use crate::cards::{CardRow, Projection, Taught};
use crate::db::client::Entry;
use crate::db::{Db, Op, OpResult, Txn, run_txn, ts};
use crate::event_log::{Event, EventKind};
use crate::graph::{Curriculum, Graph};
use crate::overlay::{Edit, Overlay};
use crate::path::PathFile;
use crate::progress::PathProgress;
use crate::schema::{
    ACTIVITY_PREFIX, CardRecord, LessonRow, QuizRow, RemovedQuizRow, TaughtRow, activity_key,
    events_stream,
};
use crate::{Error, Result};

pub(crate) const OVERLAY_VERSION_KEY: &str = "overlay_version";

/// Version string of a database before any overlay writes.
const UNWRITTEN_OVERLAY_VERSION: &str = "";

/// Overlay at a stored version, with the curriculum graph that it amends merged on first use.
#[derive(Debug, Clone)]
pub(crate) struct CachedOverlay {
    version: String,
    overlay: Arc<Overlay>,
    merged: Arc<OnceLock<Arc<Graph>>>,
}

impl CachedOverlay {
    fn new(version: String, overlay: Arc<Overlay>) -> Self {
        Self {
            version,
            overlay,
            merged: Arc::default(),
        }
    }
}

/// Overlay edits queued by a transaction and the version that they commit under.
struct OverlayWrite {
    version: String,
    edits: Vec<Edit>,
}

fn new_overlay_version() -> String {
    format!("{:032x}", rand::random::<u128>())
}

#[derive(serde::Serialize, serde::Deserialize)]
struct CacheFile {
    version: String,
    overlay: Overlay,
}

pub(crate) fn read_cached_overlay(path: &Path) -> Option<CachedOverlay> {
    let f: CacheFile = serde_json::from_slice(&std::fs::read(path).ok()?).ok()?;
    Some(CachedOverlay::new(f.version, Arc::new(f.overlay)))
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
        let f = CacheFile {
            version: version.to_string(),
            overlay: overlay.clone(),
        };
        std::fs::write(&tmp, serde_json::to_vec(&f)?)?;
        std::fs::rename(tmp, path)
    };
    if let Err(e) = write() {
        tracing::warn!(path = %path.display(), error = %e, "cannot write overlay cache");
    }
}

/// Target path selector for a [`Want`] query.
#[derive(Debug, Clone, PartialEq, Eq)]
enum PathSel {
    None,
    Id(String),
    Recent,
}

/// Specification of data to load into a [`View`].
#[derive(Debug, Clone)]
pub struct Want {
    path: PathSel,
    overlay: bool,
    all_paths: bool,
}

impl Want {
    /// Request only the database and import checks.
    pub fn nothing() -> Self {
        Self {
            path: PathSel::None,
            overlay: false,
            all_paths: false,
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
            all_paths: true,
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
    pub file: PathFile,
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
    pub overlay: Arc<Overlay>,
    /// Curriculum graph merged with `overlay`, or `None` if the view did not load the overlay.
    merged: Option<Arc<OnceLock<Arc<Graph>>>>,
    curriculum: Arc<Curriculum>,
    /// State of each loaded path, keyed by path ID.
    paths: HashMap<String, PathState>,
    /// ID of the path that [`Want::path`] selected.
    selected: Option<String>,
}

impl View {
    fn new(curriculum: Arc<Curriculum>, selected: Option<String>) -> Self {
        Self {
            overlay: Arc::default(),
            merged: None,
            curriculum,
            paths: HashMap::new(),
            selected,
        }
    }

    fn set_overlay(&mut self, cached: CachedOverlay) {
        self.overlay = cached.overlay;
        self.merged = Some(cached.merged);
    }

    /// Return the curriculum graph merged with this view's overlay, or the curriculum graph alone if the view did not load the overlay.
    pub fn graph(&self) -> Result<Arc<Graph>> {
        let Some(merged) = &self.merged else {
            return self.curriculum.graph();
        };
        if let Some(g) = merged.get() {
            return Ok(g.clone());
        }
        let g = self.curriculum.merged(&self.overlay)?;
        Ok(merged.get_or_init(|| g).clone())
    }

    /// Return the selected [`PathState`], or [`Error::NoPath`] if not found.
    pub fn path(&self) -> Result<&PathState> {
        self.selected
            .as_ref()
            .and_then(|id| self.paths.get(id))
            .ok_or(Error::NoPath)
    }

    /// Return every loaded path, ordered by creation time.
    pub fn paths(&self) -> Vec<&PathState> {
        let mut paths: Vec<&PathState> = self.paths.values().collect();
        paths.sort_by(|a, b| (a.file.created_at, &a.file.id).cmp(&(b.file.created_at, &b.file.id)));
        paths
    }
}

/// Pending write operations accumulated during a transaction.
#[derive(Debug, Default)]
pub struct Changes {
    /// Writes of `paths` rows.
    ops: Vec<Op>,
    overlay: Vec<Edit>,
    /// Derived rows that the appended events change, keyed by path ID.
    projections: BTreeMap<String, Projection>,
    events: BTreeMap<String, Vec<Event>>,
    active_paths: BTreeSet<String>,
}

impl Changes {
    /// Insert path `p` and append its `path_created` event.
    pub fn create_path(&mut self, view: &View, p: &PathFile) -> Result<()> {
        self.ops.push(Op::insert(p));
        let created = Event {
            ts: p.created_at,
            path: p.id.clone(),
            kind: EventKind::PathCreated {},
        };
        self.append(view, &created)
    }

    /// Replace the stored row of path `p.id` with `p`.
    pub fn replace_path(&mut self, p: &PathFile) {
        self.ops.push(Op::upsert(p));
    }

    /// Make `path_id` the most recent path.
    pub fn touch_path(&mut self, path_id: &str) {
        self.active_paths.insert(path_id.to_string());
    }

    /// Record an overlay [`Edit`].
    pub fn overlay(&mut self, e: Edit) {
        self.overlay.push(e);
    }

    /// Append `e` to its path event stream and update derived `cards` and `taught` rows.
    ///
    /// Fails with [`Error::PathNotLoaded`] unless `view` holds the path of `e` or this write set creates the path.
    pub fn append(&mut self, view: &View, e: &Event) -> Result<()> {
        let base = view.paths.get(&e.path);
        let creates = matches!(e.kind, EventKind::PathCreated {});
        if base.is_none() && !creates && !self.creates_path(&e.path) {
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

    /// Return `true` if this write set appends the `path_created` event of `path_id`.
    fn creates_path(&self, path_id: &str) -> bool {
        self.events.get(path_id).is_some_and(|events| {
            events
                .iter()
                .any(|e| matches!(e.kind, EventKind::PathCreated {}))
        })
    }

    fn into_ops(self, overlay_version: Option<&str>) -> Vec<Op> {
        let mut ops = self.ops;
        if let Some(version) = overlay_version {
            ops.extend(self.overlay.iter().flat_map(Edit::ops));
            ops.push(Op::put(OVERLAY_VERSION_KEY, version.to_string()));
        }
        for (path_id, projection) in self.projections {
            ops.extend(
                projection
                    .cards
                    .values()
                    .map(|c| Op::upsert(&CardRecord::from(c))),
            );
            ops.extend(
                projection
                    .taught
                    .into_iter()
                    .map(|(atom_id, t)| Op::upsert(&t.into_row(&path_id, atom_id))),
            );
        }
        for path_id in &self.active_paths {
            ops.push(Op::put(activity_key(path_id), ts::format(Utc::now())));
        }
        for (path_id, events) in self.events {
            ops.push(Op::append(events_stream(&path_id), &events));
        }
        ops
    }
}

impl Db {
    /// Load `want` from a strong read snapshot without starting a write transaction.
    pub async fn snapshot(&self, want: &Want) -> Result<View> {
        let path_id = self.selected_path_id(&want.path).await?;
        let cached = self.cached_overlay().filter(|_| want.overlay);
        if want.overlay && cached.is_none() {
            let plan = self.plan(want, path_id.as_deref(), None);
            let (overlay, plan) = tokio::try_join!(self.load_overlay(), self.read_plan(plan))?;
            let mut view = plan.view;
            view.set_overlay(overlay);
            return Ok(view);
        }
        let plan = self
            .read_plan(self.plan(want, path_id.as_deref(), cached))
            .await?;
        let stale = plan.overlay_stale;
        let mut view = plan.view;
        if stale {
            view.set_overlay(self.load_overlay().await?);
        }
        Ok(view)
    }

    /// Read every batch of `plan` without a transaction.
    async fn read_plan(&self, mut plan: Plan) -> Result<Plan> {
        let ops = plan.ops();
        let mut results = if ops.is_empty() {
            Vec::new()
        } else {
            self.read(&ops).await?
        };
        while let Some(more) = plan.absorb(results)? {
            results = self.read(&more).await?;
        }
        Ok(plan)
    }

    /// Execute `f` against `want` in a serializable transaction, retrying on conflict.
    pub async fn transact<T>(
        &self,
        want: &Want,
        mut f: impl FnMut(&View, &mut Changes) -> Result<T>,
    ) -> Result<T> {
        let path_id = self.selected_path_id(&want.path).await?;
        let mut overlay = if want.overlay {
            Some(self.overlay_or_load().await?)
        } else {
            None
        };
        let (out, overlay_write) = loop {
            let attempt = run_txn!(self, |txn| {
                self.attempt(&mut txn, want, path_id.as_deref(), overlay.clone(), &mut f)
                    .await
            })?;
            match attempt {
                Attempt::Ready(ready) => break ready,
                Attempt::OverlayChanged => overlay = Some(self.load_overlay().await?),
            }
        };
        if let Some(write) = overlay_write {
            self.remember_edits(overlay, write);
        }
        Ok(out)
    }

    /// Read `want` in `txn` and queue the writes of `f`, or report that `overlay` is out of date.
    async fn attempt<T>(
        &self,
        txn: &mut Txn,
        want: &Want,
        path_id: Option<&str>,
        overlay: Option<CachedOverlay>,
        f: &mut impl FnMut(&View, &mut Changes) -> Result<T>,
    ) -> Result<Attempt<T>> {
        let mut plan = self.plan(want, path_id, overlay);
        let mut results = txn.run(&plan.ops()).await?;
        while let Some(more) = plan.absorb(results)? {
            results = txn.run(&more).await?;
        }
        if plan.overlay_stale {
            return Ok(Attempt::OverlayChanged);
        }
        let mut changes = Changes::default();
        let out = f(&plan.view, &mut changes)?;
        if let PathSel::Id(explicit_id) = &want.path {
            changes.touch_path(explicit_id);
        }
        let overlay_write = (!changes.overlay.is_empty()).then(|| OverlayWrite {
            version: new_overlay_version(),
            edits: changes.overlay.clone(),
        });
        txn.write(changes.into_ops(overlay_write.as_ref().map(|w| w.version.as_str())));
        Ok(Attempt::Ready((out, overlay_write)))
    }

    fn plan(&self, want: &Want, path_id: Option<&str>, overlay: Option<CachedOverlay>) -> Plan {
        Plan::new(self.curriculum.clone(), want, path_id, overlay)
    }

    /// Return the cached overlay, or load it if the cache is empty.
    async fn overlay_or_load(&self) -> Result<CachedOverlay> {
        match self.cached_overlay() {
            Some(cached) => Ok(cached),
            None => self.load_overlay().await,
        }
    }

    /// Read the overlay and its version without a transaction and cache them.
    ///
    /// Each page request reads the version again, and a changed version restarts the load, so the overlay matches its version.
    async fn load_overlay(&self) -> Result<CachedOverlay> {
        'load: loop {
            let mut version: Option<String> = None;
            let mut rows = OverlayRows::default();
            let mut pending = vec![
                (OverlayTable::Lessons, Op::query_all::<LessonRow>()),
                (OverlayTable::Quizzes, Op::query_all::<QuizRow>()),
                (OverlayTable::Removed, Op::query_all::<RemovedQuizRow>()),
            ];
            while !pending.is_empty() {
                let ops: Vec<Op> = std::iter::once(Op::get(OVERLAY_VERSION_KEY))
                    .chain(pending.iter().map(|(_, op)| op.clone()))
                    .collect();
                let mut results = self.read(&ops).await?.into_iter();
                let read_version = overlay_version(results.next())?;
                if version.as_ref().is_some_and(|v| *v != read_version) {
                    continue 'load;
                }
                version = Some(read_version);
                for ((table, op), r) in std::mem::take(&mut pending).into_iter().zip(results) {
                    if let Some(next) = op.next_page(&r) {
                        pending.push((table, next));
                    }
                    rows.absorb(table, r)?;
                }
            }
            let loaded =
                CachedOverlay::new(version.unwrap_or_default(), Arc::new(rows.into_overlay()));
            self.cache_overlay(Some(loaded.clone()));
            return Ok(loaded);
        }
    }

    /// Resolve `selector` to a path ID, choosing the most recent path at most once per command.
    async fn selected_path_id(&self, selector: &PathSel) -> Result<Option<String>> {
        match selector {
            PathSel::None => Ok(None),
            PathSel::Id(id) => Ok(Some(id.clone())),
            PathSel::Recent => self.most_recent_path_id().await,
        }
    }

    async fn most_recent_path_id(&self) -> Result<Option<String>> {
        let mut latest: Option<(DateTime<Utc>, String)> = None;
        for page in self.read_pages(Op::scan(ACTIVITY_PREFIX)).await? {
            for e in page.scan_page()?.entries {
                let (id, at) = parse_activity(&e)?;
                if latest
                    .as_ref()
                    .is_none_or(|l| (at, id) > (l.0, l.1.as_str()))
                {
                    latest = Some((at, id.to_string()));
                }
            }
        }
        Ok(latest.map(|(_, id)| id))
    }

    fn cache_overlay(&self, next: Option<CachedOverlay>) {
        let mut cache = self
            .overlay_cache
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let changed = cache.as_ref().map(|c| &c.version) != next.as_ref().map(|n| &n.version);
        if changed && let (Some(file), Some(n)) = (&self.cache_file, &next) {
            write_cached_overlay(file, &n.version, &n.overlay);
        }
        *cache = next;
    }

    fn cached_overlay(&self) -> Option<CachedOverlay> {
        self.overlay_cache
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    /// Cache the overlay `base` that a transaction checked with its committed `write` applied, or clear the cache if the transaction did not check an overlay.
    fn remember_edits(&self, base: Option<CachedOverlay>, write: OverlayWrite) {
        let next = base.map(|base| {
            let mut o = (*base.overlay).clone();
            for e in &write.edits {
                o.apply(e);
            }
            CachedOverlay::new(write.version, Arc::new(o))
        });
        self.cache_overlay(next);
    }
}

/// Result of a transaction attempt.
enum Attempt<T> {
    Ready((T, Option<OverlayWrite>)),
    /// The stored overlay version differs from the overlay that the attempt used.
    OverlayChanged,
}

#[derive(Debug, Clone, Copy)]
enum OverlayTable {
    Lessons,
    Quizzes,
    Removed,
}

/// Overlay rows accumulated across pages.
#[derive(Default)]
struct OverlayRows {
    lessons: Vec<LessonRow>,
    quizzes: Vec<QuizRow>,
    removed: Vec<RemovedQuizRow>,
}

impl OverlayRows {
    fn absorb(&mut self, table: OverlayTable, r: OpResult) -> Result<()> {
        match table {
            OverlayTable::Lessons => self.lessons.extend(r.rows()?),
            OverlayTable::Quizzes => self.quizzes.extend(r.rows()?),
            OverlayTable::Removed => self.removed.extend(r.rows()?),
        }
        Ok(())
    }

    fn into_overlay(self) -> Overlay {
        Overlay::from_rows(self.lessons, self.quizzes, self.removed)
    }
}

/// Decode the overlay version from the result of a read of [`OVERLAY_VERSION_KEY`].
fn overlay_version(r: Option<OpResult>) -> Result<String> {
    let r = r.ok_or_else(|| Error::BadRow(format!("{OVERLAY_VERSION_KEY}: missing result")))?;
    Ok(match r.entry()? {
        Some(e) => entry_str(&e)?.1.to_string(),
        None => UNWRITTEN_OVERLAY_VERSION.to_string(),
    })
}

/// Accumulator that receives the result of a planned read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Slot {
    OverlayVersion,
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
    /// Overlay to use if the stored version matches.
    overlay: Option<CachedOverlay>,
    /// The stored overlay version differs from the version of `overlay`.
    overlay_stale: bool,
    pending: Vec<(Slot, Op)>,
    paths: Vec<PathFile>,
    cards: Vec<CardRecord>,
    taught: Vec<TaughtRow>,
    activity: HashMap<String, DateTime<Utc>>,
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
    /// Plan the reads of `want`, and check that `overlay` is current instead of reading overlay rows.
    fn new(
        curriculum: Arc<Curriculum>,
        want: &Want,
        path_id: Option<&str>,
        overlay: Option<CachedOverlay>,
    ) -> Self {
        let mut pending = Vec::new();
        if overlay.is_some() {
            pending.push((Slot::OverlayVersion, Op::get(OVERLAY_VERSION_KEY)));
        }
        if want.all_paths {
            pending.extend([
                (Slot::Paths, Op::query_all::<PathFile>()),
                (Slot::Cards, Op::query_all::<CardRecord>()),
                (Slot::Taught, Op::query_all::<TaughtRow>()),
                (Slot::ActivityScan, Op::scan(ACTIVITY_PREFIX)),
            ]);
        } else if let Some(id) = path_id {
            pending.extend([
                (Slot::Path, Op::get_row::<PathFile>(id)),
                (Slot::Cards, Op::query::<CardRecord>(id)),
                (Slot::Taught, Op::query::<TaughtRow>(id)),
                (Slot::Activity, Op::get(activity_key(id))),
            ]);
        }
        Self {
            view: View::new(curriculum, path_id.map(str::to_string)),
            overlay,
            overlay_stale: false,
            pending,
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
    fn absorb(&mut self, results: Vec<OpResult>) -> Result<Option<Vec<Op>>> {
        let pending = std::mem::take(&mut self.pending);
        for ((slot, op), r) in pending.into_iter().zip(results) {
            if let Some(next) = op.next_page(&r) {
                self.pending.push((slot, next));
            }
            match slot {
                Slot::OverlayVersion => {
                    let stored = overlay_version(Some(r))?;
                    match self.overlay.take() {
                        Some(cached) if cached.version == stored => self.view.set_overlay(cached),
                        _ => self.overlay_stale = true,
                    }
                }
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
        if !self.pending.is_empty() {
            return Ok(Some(self.ops()));
        }
        self.finish()?;
        Ok(None)
    }

    /// Group the accumulated rows into path states.
    fn finish(&mut self) -> Result<()> {
        use std::mem::take;
        let mut paths: HashMap<String, PathState> = take(&mut self.paths)
            .into_iter()
            .map(|file| {
                let updated_at = self
                    .activity
                    .get(&file.id)
                    .copied()
                    .unwrap_or(file.created_at);
                let state = PathState {
                    updated_at,
                    file,
                    projection: Projection::default(),
                };
                (state.file.id.clone(), state)
            })
            .collect();
        for r in take(&mut self.cards) {
            if let Some(p) = paths.get_mut(&r.path_id) {
                p.projection
                    .cards
                    .insert(r.quiz_id.clone(), CardRow::try_from(r)?);
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
