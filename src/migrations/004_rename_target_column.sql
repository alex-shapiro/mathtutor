-- `path_targets` now stores the learner's chosen target IDs verbatim
-- (atoms, clusters, or area roots) and they are expanded to atoms
-- on graph load. Rename the column from `atom_id` to `target_id`
-- to reflect that a target is no longer necessarily an atom.

ALTER TABLE path_targets RENAME COLUMN atom_id TO target_id;
