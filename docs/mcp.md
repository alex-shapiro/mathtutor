# MCP Server Design: mathtutor

This document outlines the transition of `mathtutor` from a standalone CLI tool to a Model Context Protocol (MCP) server.

## Overview

The current `mathtutor` is a CLI tool (`mt`) that manages a math curriculum DAG and tracks user progress via local AYML files. To make it more accessible to LLM agents (like Claude Desktop), we will wrap the core logic in an MCP server.

## Architecture: Unified Binary (`mt mcp`)

The MCP server is integrated into the existing `mt` tool.

- **Feature Flag:** `mcp` ff in `Cargo.toml` gates `axum`, `rmcp`, `tokio` and enables the `mt mcp` command
- **Transport:** SSE over HTTP for remote access
- **Protocol:** MCP JSON-RPC 2.0 via `rmcp`

## Interface: Tools & Prompts

### Tool Schemas

All CLI tools are ported to MCP tools. The server handles the mapping from JSON-RPC parameters to internal command structures.

#### General Conventions

- **`path_id`**: **Mandatory** for all path-specific tools:
  - State-modifying: `AnswerQuiz`, `UpsertLesson`, `CreateQuiz`, `UpdateQuiz`, `DeleteQuiz`.
  - Progress-related: `GetNext`, `GetState`, `GetTree`.
- **Curriculum Exploration**: `path_id` is **Optional** for `GetItem` and `GetChildren`. If omitted, these tools return static curriculum data only (no per-atom status or progress).
- **`graph`**: Removed from all tool schemas. The server uses its own configured curriculum (embedded or via `MT_GRAPH`).
- **Return Values**: All tools return structured JSON. Resource creation tools (`NewPath`, `CreateQuiz`) return the ID of the new resource in a JSON field.
- **Validation**: The server validates all `atom_id` and `quiz_id` inputs against the merged graph (Base + Overlay) before processing.

#### Tool Definitions

```rust
/// List all learning paths for this user.
struct GetPaths {}

/// Start a new learning path with a goal and target atoms.
struct NewPath {
    goal: String,
    atoms: Vec<String>,
}

/// Get the next action (lesson or quiz) in the path
struct GetNext {
    path_id: String
}

/// Returns a summary of progress, including completed vs. remaining atoms.
struct GetState {
    path_id: String
}

/// Returns the full prerequisite tree with per-atom status.
struct GetTree {
    path_id: String,
    /// Max levels to traverse. Omit for full tree.
    depth: Option<u32>,
}

/// Show a detailed view of a curriculum node (atom, cluster, or area).
struct GetItem {
    id: String,
    path_id: Option<String>,
}

/// Lists children of a curriculum node. Omit `id` for root areas.
struct GetChildren {
    id: Option<String>,
    path_id: Option<String>,
    /// If true, recursively list all descendants.
    recursive: Option<bool>,
}

/// Create or update an agent-authored lesson for an atom.
struct UpsertLesson {
    atom: String,
    body: String,
    path_id: String
}

/// Create an agent-authored quiz.
struct CreateQuiz {
    atom: String,
    question: String,
    answer: String,
    rubric: Option<String>,
    difficulty: Difficulty,
    kind: QuizType,
    path_id: String,
}

/// Update fields of an existing quiz while preserving ID and history.
struct UpdateQuiz {
    quiz_id: String,
    question: Option<String>,
    answer: Option<String>,
    rubric: Option<String>,
    difficulty: Option<Difficulty>,
    kind: Option<QuizType>,
    path_id: String,
}

/// Tombstone a quiz so it no longer appears in the curriculum.
/// Quizzes are never hard-deleted to preserve event log integrity.
struct DeleteQuiz {
    quiz_id: String,
    path_id: String,
}

enum Difficulty {
    Easy,
    Medium,
    Hard,
}

enum QuizType {
    FreeText,
    MultipleChoice,
}

enum Rating {
    Again,
    Hard,
    Good,
    Easy,
}

/// Record a user's quiz answer and FSRS rating.
struct AnswerQuiz {
    quiz_id: String,
    answer: Option<String>,
    rating: Rating,
    path_id: String,
}
```

### Prompt Definitions

