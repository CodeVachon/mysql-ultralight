//! What one statement produced, in a form the two servers can be compared in.

use mysql::consts::{ColumnFlags, ColumnType};
use mysql::prelude::Queryable;
use mysql::{Conn, Row, Value};

/// One result column, as the server described it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Column {
    pub name: String,
    pub ty: ColumnType,
    pub nullable: bool,
}

/// Everything a statement told the client.
///
/// An error is an answer too, and the interesting one: two servers agreeing on
/// the rows but disagreeing on which statements are legal is the divergence a
/// test suite trips over first.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Answer {
    Rows {
        columns: Vec<Column>,
        rows: Vec<Vec<Option<String>>>,
    },
    Affected(u64),
    Failed {
        code: u16,
        state: String,
    },
    /// The result set could not be read to the end.
    Truncated {
        after: usize,
        why: String,
    },
}

impl Answer {
    /// Runs one statement and records what came back.
    ///
    /// Values are compared as text because that is what both servers put on
    /// the wire for a text-protocol query, and rendering them any other way
    /// would compare this tool's own conversions rather than the servers'.
    pub fn of(conn: &mut Conn, sql: &str) -> Self {
        match conn.query_iter(sql) {
            Err(mysql::Error::MySqlError(e)) => Answer::Failed {
                code: e.code,
                state: e.state,
            },
            // A transport failure is not an answer about the SQL, so it is
            // reported as its own thing rather than folded into a code.
            Err(other) => Answer::Failed {
                code: 0,
                state: format!("{other}").chars().take(60).collect(),
            },
            Ok(result) => {
                let described = result.columns();
                let described = described.as_ref();
                // No columns means the statement changed something rather than
                // returning anything, and the interesting number is how much.
                if described.is_empty() {
                    return Answer::Affected(result.affected_rows());
                }
                let columns = described
                    .iter()
                    .map(|c| Column {
                        name: c.name_str().into_owned(),
                        ty: c.column_type(),
                        nullable: !c.flags().contains(ColumnFlags::NOT_NULL_FLAG),
                    })
                    .collect();

                let mut rows = Vec::new();
                for row in result {
                    match row {
                        Ok(row) => rows.push(render(&row)),
                        // Reported rather than swallowed: stopping here and
                        // returning the short list would be read as the server
                        // having sent fewer rows, and the actual reason —
                        // which is this tool's problem, not the server's —
                        // would be gone.
                        Err(e) => {
                            return Answer::Truncated {
                                after: rows.len(),
                                why: format!("{e}").chars().take(80).collect(),
                            }
                        }
                    }
                }
                Answer::Rows { columns, rows }
            }
        }
    }

    pub fn summary(&self) -> String {
        match self {
            Answer::Failed { code, state } => format!("error {code} ({state})"),
            Answer::Truncated { after, why } => {
                format!("unreadable after {after} row(s): {why}")
            }
            Answer::Affected(n) => format!("{n} affected"),
            Answer::Rows { columns, rows } => {
                format!("{} column(s), {} row(s)", columns.len(), rows.len())
            }
        }
    }
}

fn render(row: &Row) -> Vec<Option<String>> {
    (0..row.len())
        .map(|i| match row.as_ref(i) {
            None | Some(Value::NULL) => None,
            Some(Value::Bytes(b)) => Some(String::from_utf8_lossy(b).into_owned()),
            Some(other) => Some(format!("{other:?}")),
        })
        .collect()
}
