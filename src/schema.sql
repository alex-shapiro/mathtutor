-- Initial schema for the per-user mathtutor SQLite database.
-- All statements are idempotent so this script doubles as a migration:
-- it runs unconditionally on every `db::open`.

PRAGMA foreign_keys = ON;

CREATE TABLE IF NOT EXISTS paths (
    id         TEXT PRIMARY KEY,
    goal       TEXT NOT NULL,
    created_at DATETIME NOT NULL
);

CREATE TABLE IF NOT EXISTS path_targets (
    path_id TEXT NOT NULL REFERENCES paths(id),
    atom_id TEXT NOT NULL,
    PRIMARY KEY (path_id, atom_id)
);

CREATE TABLE IF NOT EXISTS events (
    id       INTEGER PRIMARY KEY AUTOINCREMENT,
    ts       DATETIME NOT NULL,
    kind     TEXT NOT NULL,
    path_id  TEXT NOT NULL REFERENCES paths(id),
    atom_id  TEXT,
    quiz_id  TEXT,
    rating   INTEGER,
    payload  TEXT CHECK (payload IS NULL OR json_valid(payload))
);

CREATE INDEX IF NOT EXISTS idx_events_path ON events(path_id);
CREATE INDEX IF NOT EXISTS idx_events_atom ON events(atom_id);
CREATE INDEX IF NOT EXISTS idx_events_quiz ON events(quiz_id);

-- Write-through cache of the latest FSRS state per (path, quiz). The
-- event log is the source of truth; this table can be rebuilt by
-- replaying `events` if it's missing or suspected corrupt.
CREATE TABLE IF NOT EXISTS cards (
    path_id          TEXT NOT NULL REFERENCES paths(id),
    quiz_id          TEXT NOT NULL,
    stability        REAL NOT NULL,
    difficulty       REAL NOT NULL,
    due_at           DATETIME NOT NULL,
    last_reviewed_at DATETIME NOT NULL,
    reps             INTEGER NOT NULL,
    lapses           INTEGER NOT NULL,
    PRIMARY KEY (path_id, quiz_id)
);
CREATE INDEX IF NOT EXISTS idx_cards_due ON cards(path_id, due_at);

-- Overlays are global to the user / database, not path-scoped.
CREATE TABLE IF NOT EXISTS overlay_lessons (
    atom_id TEXT PRIMARY KEY,
    body    TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS overlay_quizzes (
    atom_id    TEXT NOT NULL,
    quiz_id    TEXT NOT NULL,
    difficulty TEXT NOT NULL,
    kind       TEXT,
    question   TEXT NOT NULL,
    answer     TEXT NOT NULL,
    rubric     TEXT,
    PRIMARY KEY (quiz_id)
);

CREATE TABLE IF NOT EXISTS overlay_removed_quizzes (
    quiz_id TEXT PRIMARY KEY
);