A MCP prompt instructs the LLM how to use the MCP server effectively. The MathTutor MCP server includes a `mathtutor-playbook` prompt that returns master instructions for the tutor agent. This prompt mirrors the `mt instruct` MD file used in the CLI, with MCP tool-calling conventions replacing CLI-calling conventions.

### Tool Result Mapping

To provide machine-readable data for the LLM, the server returns JSON from tool calls.

- Format: `application/json`.
- Content: A structured object containing the raw data (IDs, statuses, content). Agents should parse this JSON rather than relying on decorative text formatting.

#### Error Handling

If a tool fails due to business logic (e.g., "atom not found", "invalid rating"), the server returns a `CallToolResult` with `isError: true` and a JSON error object in the content.

JSON-RPC error codes (e.g., -32601 "Method not found") are reserved for structural or communication failures that the LLM cannot resolve.

#### Stdout/Stderr Capture

While the primary output is structured JSON, the MCP response may also include a `logs` field or similar containing any developer-facing diagnostic messages generated during execution.

### Remote Access & Security

#### Authentication Source of Truth

The server currently implements a single-user model. Authentication is performed against a single static API key defined on the server (e.g., via a `MT_API_KEY` environment variable).

- **Multi-user Support:** While the architecture allows for multiple paths, the server assumes a single owner for all paths hosted by that instance.
- **Database Scope:** A single SQLite database represents the entire state for a user (or server instance). The authentication key provides access to the server's management of all paths and data within this database.

#### Authentication Fallback Logic

To support both modern MCP clients and legacy `EventSource` (SSE) browsers, the server implements a sequential authentication check:

1.  **Authorization Header:** First, try to extract a `Bearer` token from the `Authorization` header.
2.  **Query Parameter Fallback:** If no header is present, look for a `token` query parameter
3.  **Precedence:** If both are provided, the `Authorization` header takes precedence
4.  **Error Handling:** If no valid token is found, the server returns a `401 Unauthorized`.

#### Keep-alive & Heartbeats

To prevent network proxies and load balancers from prematurely closing the long-lived SSE connection, the server sends a periodic heartbeat:

- **Interval:** 15 seconds.
- **Payload:** A standard SSE comment (`: keep-alive`) or an empty `heartbeat` event.
- **Client Impact:** Clients should be configured to handle these heartbeats without treating them as tool/resource data.

## Persistence & Storage: SQLite + Turso

The server will be hosted remotely and accessed from multiple devices and so we must migrate the storage layer from local AYML files to SQLite. The server will use Turso for sync/backup.

### Storage Approach

We take a hybrid approach to data storage.

- **Base Curriculum:** Remains a static set of AYML files embedded in the binary. This preserves the developer experience for curriculum authoring.
- **User Database**: A single SQLite database represents the entire state for a user (or server instance). It contains:
  - All learning paths, goals, and target atoms.
  - The user's shared curriculum overlay (agent-authored lessons and quizzes).
  - The event log of user interactions, partitioned by `path_id`.
  - FSRS card states: a materialized view of the latest FSRS parameters (stability, difficulty, due date) for every quiz/path combination, updated on every answer.

FSRS Recomputation: The event log remains the source of truth for all learning path state. The `cards` table is a write-through cache. If the cache is missing or suspected to be corrupt, it can be recomputed by replaying the `events` table.

### Sync Strategy (libSQL)

- **Mode:** `libsql::Builder::new_synced_database` (Embedded Replica with Offline Writes).
- **Initialization:**
  - If `TURSO_URL` and `TURSO_AUTH_TOKEN` are set: Enable remote sync.
  - Otherwise: Fallback to a standard local SQLite database.
- **Sync Behavior:**
  - MCP: Sync in a background tokio task every 5 minutes. State-modifying tool calls trigger a non-blocking background sync.
  - CLI: Sync at the end of every command that modifies state.
  - Timeout: Use a 10-second timeout for all foreground sync operations.
  - Error Handling: If a sync fails or times out, issue a warning to stderr but consider the operation successful. libSQL can sync after a future command.

### SQL Schema

