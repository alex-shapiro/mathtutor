# MCP Server Design: mathtutor

This document outlines the transition of `mathtutor` from a standalone CLI tool to a Model Context Protocol (MCP) server.

## Overview

The current `mathtutor` is a CLI tool (`mt`) that manages a math curriculum DAG and tracks user progress via local AYML files. To make it more accessible to LLM agents (like Claude Desktop), we will wrap the core logic in an MCP server.

## Architecture: Unified Binary (`mt mcp`)

The MCP server is integrated into the existing `mt` tool.

- **Feature Flag:** `mcp` ff in `Cargo.toml` gates `axum`, `rmcp`, `tokio` and enables the `mt mcp` command
- **Transport:** SSE over HTTP for remote access
- **Protocol:** MCP JSON-RPC 2.0 via `rmcp`

## Tool Schemas

All CLI tools are ported to MCP tools. The server handles the mapping from JSON-RPC parameters to internal command structures.

### General Conventions

- **`path_id`**: Optional in all tools. If omitted, the server defaults to the most recently created/accessed path for the authenticated session.
- **`graph`**: Removed from all tool schemas. The server uses its own configured curriculum (embedded or via `MT_GRAPH`).

### Tool Definitions

```rust
/// Start a new learning path with a goal and target atoms
struct NewPath {
    goal: String,
    atoms: Vec<String>,
}

/// Get the next action (lesson or quiz) in the path
struct GetNext {
    path_id: Option<String>
}

/// Returns a summary of progress, including completed vs. remaining atoms.
struct GetState {
    path_id: Option<String>
}

/// Returns the full prerequisite tree with per-atom status.
struct GetTree {
    path_id: Option<String>
}

/// Show a detailed view of a curriculum node (atom, cluster, or area).
struct GetItem {
    id: String,
    path_id: Option<String>,
}

/// Lists children of a curriculum node. Omit `id` for root areass
struct GetChildren {
    id: Option<String>,
    path_id: Option<String>,
}

/// Create or update an agent-authored lesson for an atom.
struct UpsertLesson {
    atom: String,
    body: String,
    path_id: Option<String>
}

/// Create an agent-authored quiz.
struct CreateQuiz {
    atom: String,
    question: String,
    answer: String,
    difficulty: QuizDifficulty,
    type: QuizType,
    path_id: Option<String>,
}

/// Update fields of an existing quiz while preserving ID and history.
struct UpdateQuiz {
    quiz_id: String,
    question: Option<String>,
    answer: Option<String>,
    difficulty: Option<QuizDifficulty>,
    type: Option<QuizType>,
    path_id: Option<String>,
}

/// Tombstone a quiz so it no longer appears in the curriculum.
struct DeleteQuiz {
    quiz_id: String,
    path_id: Option<String>,
}

enum QuizDifficulty {
    Easy,
    Medium,
    Hard,
}

enum QuizType {
    FreeChoice,
    MultipleChoice,
}

/// Record a user's quiz answer and FSRS rating.
struct AnswerQuiz {
    id: String,
    answer: Option<String>,
    rating: QuizRating,
    path_id: Option<String>,
}
```

### Prompt Definitions

A MCP prompt instructs the LLM how to use the MCP server effectively. The MathTutor MCP server includes a `mathtutor-playbook` prompt that returns master instructions for the tutor agent. This prompt mirrors the `mt instruct` MD file used in the CLI, with MCP tool-calling conventions replacing CLI-calling conventions.

## Data Persistence & Backup: SQLite + Turso

The server will be hosted remotely and accessed from multiple devices and so we must migrate the storage layer from local AYML files to SQLite. The server will use Turso for sync/backup.

- **Storage:** We take a hybrid approach to data storage.
  - The base curriculum remains a static set of AYML files embedded in the binary
  - Each learning path migrates to an independent SQLite database containing:
    - The learning path goals and target atoms
    - The curriculum overlay
    - The event log of user interactions
- **Sync:** Turso's `libsql` driver syncs in an asynchronous background thread after each update.
- **Implementation:**
  - Add a
  - Rewrite `src/store.rs`, `src/event_log.rs`, and `src/overlay.rs` to use SQL queries.
  - The server runs a local SQLite file for low-latency reads and the driver pushes updates to Turso

### SQL Schema

