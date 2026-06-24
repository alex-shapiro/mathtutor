//! Top-down subpath: a mutable, ordered detour through prerequisites that
//! ends in a target atom. Stored in the `path_subpath` table — replaced
//! wholesale on `mt path subpath set`, emptied on `clear`. The scheduler
//! reads it to walk a top-down learner back to a target they're stuck on.

use std::collections::HashSet;
use std::path::Path;

use libsql::{Connection, params};

use crate::graph::Graph;
use crate::path::{load_path, resolve_id};
use crate::types::Strategy;
use crate::{Error, Result};

/// Load a path's subpath in order. Empty when none is set.
pub async fn load(conn: &Connection, path_id: &str) -> Result<Vec<String>> {
    let mut rows = conn
        .query(
            "SELECT atom_id FROM path_subpath WHERE path_id = ? ORDER BY position ASC",
            params![path_id],
        )
        .await?;
    let mut out = Vec::new();
    while let Some(r) = rows.next().await? {
        out.push(r.get::<String>(0)?);
    }
    Ok(out)
}

/// Replace `path_id`'s subpath with `atoms`, preserving their order.
/// Atomicity is the caller's responsibility.
///
/// # Panics
/// Panics if `atoms.len()` doesn't fit in `i64` (≈9e18 atoms).
pub async fn replace(conn: &Connection, path_id: &str, atoms: &[String]) -> Result<()> {
    clear(conn, path_id).await?;
    for (i, atom) in atoms.iter().enumerate() {
        let position = i64::try_from(i).expect("position fits in i64");
        conn.execute(
            "INSERT INTO path_subpath(path_id, atom_id, position) VALUES (?, ?, ?)",
            params![path_id, atom.as_str(), position],
        )
        .await?;
    }
    Ok(())
}

/// Delete `path_id`'s subpath. No-op when none is set.
pub async fn clear(conn: &Connection, path_id: &str) -> Result<()> {
    conn.execute(
        "DELETE FROM path_subpath WHERE path_id = ?",
        params![path_id],
    )
    .await?;
    Ok(())
}

/// `mt path subpath set`: validate `atoms` and store them as the path's
/// subpath. Requires a top-down strategy; every atom must be a leaf in the
/// graph, the atoms must be distinct, and the last must be a path target.
pub async fn cmd_subpath_set(
    conn: &Connection,
    explicit_id: Option<&str>,
    atoms: &[String],
    graph_dir: Option<&Path>,
) -> Result<String> {
    let Some((tail, _)) = atoms.split_last() else {
        return Err(Error::SubpathEmpty);
    };
    let id = resolve_id(conn, explicit_id).await?;
    let p = load_path(conn, &id).await?;
    if p.strategy != Strategy::TopDown {
        return Err(Error::SubpathNotTopDown);
    }

    let g = Graph::load_for_path(conn, graph_dir).await?;
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

    let tx = conn.transaction().await?;
    replace(&tx, &id, atoms).await?;
    tx.commit().await?;
    Ok(id)
}

/// `mt path subpath clear`: drop the path's subpath.
pub async fn cmd_subpath_clear(conn: &Connection, explicit_id: Option<&str>) -> Result<String> {
    let id = resolve_id(conn, explicit_id).await?;
    let tx = conn.transaction().await?;
    clear(&tx, &id).await?;
    tx.commit().await?;
    Ok(id)
}
