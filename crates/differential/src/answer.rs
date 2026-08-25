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
    /// Whether the column is UNSIGNED, which the binary protocol needs in
    /// order to read the bytes back as the number that was sent.
    pub unsigned: bool,
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

/// Which of MySQL's two protocols a statement is asked through.
///
/// They are not interchangeable and only one of them was being tested. The
/// text protocol sends every value as a string, so a column's declared type
/// affects nothing about the bytes; the binary protocol encodes *by* that
/// type, so a column announced as one byte is sent as one byte whatever it
/// holds. Every truncation this engine has had lives in the second one, and a
/// harness that only spoke the first could not see any of them.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Protocol {
    Text,
    Binary,
}

impl Protocol {
    pub fn name(self) -> &'static str {
        match self {
            Protocol::Text => "text",
            Protocol::Binary => "binary",
        }
    }
}

impl Answer {
    /// Runs one statement and records what came back.
    ///
    /// Values are compared as text because that is what the text protocol puts
    /// on the wire, and because rendering them any other way would compare
    /// this tool's own conversions rather than the servers'. The binary
    /// protocol's values are rendered by the client library, which is the same
    /// library on both sides, so the comparison still holds.
    pub fn of(conn: &mut Conn, sql: &str, protocol: Protocol) -> Self {
        match protocol {
            Protocol::Text => Self::from_text(conn, sql),
            Protocol::Binary => Self::from_binary(conn, sql),
        }
    }

    /// The binary protocol, which is what a prepared statement uses.
    ///
    /// A statement the server will not prepare is not a divergence in itself —
    /// `USE`, and DDL on some servers — so it answers the same way the text
    /// protocol did rather than inventing a failure.
    fn from_binary(conn: &mut Conn, sql: &str) -> Self {
        let statement = match conn.prep(sql) {
            Ok(statement) => statement,
            Err(mysql::Error::MySqlError(e)) => {
                return Answer::Failed {
                    code: e.code,
                    state: e.state,
                }
            }
            Err(other) => {
                return Answer::Failed {
                    code: 0,
                    state: format!("{other}").chars().take(60).collect(),
                }
            }
        };
        match conn.exec_iter(&statement, ()) {
            Err(mysql::Error::MySqlError(e)) => Answer::Failed {
                code: e.code,
                state: e.state,
            },
            Err(other) => Answer::Failed {
                code: 0,
                state: format!("{other}").chars().take(60).collect(),
            },
            Ok(result) => Self::collect(result),
        }
    }

    fn from_text(conn: &mut Conn, sql: &str) -> Self {
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
            Ok(result) => Self::collect(result),
        }
    }

    /// Reads a result set, however it was asked for.
    fn collect<'a, T: mysql::prelude::Protocol>(
        result: mysql::QueryResult<'a, 'a, 'a, T>,
    ) -> Answer {
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
                unsigned: c.flags().contains(ColumnFlags::UNSIGNED_FLAG),
            })
            .collect();

        let mut rows = Vec::new();
        for row in result {
            match row {
                Ok(row) => rows.push(render(&row)),
                // A server error before any row is a refusal, not a
                // truncation. MySQL raises some of them *per row* rather than
                // while planning — `SELECT iu - 100` over an INT UNSIGNED
                // describes its column, then sends 1690 — so the refusal
                // arrives after `columns()` has already been read, where this
                // engine refuses before answering at all. A client sees an
                // error with the same code either way, which is the fact this
                // harness is comparing.
                //
                // Only before any row, and deliberately: a statement that
                // sent rows and *then* refused is a partial answer, and
                // calling that a refusal would hide a real difference in what
                // reached the client.
                Err(mysql::Error::MySqlError(e)) if rows.is_empty() => {
                    return Answer::Failed {
                        code: e.code,
                        state: e.state,
                    }
                }
                // Reported rather than swallowed: stopping here and returning
                // the short list would be read as the server having sent fewer
                // rows, and the actual reason would be gone.
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