```sql
CREATE TABLE paths (
    id TEXT PRIMARY KEY,
    goal TEXT NOT NULL,
    created_at DATETIME NOT NULL,
    target_atoms TEXT NOT NULL CHECK (json_valid(target_atoms))
);

CREATE TABLE events (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    ts DATETIME NOT NULL,
    kind TEXT NOT NULL,
    path_id TEXT NOT NULL REFERENCES paths(id),
    atom_id TEXT,
    quiz_id TEXT,
    payload TEXT CHECK (payload IS NULL OR json_valid(payload))
);

-- Indexes for performance
CREATE INDEX idx_events_path ON events(path_id);
CREATE INDEX idx_events_atom ON events(atom_id);

CREATE TABLE overlay_lessons (
    path_id TEXT NOT NULL REFERENCES paths(id),
    atom_id TEXT NOT NULL,
    body TEXT NOT NULL,
    PRIMARY KEY (path_id, atom_id)
);

CREATE TABLE overlay_quizzes (
    path_id TEXT NOT NULL REFERENCES paths(id),
    atom_id TEXT NOT NULL,
    quiz_id TEXT NOT NULL,
    difficulty TEXT NOT NULL,
    kind TEXT, -- NULL for free_text
    question TEXT NOT NULL,
    answer TEXT NOT NULL,
    rubric TEXT,
    PRIMARY KEY (path_id, quiz_id)
);

CREATE TABLE overlay_removed_quizzes (
    path_id TEXT NOT NULL REFERENCES paths(id),
    quiz_id TEXT NOT NULL,
    PRIMARY KEY (path_id, quiz_id)
);
```

## Data Persistence: SQLite + Turso

### libSQL Sync Strategy

- **Mode:** `libsql::Builder::new_synced_database` (Embedded Replica with Offline Writes).
- **Initialization:**
  - If `TURSO_URL` and `TURSO_AUTH_TOKEN` are set: Enable remote sync.
  - Otherwise: Fallback to a standard local SQLite database.
- **Sync Trigger:**
  - Server Mode: A background tokio task syncs every 5 minutes (or on startup/shutdown).
  - CLI Mode: `db.sync()` is called at the end of every command that modifies state (store, answer, etc.).
    - 3 second timeout if a sync has occurred in the last 5 minutes
    - 10 second timeout if a sync has occurred in the last hour
    - 30 second timeout otherwise
    - If a db has not synced in the last 5 minutes, issue a warning to stderr
    - If a db takes >3 seconds to sync, issue a warning to stderr

## Remote Access & Security

### Authentication Source of Truth

The server currently implements a single-user model. Authentication is performed against a single static API key defined on the server (e.g., via a `MT_API_KEY` environment variable).

- **Multi-user Support:** While the architecture allows for multiple paths, the server assumes a single owner for all paths hosted by that instance.
- **Database Scope:** Each SQLite database represents an independent learning path. The authentication key provides access to the server's management of these paths.

### Authentication Fallback Logic

To support both modern MCP clients and legacy `EventSource` (SSE) browsers, the server implements a sequential authentication check:

1.  **Authorization Header:** First, try to extract a `Bearer` token from the `Authorization` header.
2.  **Query Parameter Fallback:** If no header is present, look for a `token` query parameter
3.  **Precedence:** If both are provided, the `Authorization` header takes precedence
4.  **Error Handling:** If no valid token is found, the server returns a `401 Unauthorized`.

_Security Note:_ While query parameters are less secure due to potential logging, this fallback is required for standard SSE client compatibility. Users are encouraged to use HTTPS and rotate keys regularly.

## Implementation Plan (PR-Sized Tasks)

1.  **PR 1: Database Foundation.**
    - Update `Cargo.toml` dependencies (`libsql`, `tokio`, `serde_json`).
    - Create `src/db.rs`: setup `libsql` connection pooling, `json_valid` checks, and initial schema migrations.
    - Implement the "Local vs. Synced" initialization logic.
2.  **PR 2: Event Log SQL Migration.**
    - Refactor `src/event_log.rs` to use SQL for appending and loading events.
    - Update `src/scheduler.rs` and `src/cards.rs` if they need direct DB access.
3.  **PR 3: Overlay & Store SQL Migration.**
    - Refactor `src/overlay.rs` and `src/store.rs` to use SQL.
    - Update `Graph::load_for_path` to fetch the overlay from the database.
4.  **PR 4: AYML to SQLite Migration Tool.**
    - Implement `mt migrate-from-ayml` CLI command.
    - **Idempotency:** Use `INSERT OR IGNORE` and unique constraints to allow repeated runs.
5.  **PR 5: MCP Server (SSE) + Authentication.**
    - Implement `src/mcp.rs` using `rmcp`.
    - Setup `axum` server with SSE routes and API Key authentication (Header + Query Fallback).
6.  **PR 6: Deployment & Infrastructure.**
    - Create `Dockerfile` and Fly.io/Railway configuration.
    - Setup CI/CD for automated deployments.
