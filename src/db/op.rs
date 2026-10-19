//! Transaction ops and their results, typed over the rows they carry.

use chrono::TimeDelta;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::Value as Json;

use crate::{Error, Result};

/// Rows a query, scan or stream read returns per page; the server's maximum.
pub const PAGE: usize = 1000;

/// A row of a table, keyed by string columns.
pub trait Row: Serialize + DeserializeOwned {
    const TABLE: &'static str;
}

/// One op of a transaction.
#[derive(Debug, Clone, Serialize)]
pub struct Op(Kind);

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "op", rename_all = "snake_case")]
enum Kind {
    Get {
        key: String,
    },
    Put {
        key: String,
        value: String,
    },
    Increment {
        key: String,
    },
    Scan {
        prefix: String,
        limit: usize,
        #[serde(skip_serializing_if = "Option::is_none")]
        cursor: Option<String>,
    },
    GetRow {
        table: &'static str,
        pk: Vec<String>,
    },
    InsertRow {
        table: &'static str,
        row: Json,
        #[serde(skip_serializing_if = "Option::is_none")]
        ttl_ms: Option<u64>,
    },
    UpsertRow {
        table: &'static str,
        row: Json,
    },
    UpdateRow {
        table: &'static str,
        pk: Vec<String>,
        set: Json,
    },
    DeleteRow {
        table: &'static str,
        pk: Vec<String>,
    },
    DeleteRows {
        table: &'static str,
        #[serde(rename = "where")]
        selection: KeyPrefix,
    },
    QueryRows {
        table: &'static str,
        eq: Vec<String>,
        limit: usize,
        #[serde(skip_serializing_if = "Option::is_none")]
        cursor: Option<String>,
    },
    Append {
        stream: String,
        records: Vec<Record>,
        create: bool,
    },
    ReadStream {
        stream: String,
        from_seq: u64,
        limit: usize,
    },
}

#[derive(Debug, Clone, Serialize)]
struct KeyPrefix {
    eq: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
struct Record {
    json: Json,
}

fn keys(k: &[&str]) -> Vec<String> {
    k.iter().map(|s| (*s).to_string()).collect()
}

/// Rows and records are plain data structs, whose serialization cannot fail.
fn to_json(v: &impl Serialize) -> Json {
    serde_json::to_value(v).expect("rows serialize to JSON")
}

impl Op {
    /// Read a key's value.
    pub fn get(key: impl Into<String>) -> Self {
        Self(Kind::Get { key: key.into() })
    }

    pub fn put(key: impl Into<String>, value: impl Into<String>) -> Self {
        Self(Kind::Put {
            key: key.into(),
            value: value.into(),
        })
    }

    /// Add one to a key's integer value, starting from zero.
    pub fn increment(key: impl Into<String>) -> Self {
        Self(Kind::Increment { key: key.into() })
    }

    /// Read the keys starting with `prefix`, a page at a time.
    pub fn scan(prefix: impl Into<String>) -> Self {
        Self(Kind::Scan {
            prefix: prefix.into(),
            limit: PAGE,
            cursor: None,
        })
    }

    /// Read the row of `R` with primary key `pk`.
    pub fn get_row<R: Row>(pk: &[&str]) -> Self {
        Self(Kind::GetRow {
            table: R::TABLE,
            pk: keys(pk),
        })
    }

    /// Read the rows of `R` whose primary key starts with `eq`, a page at a time.
    pub fn query<R: Row>(eq: &[&str]) -> Self {
        Self(Kind::QueryRows {
            table: R::TABLE,
            eq: keys(eq),
            limit: PAGE,
            cursor: None,
        })
    }

    /// Insert `row`, failing the transaction if its primary key is taken.
    pub fn insert<R: Row>(row: &R) -> Self {
        Self(Kind::InsertRow {
            table: R::TABLE,
            row: to_json(row),
            ttl_ms: None,
        })
    }

    /// Insert `row`, which the database deletes after `ttl`.
    pub fn insert_expiring<R: Row>(row: &R, ttl: TimeDelta) -> Self {
        Self(Kind::InsertRow {
            table: R::TABLE,
            row: to_json(row),
            ttl_ms: Some(u64::try_from(ttl.num_milliseconds()).unwrap_or(0).max(1)),
        })
    }

    /// Insert or replace `row`.
    pub fn upsert<R: Row>(row: &R) -> Self {
        Self(Kind::UpsertRow {
            table: R::TABLE,
            row: to_json(row),
        })
    }

    /// Overwrite the columns `set` serializes to in the row of `R` with primary key `pk`.
    pub fn update<R: Row>(pk: &[&str], set: &impl Serialize) -> Self {
        Self(Kind::UpdateRow {
            table: R::TABLE,
            pk: keys(pk),
            set: to_json(set),
        })
    }

    pub fn delete_row<R: Row>(pk: &[&str]) -> Self {
        Self(Kind::DeleteRow {
            table: R::TABLE,
            pk: keys(pk),
        })
    }

    /// Delete the rows of `R` whose primary key starts with `eq`.
    pub fn delete_rows<R: Row>(eq: &[&str]) -> Self {
        Self(Kind::DeleteRows {
            table: R::TABLE,
            selection: KeyPrefix { eq: keys(eq) },
        })
    }

    /// Append `records` to `stream`, creating it if needed.
    pub fn append<T: Serialize>(stream: impl Into<String>, records: &[T]) -> Self {
        Self(Kind::Append {
            stream: stream.into(),
            records: records
                .iter()
                .map(|r| Record { json: to_json(r) })
                .collect(),
            create: true,
        })
    }

