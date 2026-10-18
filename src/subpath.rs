//! Top-down subpath: a mutable, ordered detour through prerequisites that
//! ends in a target atom. Stored with its path, replaced wholesale on
//! `mt path subpath set` and emptied on `clear`. The scheduler
//! reads it to walk a top-down learner back to a target they're stuck on.

use std::collections::HashSet;
use std::path::Path;

use crate::db::Db;
use crate::graph::Graph;
use crate::path::{self, PathFile};
use crate::types::Strategy;
use crate::view::{PathSel, Want};
use crate::{Error, Result};

/// A path's subpath, expanded to atoms against `g` and kept in order,
/// mirroring how path targets are resolved.
pub fn resolved(p: &PathFile, g: &Graph) -> Result<Vec<String>> {
    g.expand_to_atoms(&p.subpath)
}

/// `mt path subpath set`: validate `atoms` and store them as the path's
/// subpath. Requires a top-down strategy; every atom must be a leaf in the
/// graph, the atoms must be distinct, and the last must be a path target.
pub async fn cmd_subpath_set(
    db: &Db,
    explicit_id: Option<&str>,
    atoms: &[String],
    graph_dir: Option<&Path>,
) -> Result<String> {
    let Some((tail, _)) = atoms.split_last() else {
        return Err(Error::SubpathEmpty);
    };
    db.transact(&Want::path(explicit_id), |view, changes| {
        let p = &view.path()?.file;
        if p.strategy != Strategy::TopDown {
            return Err(Error::SubpathNotTopDown);
        }
        let g = view.graph(graph_dir)?;
        if !p.resolve_targets(&g)?.iter().any(|t| t == tail) {
            return Err(Error::SubpathTailNotTarget(tail.clone()));
        }
        let mut seen = HashSet::with_capacity(atoms.len());
        for a in atoms {
            if !seen.insert(a.as_str()) {
                return Err(Error::SubpathDuplicateAtom(a.clone()));
            }
            match g.by_id.get(a) {
                Some(c) if c.children_ids.is_empty() => {}
                Some(_) => return Err(Error::NotAtom(a.clone())),
                None => return Err(Error::AtomNotFound(a.clone())),
            }
        }
        path::set_subpath(changes, &p.id, atoms);
        Ok(p.id.clone())
    })
    .await
}

/// `mt path subpath clear`: drop the path's subpath.
pub async fn cmd_subpath_clear(db: &Db, explicit_id: Option<&str>) -> Result<String> {
    let want = Want {
        path: PathSel::from(explicit_id),
        overlay: false,
        all_paths: false,
    };
    db.transact(&want, |view, changes| {
        let id = view.path()?.file.id.clone();
        path::set_subpath(changes, &id, &[]);
        Ok(id)
    })
    .await
}
