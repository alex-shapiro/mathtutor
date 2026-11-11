//! Transactional database snapshots and atomic write sets.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::Path;
use std::sync::{Arc, Mutex, Weak};

use chrono::{DateTime, Utc};

use crate::cards::{self, CardRow, ProjectionTarget};
use crate::db::client::Entry;
use crate::db::{Db, Op, OpResult, Txn, ts};
use crate::event_log::Event;
use crate::graph::Graph;
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

pub(crate) type CachedOverlay = (String, Arc<Overlay>);

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
    Some((f.version, Arc::new(f.overlay)))
}

fn write_cached_overlay(path: &Path, version: &str, overlay: &Overlay) {
    let write = || -> std::io::Result<()> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let tmp = path.with_extension("tmp");
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
pub enum PathSel {
    None,
    Id(String),
    Recent,
}

impl PathSel {
    /// Return [`PathSel::Id`] if `explicit` is `Some`, or [`PathSel::Recent`] if `None`.
    pub fn from(explicit: Option<&str>) -> Self {
        explicit.map_or(Self::Recent, |id| Self::Id(id.to_string()))
    }
}

/// Specification of data to load into a [`View`].
#[derive(Debug, Clone)]
pub struct Want {
    pub path: PathSel,
    pub overlay: bool,
    pub all_paths: bool,
}

impl Want {
    /// Request the curriculum overlay and the selected path state.
    pub fn path(explicit: Option<&str>) -> Self {
        Self {
            path: PathSel::from(explicit),
            overlay: true,
            all_paths: false,
        }
    }

    /// Request only the curriculum overlay.
    pub fn overlay() -> Self {
        Self {
            path: PathSel::None,
            overlay: true,
            all_paths: false,
        }
    }
}

/// Loaded state for a single learning path.
#[derive(Debug, Clone)]
pub struct PathState {
    pub file: PathFile,
    pub cards: HashMap<String, CardRow>,
    pub taught: HashMap<String, Taught>,
}

/// Presentation count and last presentation timestamp for a lesson on a path.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Taught {
    pub count: u32,
    pub last_at: Option<DateTime<Utc>>,
}

impl Taught {
    pub fn into_row(self, path_id: &str, atom_id: String) -> TaughtRow {
        TaughtRow {
            path_id: path_id.to_string(),
            atom_id,
            count: self.count,
            last_at: self.last_at,
        }
    }
}

impl PathState {
    /// Return the [`PathProgress`] derived from this path state.
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

    /// Return quiz cards due at or before `now`, ordered from earliest to latest due timestamp.
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

/// Database snapshot read by a transaction or [`Db::snapshot`].
#[derive(Debug, Default)]
pub struct View {
    pub overlay: Arc<Overlay>,
    path: Option<PathState>,
    pub paths: Vec<PathState>,
}

impl View {
    /// Return the curriculum graph merged with this view's overlay.
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

    /// Return the selected [`PathState`], or [`Error::NoPath`] if not found.
    pub fn path(&self) -> Result<&PathState> {
        self.path.as_ref().ok_or(Error::NoPath)
    }
}

/// Pending write operations accumulated during a transaction.
#[derive(Debug, Default)]
pub struct Changes {
    ops: Vec<Op>,
    overlay: Vec<Edit>,
    cards: BTreeMap<(String, String), CardRow>,
    taught: BTreeMap<(String, String), Taught>,
    events: BTreeMap<String, Vec<Event>>,
    active_paths: BTreeSet<String>,
}

impl Changes {
    /// Record a raw database write operation.
    pub fn op(&mut self, op: Op) {
        self.ops.push(op);
    }