    /// Read `stream`'s records from `from_seq` on, a page at a time.
    pub fn read_stream(stream: impl Into<String>, from_seq: u64) -> Self {
        Self(Kind::ReadStream {
            stream: stream.into(),
            from_seq,
            limit: PAGE,
        })
    }

    /// The op reading the page after `result`, if `result` was not the last.
    pub fn next_page(&self, result: &OpResult) -> Option<Self> {
        let mut next = self.clone();
        match &mut next.0 {
            Kind::Scan { cursor, .. } | Kind::QueryRows { cursor, .. } => {
                *cursor = Some(result.next_cursor.clone()?);
            }
            Kind::ReadStream { from_seq, .. } => {
                if result.done != Some(false) {
                    return None;
                }
                *from_seq = result.next_seq?;
            }
            _ => return None,
        }
        Some(next)
    }
}

/// The result of one read op.
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub struct OpResult {
    found: bool,
    value: Option<String>,
    row: Option<Json>,
    rows: Vec<Json>,
    items: Vec<KvItem>,
    records: Vec<StreamRecord>,
    next_cursor: Option<String>,
    next_seq: Option<u64>,
    done: Option<bool>,
}

/// A key and its value, from a scan.
#[derive(Debug, Deserialize)]
pub struct KvItem {
    pub key: String,
    #[serde(default)]
    pub value: Option<String>,
}

#[derive(Debug, Deserialize)]
struct StreamRecord {
    json: Json,
}

fn decode<T: DeserializeOwned>(what: &str, v: Json) -> Result<T> {
    serde_json::from_value(v).map_err(|e| Error::BadRow(format!("{what}: {e}")))
}

impl OpResult {
    /// A `get`'s value, if the key exists.
    pub fn value(&self) -> Option<&str> {
        self.value.as_deref()
    }

    /// A `get_row`'s row, if it exists.
    pub fn row<R: Row>(self) -> Result<Option<R>> {
        match self.row.filter(|_| self.found) {
            Some(r) => decode(R::TABLE, r).map(Some),
            None => Ok(None),
        }
    }

    /// A `query`'s page of rows.
    pub fn rows<R: Row>(self) -> Result<Vec<R>> {
        self.rows.into_iter().map(|r| decode(R::TABLE, r)).collect()
    }

    /// A `scan`'s page of keys and values.
    pub fn items(self) -> Vec<KvItem> {
        self.items
    }

    /// A `read_stream`'s page of record bodies.
    pub fn records<T: DeserializeOwned>(self) -> Result<Vec<T>> {
        self.records
            .into_iter()
            .map(|r| decode("stream record", r.json))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Serialize, Deserialize, Debug, PartialEq)]
    struct Thing {
        id: String,
        n: u32,
    }

    impl Row for Thing {
        const TABLE: &'static str = "things";
    }

    #[test]
    fn ops_serialize_as_the_api_takes_them() {
        let row = Thing {
            id: "a".into(),
            n: 1,
        };
        let cases = [
            (
                Op::insert_expiring(&row, TimeDelta::seconds(2)),
                r#"{"op":"insert_row","table":"things","row":{"id":"a","n":1},"ttl_ms":2000}"#,
            ),
            (
                Op::delete_rows::<Thing>(&["a"]),
                r#"{"op":"delete_rows","table":"things","where":{"eq":["a"]}}"#,
            ),
            (
                Op::append("s", &[&row]),
                r#"{"op":"append","stream":"s","records":[{"json":{"id":"a","n":1}}],"create":true}"#,
            ),
            (
                Op::query::<Thing>(&[]),
                r#"{"op":"query_rows","table":"things","eq":[],"limit":1000}"#,
            ),
        ];
        for (op, want) in cases {
            assert_eq!(serde_json::to_string(&op).unwrap(), want);
        }
    }

    #[test]
    fn next_page_follows_cursors_and_stream_positions() {
        let page = |j: &str| serde_json::from_str::<OpResult>(j).unwrap();
        let q = Op::query::<Thing>(&["a"]);
        let next = q.next_page(&page(r#"{"rows":[],"next_cursor":"c1"}"#));
        let json = |op: Option<Op>| serde_json::to_value(op.expect("next page")).unwrap();
        assert_eq!(json(next)["cursor"], "c1");
        assert!(
            q.next_page(&page(r#"{"rows":[],"next_cursor":null}"#))
                .is_none()
        );

        let s = Op::read_stream("s", 0);
        let next = s.next_page(&page(r#"{"records":[],"next_seq":7,"done":false}"#));
        assert_eq!(json(next)["from_seq"], 7);
        assert!(
            s.next_page(&page(r#"{"records":[],"next_seq":7,"done":true}"#))
                .is_none()
        );
    }

    #[test]
    fn rows_decode_into_their_type_and_name_the_table_when_malformed() {
        let r: OpResult = serde_json::from_str(r#"{"found":true,"row":{"id":"a","n":2}}"#).unwrap();
        assert_eq!(
            r.row::<Thing>().unwrap(),
            Some(Thing {
                id: "a".into(),
                n: 2
            })
        );
        let missing: OpResult = serde_json::from_str(r#"{"found":false}"#).unwrap();
        assert_eq!(missing.row::<Thing>().unwrap(), None);
        let bad: OpResult = serde_json::from_str(r#"{"rows":[{"id":"a"}]}"#).unwrap();
        let e = bad.rows::<Thing>().unwrap_err().to_string();
        assert!(
            e.contains("things") && e.contains("missing field `n`"),
            "{e}"
        );
    }
}
