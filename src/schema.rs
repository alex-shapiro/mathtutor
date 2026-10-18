//! Tables of the user database and their creation.
//!
//! The database holds:
//!
//! | table             | primary key           | role                                          |
//! | ----------------- | --------------------- | --------------------------------------------- |
//! | `paths`           | `id`                  | goal, strategy, targets and subpath per path  |
//! | `lessons`         | `atom_id`             | user-authored lessons (overlay)               |
//! | `quizzes`         | `quiz_id`             | user-authored or amended quizzes (overlay)    |
//! | `removed_quizzes` | `quiz_id`             | quiz tombstones (overlay)                     |
//! | `cards`           | `(path_id, quiz_id)`  | FSRS state and answer history per quiz        |
//! | `taught`          | `(path_id, atom_id)`  | lessons taught per path                       |
//! | `oauth_*`         | code or token         | MCP authorization-server state                |
//!
//! Each path's event log is the stream `events.<path id>`, the source of
//! truth that `cards` and `taught` are derived from. The key
//! `activity/<path id>` holds the time of the path's latest event.

use reqwest::Method;
use serde_json::{Value as Json, json};

use crate::Result;
use crate::db::Db;
use crate::error::Error;

/// Bumped whenever a table is added or changed. Existing databases are
/// brought up to date by [`ensure`].
const VERSION: u64 = 1;
const VERSION_KEY: &str = "schema_version";

/// Name of the stream holding `path_id`'s event log.
pub fn events_stream(path_id: &str) -> String {
    format!("events.{path_id}")
}

fn col(name: &str, ty: &str, nullable: bool) -> Json {
    json!({ "name": name, "type": ty, "nullable": nullable })
}

fn tables() -> Vec<Json> {
    let s = |n| col(n, "string", false);
    let s_null = |n| col(n, "string", true);
    let i = |n| col(n, "int", false);
    let f_null = |n| col(n, "float", true);
    let j = |n| col(n, "json", false);
    vec![
        json!({ "name": "paths", "primary_key": ["id"], "columns": [
            s("id"), s("goal"), s("created_at"), s("strategy"), j("targets"), j("subpath"),
        ]}),
        json!({ "name": "lessons", "primary_key": ["atom_id"], "columns": [s("atom_id"), s("body")] }),
        json!({ "name": "quizzes", "primary_key": ["quiz_id"], "columns": [
            s("quiz_id"), s("atom_id"), s("difficulty"), s_null("kind"),
            s("question"), s("answer"), s_null("rubric"),
        ]}),
        json!({ "name": "removed_quizzes", "primary_key": ["quiz_id"], "columns": [
            s("quiz_id"), s("atom_id"),
        ]}),
        json!({ "name": "cards", "primary_key": ["path_id", "quiz_id"], "columns": [
            s("path_id"), s("quiz_id"),
            f_null("stability"), f_null("difficulty"), s_null("due_at"), s_null("last_reviewed_at"),
            i("reps"), i("lapses"), i("presented"), s_null("last_presented_at"),
            s_null("first_correct_at"), j("recent"),
        ], "indexes": [{ "name": "by_due", "columns": ["path_id", "due_at"] }]}),
        json!({ "name": "taught", "primary_key": ["path_id", "atom_id"], "columns": [
            s("path_id"), s("atom_id"), i("count"), s_null("last_at"),
        ]}),
        json!({ "name": "oauth_clients", "primary_key": ["client_id"], "columns": [
            s("client_id"), s_null("client_name"), j("redirect_uris"), s("created_at"),
        ]}),
        json!({ "name": "oauth_codes", "primary_key": ["code"], "columns": [
            s("code"), s("client_id"), s("redirect_uri"), s_null("scope"), s("code_challenge"),
            s("code_challenge_method"), s_null("resource"), s("expires_at"), s_null("used_at"),
        ]}),
        json!({ "name": "oauth_access_tokens", "primary_key": ["token"], "columns": [
            s("token"), s("client_id"), s_null("scope"), s("expires_at"),
        ]}),
        json!({ "name": "oauth_refresh_tokens", "primary_key": ["token"], "columns": [
            s("token"), s("client_id"), s_null("scope"), s("expires_at"),
        ]}),
    ]
}

/// Create the database `name` and its tables unless they already exist.
/// Costs one read when the schema is current.
pub async fn ensure(db: &Db, name: &str) -> Result<()> {
    let probe = [json!({ "op": "get", "key": VERSION_KEY })];
    match db.read(&probe).await {
        Ok(r) if r[0]["value"].as_str().and_then(|v| v.parse().ok()) == Some(VERSION) => {
            return Ok(());
        }
        Ok(_) => {}
        Err(Error::Api(e)) if e.status == 404 => create_db(db, name).await?,
        Err(e) => return Err(e),
    }
    for t in tables() {
        match db.call(Method::POST, "/tables", Some(&t)).await {
            Err(Error::Api(e)) if e.code == "already_exists" => {}
            r => {
                r?;
            }
        }
    }
    db.write(&[json!({ "op": "put", "key": VERSION_KEY, "value": VERSION.to_string() })])
        .await?;
    Ok(())
}

async fn create_db(db: &Db, name: &str) -> Result<()> {
    let body = match name.split_once('.') {
        Some((org, n)) => json!({ "name": n, "org": org }),
        None => json!({ "name": name }),
    };
    match db.root_call(Method::POST, "/v1/dbs", Some(&body)).await {
        Err(Error::Api(e)) if e.code == "already_exists" => Ok(()),
        r => r.map(drop),
    }
}
