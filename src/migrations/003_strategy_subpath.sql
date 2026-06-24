-- Per-path traversal strategy and the top-down subpath.
--
-- `strategy` is mutable navigation config, not part of the path's intent;
-- existing paths default to 'bottom_up', preserving prior behavior.
--
-- `path_subpath` holds the current top-down detour: an ordered sequence of
-- atoms ending in the target it drives toward. It mirrors `path_targets`
-- and is replaced wholesale on `mt path subpath set`, emptied on clear.

ALTER TABLE paths ADD COLUMN strategy TEXT NOT NULL DEFAULT 'bottom_up';

CREATE TABLE path_subpath (
    path_id  TEXT NOT NULL REFERENCES paths(id),
    atom_id  TEXT NOT NULL,
    position INTEGER NOT NULL,
    PRIMARY KEY (path_id, atom_id)
);