```sql
CREATE TABLE paths (
    id TEXT PRIMARY KEY,
    goal TEXT NOT NULL,
    created_at DATETIME NOT NULL
);

-- Join table for referential integrity and efficient querying of goals
CREATE TABLE path_targets (
    path_id TEXT NOT NULL REFERENCES paths(id),
    atom_id TEXT NOT NULL,
    PRIMARY KEY (path_id, atom_id)
);

CREATE TABLE events (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    ts DATETIME NOT NULL,
    kind TEXT NOT NULL, -- e.g., 'LessonRead', 'QuizAnswered'
    path_id TEXT NOT NULL REFERENCES paths(id),
    atom_id TEXT,
    quiz_id TEXT,
    rating INTEGER, -- Pulled out of payload for easy FSRS querying
    payload TEXT CHECK (payload IS NULL OR json_valid(payload))
);

-- Indexes for performance
CREATE INDEX idx_events_path ON events(path_id);
CREATE INDEX idx_events_atom ON events(atom_id);
CREATE INDEX idx_events_quiz ON events(quiz_id);

-- FSRS Card State: Materialized view of the event log for fast scheduling.
CREATE TABLE cards (
    path_id TEXT NOT NULL REFERENCES paths(id),
    quiz_id TEXT NOT NULL,
    stability REAL NOT NULL,
    difficulty REAL NOT NULL,
    due_at DATETIME NOT NULL,
    last_reviewed_at DATETIME NOT NULL,
    reps INTEGER NOT NULL,
    lapses INTEGER NOT NULL,
    PRIMARY KEY (path_id, quiz_id)
);
CREATE INDEX idx_cards_due ON cards(path_id, due_at);

-- Overlays are global to the user/database, not path-scoped.
CREATE TABLE overlay_lessons (
    atom_id TEXT PRIMARY KEY,
    body TEXT NOT NULL
);

CREATE TABLE overlay_quizzes (
    atom_id TEXT NOT NULL,
    quiz_id TEXT NOT NULL,
    difficulty TEXT NOT NULL, -- easy, medium, hard
    kind TEXT, -- NULL for free_text
    question TEXT NOT NULL,
    answer TEXT NOT NULL,
    rubric TEXT,
    PRIMARY KEY (quiz_id)
);

CREATE TABLE overlay_removed_quizzes (
    quiz_id TEXT PRIMARY KEY
);
```

## Implementation Plan (PR-Sized Tasks)

1.  **PR 1: Database Foundation.**
    - Update `Cargo.toml` dependencies (`libsql`, `tokio`, `serde_json`).
    - Create `src/db.rs`: setup `libsql` connection pooling, initial schema migrations (paths, events, cards, overlays).
    - Implement the "Local vs. Synced" initialization logic.
2.  **PR 2: Event Log & FSRS Cache Migration.**
    - Refactor `src/event_log.rs` to use SQL for appending and loading events.
    - **Write-through Cache:** Implement logic to update the `cards` table whenever a `QuizAnswered` event is recorded.
    - Create `src/cards.rs` (or update) to support reading from the `cards` table for O(1) scheduling lookups.
3.  **PR 3: User Overlay & Store SQL Migration.**
    - Refactor `src/overlay.rs` and `src/store.rs` to use SQL.
    - **Scope Shift:** Ensure overlays (lessons/quizzes) are stored globally in the user database, not partitioned by `path_id`.
    - **Upsert Lessons:** `mt store lesson` becomes an upsert (matches the MCP `UpsertLesson` tool); a second call replaces the body and emits `lesson_amended` instead of `lesson_authored`.
    - **Validation:** Update `Graph::load_for_path` to merge the static base graph with the global SQL-resident overlay and implement fast-fail validation for IDs.
4.  **PR 4: AYML to SQLite Migration Tool.**
    - Implement `mt migrate-from-ayml` CLI command to port existing local paths and overlays into the new schema.
    - **Idempotency:** Use `INSERT OR IGNORE` to allow safe repeated runs.
5.  **PR 5: MCP Server (SSE) + Tool Implementation.**
    - Implement `src/mcp.rs` using `rmcp`.
    - Port all tools to return **machine-readable JSON**.
    - Implement the `GetPaths` tool.
    - Setup `axum` server with SSE routes, heartbeats (15s), and API Key authentication.
    - **Graceful Shutdown:** Implement signal handlers to ensure final `db.sync()` before exit.
6.  **PR 6: Deployment & Infrastructure.**
    - Create `Dockerfile` and Fly.io/Railway configuration.
    - Setup CI/CD for automated deployments.
