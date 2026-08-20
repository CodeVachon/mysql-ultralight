//! Where two answers differ, and whether that difference was expected.

use crate::answer::Answer;

/// One way the two servers disagreed.
#[derive(Debug)]
pub struct Difference {
    pub what: String,
    pub reference: String,
    pub engine: String,
}

/// Every difference between two answers, in the order they matter.
///
/// Ordered deliberately: a statement one server refused and the other answered
/// is a bigger fact than a column type, and reporting the type mismatch first
/// buries it.
pub fn differences(reference: &Answer, engine: &Answer) -> Vec<Difference> {
    let mut found = Vec::new();
    let mut note = |what: &str, r: String, e: String| {
        found.push(Difference {
            what: what.to_owned(),
            reference: r,
            engine: e,
        })
    };

    match (reference, engine) {
        (Answer::Failed { code: a, .. }, Answer::Failed { code: b, .. }) => {
            if a != b {
                note("error code", a.to_string(), b.to_string());
            }
        }
        (Answer::Affected(a), Answer::Affected(b)) => {
            if a != b {
                note("affected rows", a.to_string(), b.to_string());
            }
        }
        (
            Answer::Rows {
                columns: ca,
                rows: ra,
            },
            Answer::Rows {
                columns: cb,
                rows: rb,
            },
        ) => {
            if ca.len() != cb.len() {
                note("column count", ca.len().to_string(), cb.len().to_string());
                return found;
            }
            for (i, (a, b)) in ca.iter().zip(cb).enumerate() {
                // Keyed by position, not by name. Two columns of one statement
                // can differ the same way — a query with two arrow operators
                // has two `column name` mismatches — and a key they share
                // means fixing one of them shows up as no change at all.
                if a.name != b.name {
                    note(
                        &format!("name of column {i}"),
                        a.name.clone(),
                        b.name.clone(),
                    );
                }
                // Checked even when the names disagree: the columns are still
                // positionally the same one, and skipping them here hides
                // divergences that would then surface as *new* the day the
                // name is fixed — a run failing because something got better.
                if a.ty != b.ty {
                    note(
                        &format!("type of column {i}"),
                        format!("{:?}", a.ty),
                        format!("{:?}", b.ty),
                    );
                }
                if a.nullable != b.nullable {
                    note(
                        &format!("nullability of column {i}"),
                        nullable(a.nullable),
                        nullable(b.nullable),
                    );
                }
                // Not cosmetic: the binary protocol reads the bytes back as a
                // signed number unless this says otherwise, so an UNSIGNED
                // column without the flag comes out negative.
                if a.unsigned != b.unsigned {
                    note(
                        &format!("signedness of column {i}"),
                        signed(a.unsigned),
                        signed(b.unsigned),
                    );
                }
            }
            if ra.len() != rb.len() {
                note("row count", ra.len().to_string(), rb.len().to_string());
                return found;
            }
            for (i, (a, b)) in ra.iter().zip(rb).enumerate() {
                if a != b {
                    note(&format!("row {i}"), cells(a), cells(b));
                }
            }
        }
        // Different kinds of answer entirely: one refused, the other did not.
        (a, b) => note("outcome", a.summary(), b.summary()),
    }

    found
}

fn signed(unsigned: bool) -> String {
    if unsigned { "UNSIGNED" } else { "signed" }.to_owned()
}

fn nullable(v: bool) -> String {
    if v { "nullable" } else { "NOT NULL" }.to_owned()
}

fn cells(row: &[Option<String>]) -> String {
    row.iter()
        .map(|c| match c {
            None => "NULL".to_owned(),
            Some(v) => format!("{v:?}"),
        })
        .collect::<Vec<_>>()
        .join(", ")
}