    /// Set the updated [`CardRow`] for its `(path_id, quiz_id)` pair.
    pub fn set_card(&mut self, c: CardRow) {
        self.cards.insert((c.path_id.clone(), c.quiz_id.clone()), c);
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
    pub fn append(&mut self, view: &View, e: &Event) -> Result<()> {
        let base = view
            .path
            .as_ref()
            .filter(|p| p.file.id == e.path)
            .or_else(|| view.paths.iter().find(|p| p.file.id == e.path));
        match cards::projection_target(e)? {
            ProjectionTarget::Taught { atom_id } => {
                let key = (e.path.clone(), atom_id.to_string());
                let t = self.taught.entry(key).or_insert_with(|| {
                    base.and_then(|p| p.taught.get(atom_id).cloned())
                        .unwrap_or_default()
                });
                cards::apply_taught_event(t, e);
            }
            ProjectionTarget::Card { quiz_id } => {
                let key = (e.path.clone(), quiz_id.to_string());
                let c = self.cards.entry(key).or_insert_with(|| {
                    base.and_then(|p| p.cards.get(quiz_id).cloned())
                        .unwrap_or_else(|| CardRow::new(&e.path, quiz_id))
                });
                cards::apply_event(c, e)?;
            }
            ProjectionTarget::None => {}
        }
        self.touch_path(&e.path);
        self.events
            .entry(e.path.clone())
            .or_default()
            .push(e.clone());
        Ok(())
    }

    fn into_ops(self, overlay_version: Option<&str>) -> Vec<Op> {
        let mut ops = self.ops;
        if let Some(version) = overlay_version {
            ops.extend(self.overlay.iter().flat_map(Edit::ops));
            ops.push(Op::put(OVERLAY_VERSION_KEY, version.to_string()));
        }
        ops.extend(
            self.cards
                .values()
                .map(|c| Op::upsert(&CardRecord::from(c))),
        );
        ops.extend(
            self.taught
                .into_iter()
                .map(|((path_id, atom_id), t)| Op::upsert(&t.into_row(&path_id, atom_id))),
        );
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
            let plan = Plan::new(want, path_id.as_deref(), None);
            let (overlay, plan) = tokio::try_join!(self.load_overlay(), self.read_plan(plan))?;
            let mut view = plan.view;
            view.overlay = overlay.1;
            return Ok(view);
        }
        let plan = self
            .read_plan(Plan::new(want, path_id.as_deref(), cached))
            .await?;
        let stale = plan.overlay_stale;
        let mut view = plan.view;
        if stale {
            view.overlay = self.load_overlay().await?.1;
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
        let mut retry = self.client.retry();
        let (out, overlay_write) = loop {
            let mut txn = self.begin();
            let r = match self
                .attempt(&mut txn, want, path_id.as_deref(), overlay.clone(), &mut f)
                .await
            {
                Ok(Attempt::OverlayChanged) => {
                    drop(txn);
                    overlay = Some(self.load_overlay().await?);
                    continue;
                }
                Ok(Attempt::Ready(ready)) => Ok(ready),
                Err(e) => Err(e),
            };
            if let Some(r) = retry.finish(txn.inner, r).await? {
                break r;
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
        let mut plan = Plan::new(want, path_id, overlay);
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
            let loaded = (version.unwrap_or_default(), Arc::new(rows.into_overlay()));
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
        let mut op = Op::scan(ACTIVITY_PREFIX);
        let mut latest: Option<(DateTime<Utc>, String)> = None;
        loop {
            let page = self.read_one(op.clone()).await?;
            let next = op.next_page(&page);
            for e in page.scan_page()?.entries {
                let (id, at) = parse_activity(&e)?;
                if latest
                    .as_ref()
                    .is_none_or(|l| (at, id) > (l.0, l.1.as_str()))
                {
                    latest = Some((at, id.to_string()));
                }
            }
            let Some(next) = next else { break };
            op = next;
        }
        Ok(latest.map(|(_, id)| id))
    }

    fn cache_overlay(&self, next: Option<CachedOverlay>) {
        let mut cache = self
            .overlay_cache
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let changed = cache.as_ref().map(|c| &c.0) != next.as_ref().map(|n| &n.0);
        if changed && let (Some(file), Some((v, o))) = (&self.cache_file, &next) {
            write_cached_overlay(file, v, o);
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
        let next = base.map(|(_, overlay)| {
            let mut o = (*overlay).clone();
            for e in &write.edits {
                o.apply(e);
            }
            (write.version, Arc::new(o))
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

#[derive(Debug)]
enum PathRow {
    Unread,
    Missing,
    Found(PathFile),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Slot {
    OverlayVersion,
    Paths,
    AllCards,
    AllTaught,
    Activity,
    PathActivity,
    PathRow,
    Cards,
    Taught,
}

struct Plan {
    view: View,
    /// Overlay to use if the stored version matches.
    overlay: Option<CachedOverlay>,
    /// The stored overlay version differs from the version of `overlay`.
    overlay_stale: bool,
    pending: Vec<(Slot, Op)>,
    activity: HashMap<String, DateTime<Utc>>,
    path_row: PathRow,
    cards: Vec<CardRecord>,
    taught: Vec<TaughtRow>,
    paths: Vec<PathFile>,
    all_cards: Vec<CardRecord>,
    all_taught: Vec<TaughtRow>,
}

fn path_reads(id: &str) -> [(Slot, Op); 3] {
    [
        (Slot::PathRow, Op::get_row::<PathFile>(id)),
        (Slot::Cards, Op::query::<CardRecord>(id)),
        (Slot::Taught, Op::query::<TaughtRow>(id)),
    ]
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
    fn new(want: &Want, path_id: Option<&str>, overlay: Option<CachedOverlay>) -> Self {
        let mut pending = Vec::new();
        if overlay.is_some() {
            pending.push((Slot::OverlayVersion, Op::get(OVERLAY_VERSION_KEY)));
        }
        if want.all_paths {
            pending.push((Slot::Paths, Op::query_all::<PathFile>()));
            pending.push((Slot::AllCards, Op::query_all::<CardRecord>()));
            pending.push((Slot::AllTaught, Op::query_all::<TaughtRow>()));
            pending.push((Slot::Activity, Op::scan(ACTIVITY_PREFIX)));
        }
        if let Some(id) = path_id {
            pending.push((Slot::PathActivity, Op::get(activity_key(id))));
            pending.extend(path_reads(id));
        }
        Self {
            view: View::default(),
            overlay,
            overlay_stale: false,
            pending,
            activity: HashMap::new(),
            path_row: PathRow::Unread,
            cards: Vec::new(),
            taught: Vec::new(),
            paths: Vec::new(),
            all_cards: Vec::new(),
            all_taught: Vec::new(),
        }
    }

    fn ops(&self) -> Vec<Op> {
        self.pending.iter().map(|(_, op)| op.clone()).collect()
    }

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
                        Some((version, overlay)) if version == stored => {
                            self.view.overlay = overlay;
                        }
                        _ => self.overlay_stale = true,
                    }
                }
                Slot::Paths => self.paths.extend(r.rows()?),
                Slot::AllCards => self.all_cards.extend(r.rows()?),
                Slot::AllTaught => self.all_taught.extend(r.rows()?),
                Slot::Cards => self.cards.extend(r.rows()?),
                Slot::Taught => self.taught.extend(r.rows()?),
                Slot::PathRow => {
                    self.path_row = r.row()?.map_or(PathRow::Missing, PathRow::Found);
                }
                Slot::Activity => {
                    for e in r.scan_page()?.entries {
                        let (id, at) = parse_activity(&e)?;
                        self.activity.insert(id.to_string(), at);
                    }
                }
                Slot::PathActivity => {
                    if let Some(e) = r.entry()? {
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

    fn finish(&mut self) -> Result<()> {
        use std::mem::take;
        if let PathRow::Found(file) = std::mem::replace(&mut self.path_row, PathRow::Unread) {
            let (cards, taught) = (take(&mut self.cards), take(&mut self.taught));
            self.view.path = Some(self.path_state(file, cards, taught)?);
        }
        let mut cards: HashMap<String, Vec<CardRecord>> = HashMap::new();
        for c in take(&mut self.all_cards) {
            cards.entry(c.path_id.clone()).or_default().push(c);
        }
        let mut taught: HashMap<String, Vec<TaughtRow>> = HashMap::new();
        for t in take(&mut self.all_taught) {
            taught.entry(t.path_id.clone()).or_default().push(t);
        }
        let mut paths = take(&mut self.paths);
        paths.sort_by(|a, b| {
            a.created_at
                .cmp(&b.created_at)
                .then_with(|| a.id.cmp(&b.id))
        });
        self.view.paths = paths
            .into_iter()
            .map(|file| {
                let c = cards.remove(&file.id).unwrap_or_default();
                let t = taught.remove(&file.id).unwrap_or_default();
                self.path_state(file, c, t)
            })
            .collect::<Result<_>>()?;
        Ok(())
    }

    fn path_state(
        &self,
        mut file: PathFile,
        cards: Vec<CardRecord>,
        taught: Vec<TaughtRow>,
    ) -> Result<PathState> {
        file.updated_at = self
            .activity
            .get(&file.id)
            .copied()
            .unwrap_or(file.created_at);
        Ok(PathState {
            file,
            cards: cards
                .into_iter()
                .map(|r| Ok((r.quiz_id.clone(), CardRow::try_from(r)?)))
                .collect::<Result<_>>()?,
            taught: taught
                .into_iter()
                .map(|t| {
                    let taught = Taught {
                        count: t.count,
                        last_at: t.last_at,
                    };
                    (t.atom_id, taught)
                })
                .collect(),
        })
    }
}
