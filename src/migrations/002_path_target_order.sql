-- Preserve the topo-sorted order of `target_atoms` set at `mt new` time.
-- Without an explicit column SQLite doesn't promise selection order, and
-- the scheduler's outer loop walks targets in order to decide which one
-- to advance first when multiple are pending.

ALTER TABLE path_targets ADD COLUMN position INTEGER NOT NULL DEFAULT 0;
