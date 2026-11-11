//! Prerequisite subpath management for top-down learning paths.

use std::collections::HashSet;

use crate::db::Db;
use crate::graph::Graph;
use crate::path::PathFile;
use crate::types::Strategy;
use crate::view::Want;
use crate::{Error, Result};

/// Expand the stored subpath of `p` into an ordered list of atom IDs.
pub fn resolved(p: &PathFile, g: &Graph) -> Result<Vec<String>> {
    g.expand_to_atoms(&p.subpath)
}

/// Validate `atoms` and store them as the subpath for a top-down path for `mt path subpath set`.
pub async fn cmd_subpath_set(
    db: &Db,
    explicit_id: Option<&str>,
    atoms: &[String],
) -> Result<String> {
    let Some((tail, _)) = atoms.split_last() else {
        return Err(Error::SubpathEmpty);
    };
    db.transact(&Want::path(explicit_id), |view, changes| {
        let p = &view.path()?.file;
        if p.strategy != Strategy::TopDown {
            return Err(Error::SubpathNotTopDown);
        }
        let g = view.graph()?;
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
        changes.set_subpath(&p.id, atoms);
        Ok(p.id.clone())
    })
    .await
}

/// Clear the stored subpath for a learning path for `mt path subpath clear`.
pub async fn cmd_subpath_clear(db: &Db, explicit_id: Option<&str>) -> Result<String> {
    db.transact(
        &Want::path(explicit_id).without_overlay(),
        |view, changes| {
            let id = view.path()?.file.id.clone();
            changes.set_subpath(&id, &[]);
            Ok(id)
        },
    )
    .await
}
