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
///
/// `widths`, `value_kind` and `places` are off for the few cases that ask
/// something one of those three would drown out; see `Case::widths`,
/// `Case::value_kind` and `Case::places`.
pub fn differences(
    reference: &Answer,
    engine: &Answer,
    widths: bool,
    value_kind: bool,
    places: bool,
) -> Vec<Difference> {
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
                // Asked of every column, computed ones included, and the
                // only part of the definition that is: a column of characters
                // described as bytes is handed back to the caller as a Buffer
                // it then cannot convert. `GROUP_CONCAT` went out binary here
                // for exactly one review cycle, and nothing else in this file
                // could see it — a computed column is out of scope for the
                // width and the flags, and the charset *id* differs on every
                // text column for a reason that is not the engine's.
                if value_kind && a.binary_values != b.binary_values {
                    let describe = |binary| if binary { "bytes" } else { "characters" }.to_owned();
                    note(
                        &format!("values of column {i}"),
                        describe(a.binary_values),
                        describe(b.binary_values),
                    );
                }
                // Everything below this line is asked only of a column
                // that came out of a table, which is what a non-empty
                // `org_table` on MySQL's side means. A computed column's shape
                // is `Item::max_length` and the collation arithmetic around
                // it — a width per function, per operator and per literal —
                // and this engine models none of it as a *width*, so asking
                // would report the same missing feature 560 times and drown
                // everything else. (#0110 models enough of it to pick a
                // materialised column's type code, which is compared
                // everywhere and is a different question.)
                //
                // The scope is read off the *reference*, deliberately: an
                // engine that wrongly claimed a table for a computed column
                // could otherwise exclude itself from the comparison.
                //
                // What that leaves unasked is written down rather than left to
                // be discovered: see `crates/differential/README.md`, which
                // carries the count and the reason for each.
                if a.org_table.is_empty() {
                    // The two bits a computed column is still held to. The
                    // first is what a generated client models a field's
                    // optionality from; the second is what the binary protocol
                    // reads the bytes back through, so a column without it
                    // arrives negative.
                    const MODELLED: u16 = NOT_NULL | UNSIGNED;
                    if a.flags & MODELLED != b.flags & MODELLED {
                        note(
                            &format!("flags of column {i}"),
                            flags(a.flags & MODELLED),
                            flags(b.flags & MODELLED),
                        );
                    }
                    continue;
                }
                // The flags word, less two groups of bits, each left out for
                // its own reason and each counted so that leaving it out is a
                // debt rather than a blind spot:
                //
                // The bits that describe the column's role in the schema —
                // PRI_KEY, UNIQUE_KEY, MULTIPLE_KEY, PART_KEY,
                // AUTO_INCREMENT, NO_DEFAULT_VALUE, ON_UPDATE_NOW. This
                // engine sends none of them: a primary key arrives with no
                // PRI_KEY, a NOT NULL column with no default with no
                // NO_DEFAULT_VALUE. 224 divergences over 81 statements, and
                // its own gap — it needs the catalog carried out to the wire,
                // where everything else here needs only the type.
                //
                // NUM, which is not a fact about the column at all: MySQL sets
                // it on a field a temporary table produced, so the same column
                // carries it under `GROUP BY` and not without one. Measured on
                // 8.3, and not only in principle: `SELECT many FROM E` reports
                // an `ENUM` and `SELECT many FROM E GROUP BY many` the same
                // column as `ENUM|NUM`. Comparing it would compare the two
                // servers' query plans.
                //
                // What is left is compared: NOT_NULL and UNSIGNED, which were
                // the only two bits this harness read for its first year;
                // BINARY, BLOB, ZEROFILL and TIMESTAMP, which it read none of
                // — so a `DATE` column described without BINARY, and a
                // materialised NULL described as anything at all, compared
                // equal to MySQL; and ENUM and SET, which #0087 took out of
                // this list. Those two were excluded and counted here for as
                // long as the engine set neither: 50 divergences over 24
                // statements, which is what removing the names reported before
                // anything was fixed.
                const UNMODELLED: u16 = PRI_KEY
                    | UNIQUE_KEY
                    | MULTIPLE_KEY
                    | AUTO_INCREMENT
                    | NO_DEFAULT_VALUE
                    | ON_UPDATE_NOW
                    | PART_KEY
                    | NUM;
                let (fa, fb) = (a.flags & !UNMODELLED, b.flags & !UNMODELLED);
                if fa != fb {
                    note(&format!("flags of column {i}"), flags(fa), flags(fb));
                }
                // The declared width. MySQL takes it from the declaration —
                // `VARCHAR(10)` is 40 under utf8mb4, four bytes to a character
                // — and this engine sent 4096 for every string column and 20
                // for every number until this line was written.
                if widths && a.length != b.length {
                    note(
                        &format!("length of column {i}"),
                        a.length.to_string(),
                        b.length.to_string(),
                    );
                }
                // The declared places. MySQL reports a DECIMAL's scale and a
                // temporal's fractional digits here, and this engine reported
                // 0 for every column there was until #0111 — 1148 divergences
                // over 260 statements when that was measured. An ORM maps a
                // column to a type from this, so a money column described with
                // no places is read as a whole number.
                //
                // Asked of a declared column only, alongside the width and for
                // the same reason: an *expression's* places are
                // `Item::decimals` arithmetic, and #0111 models the exact and
                // the string families of it but not the approximate one — a
                // `FLOAT(M,D)` propagates its scale through `+`, `SUM` and
                // `AVG` — nor a temporal's through arithmetic. What that
                // leaves is counted in `README.md`.
                if places && a.decimals != b.decimals {
                    note(
                        &format!("decimals of column {i}"),
                        a.decimals.to_string(),
                        b.decimals.to_string(),
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

/// The bits of the flags word this file names.
///
/// Spelled out here rather than taken from the client library: the harness
/// compares some of them and deliberately does not compare others, and that
/// decision reads better against a list than against `ColumnFlags::` names
/// half of which have no constant in the crate anyway.
const NOT_NULL: u16 = 0x0001;
const PRI_KEY: u16 = 0x0002;
const UNIQUE_KEY: u16 = 0x0004;
const MULTIPLE_KEY: u16 = 0x0008;
const UNSIGNED: u16 = 0x0020;
const AUTO_INCREMENT: u16 = 0x0200;
const NO_DEFAULT_VALUE: u16 = 0x1000;
const ON_UPDATE_NOW: u16 = 0x2000;
const PART_KEY: u16 = 0x4000;
const NUM: u16 = 0x8000;

/// The flags word as the names of the bits that are set.
///
/// Named rather than hexadecimal because the report is read by somebody
/// deciding whether a divergence matters, and `0x0081` against `0x0001` does
/// not say "the engine left off BINARY". All sixteen bits are named, so a set
/// bit cannot fall out of the rendering and leave two different words reading
/// as one string in a report that still fails the run.
fn flags(word: u16) -> String {
    const NAMES: [(u16, &str); 16] = [
        (0x0001, "NOT_NULL"),
        (0x0002, "PRI_KEY"),
        (0x0004, "UNIQUE_KEY"),
        (0x0008, "MULTIPLE_KEY"),
        (0x0010, "BLOB"),
        (0x0020, "UNSIGNED"),
        (0x0040, "ZEROFILL"),
        (0x0080, "BINARY"),
        (0x0100, "ENUM"),
        (0x0200, "AUTO_INCREMENT"),
        (0x0400, "TIMESTAMP"),
        (0x0800, "SET"),
        (0x1000, "NO_DEFAULT_VALUE"),
        (0x2000, "ON_UPDATE_NOW"),
        (0x4000, "PART_KEY"),
        (0x8000, "NUM"),
    ];
    let named: Vec<&str> = NAMES
        .iter()
        .filter(|(bit, _)| word & bit != 0)
        .map(|(_, name)| *name)
        .collect();
    if named.is_empty() {
        return "none".to_owned();
    }
    named.join("|")
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::answer::Column;
    use mysql::consts::ColumnType;

    /// A `VARCHAR(10)` column as each server described it, differing only in
    /// the fields named.
    fn column(flags: u16, length: u32) -> Column {
        Column {
            name: "v".to_owned(),
            ty: ColumnType::MYSQL_TYPE_VAR_STRING,
            flags,
            length,
            decimals: 0,
            org_table: "t".to_owned(),
            binary_values: false,
        }
    }

    /// The same column as each server described its places.
    fn with_decimals(decimals: u8) -> Column {
        Column {
            decimals,
            ..column(0, 40)
        }
    }

    fn rows(columns: Vec<Column>) -> Answer {
        Answer::Rows {
            columns,
            rows: vec![vec![Some("x".to_owned())]],
        }
    }

    fn what(reference: &Answer, engine: &Answer, widths: bool) -> Vec<String> {
        differences(reference, engine, widths, true, true)
            .into_iter()
            .map(|d| format!("{}: mysql {} engine {}", d.what, d.reference, d.engine))
            .collect()
    }

    /// The bug this comparison was added for, reproduced: MySQL sends a
    /// `VARCHAR(10)` as 40 bytes wide and the engine sent 4096 for every
    /// string column there was.
    ///
    /// The second half is the point. The old comparison read the name, the
    /// type code, NOT_NULL and UNSIGNED, and those four agree here — so the
    /// harness reported these two answers as identical, every run, for a year.
    #[test]
    fn a_width_the_old_comparison_could_not_see_is_reported() {
        let mysql = rows(vec![column(0, 40)]);
        let engine = rows(vec![column(0, 4096)]);

        assert_eq!(
            what(&mysql, &engine, true),
            vec!["length of column 0: mysql 40 engine 4096"]
        );

        // What the harness used to compare, spelled out: identical.
        let (Answer::Rows { columns: a, .. }, Answer::Rows { columns: b, .. }) = (&mysql, &engine)
        else {
            unreachable!()
        };
        assert_eq!(a[0].name, b[0].name);
        assert_eq!(a[0].ty, b[0].ty);
        assert_eq!(
            a[0].flags & (NOT_NULL | UNSIGNED),
            b[0].flags & (NOT_NULL | UNSIGNED)
        );
    }

    /// The other half of #0126: a `DATE` column is binary on 8.3 and this
    /// engine set the flag for nothing at all. Same shape — the two bits the
    /// harness used to read are equal on both sides.
    #[test]
    fn a_flag_outside_the_two_that_were_compared_is_reported() {
        const BINARY: u16 = 0x0080;
        let mysql = rows(vec![column(NOT_NULL | BINARY, 40)]);
        let engine = rows(vec![column(NOT_NULL, 40)]);
        assert_eq!(
            what(&mysql, &engine, true),
            vec!["flags of column 0: mysql NOT_NULL|BINARY engine NOT_NULL"]
        );
    }

    /// The bits deliberately left out stay out, and each for its own reason:
    /// the key flags belong to the gap that needs the catalog on the wire, and
    /// NUM to MySQL's choice of plan.
    #[test]
    fn the_unmodelled_flags_are_not_reported() {
        for unmodelled in [NUM, PRI_KEY, PART_KEY, NO_DEFAULT_VALUE, AUTO_INCREMENT] {
            let mysql = rows(vec![column(NOT_NULL | unmodelled, 40)]);
            let engine = rows(vec![column(NOT_NULL, 40)]);
            assert!(
                what(&mysql, &engine, true).is_empty(),
                "{unmodelled:#06x} was compared"
            );
        }
    }

    /// #0087's half of the harness: ENUM and SET are compared, having been in
    /// the list above until the engine sent them.
    ///
    /// Both directions, because the flag is the only thing on the wire that
    /// tells either from the `CHAR(n)` they share a type code with — an engine
    /// that set ENUM on every string would be as wrong as one that set it on
    /// none, and only the second of those was ever the bug.
    #[test]
    fn the_enum_and_set_flags_are_reported() {
        const ENUM: u16 = 0x0100;
        const SET: u16 = 0x0800;
        for (bit, name) in [(ENUM, "ENUM"), (SET, "SET")] {
            let mysql = rows(vec![column(NOT_NULL | bit, 40)]);
            let engine = rows(vec![column(NOT_NULL, 40)]);
            assert_eq!(
                what(&mysql, &engine, true),
                vec![format!(
                    "flags of column 0: mysql NOT_NULL|{name} engine NOT_NULL"
                )]
            );
            assert_eq!(
                what(&engine, &mysql, true),
                vec![format!(
                    "flags of column 0: mysql NOT_NULL engine NOT_NULL|{name}"
                )]
            );
        }
    }

    /// A computed column is held to the two bits the engine models and to
    /// nothing else, because MySQL's width for one is an arithmetic this
    /// engine does not have. The scope is read off the reference alone, so an
    /// engine claiming a table for a computed column cannot exempt itself.
    #[test]
    fn a_computed_column_is_asked_only_what_the_engine_models() {
        let computed = |flags, length| Column {
            org_table: String::new(),
            ..column(flags, length)
        };
        let mysql = rows(vec![computed(0, 2)]);
        let engine = rows(vec![computed(0, 4096)]);
        assert!(what(&mysql, &engine, true).is_empty());

        // The engine claiming a table does not bring the width into scope.
        let engine = rows(vec![column(0, 4096)]);
        assert!(what(&mysql, &engine, true).is_empty());

        // NOT_NULL and UNSIGNED are still asked of it.
        let engine = rows(vec![computed(NOT_NULL, 2)]);
        assert_eq!(
            what(&mysql, &engine, true),
            vec!["flags of column 0: mysql none engine NOT_NULL"]
        );
    }

    /// The one part of the definition a computed column *is* asked about:
    /// whether its values are bytes. A `GROUP_CONCAT` described binary reaches
    /// the caller as a Buffer it cannot convert, and this is the only line in
    /// the file that would catch one — the width and the rest of the flags are
    /// out of scope for a column with no table behind it.
    #[test]
    fn a_computed_column_is_still_asked_whether_its_values_are_bytes() {
        let computed = |binary_values| Column {
            org_table: String::new(),
            binary_values,
            ..column(0, 40)
        };
        let mysql = rows(vec![computed(false)]);
        let engine = rows(vec![computed(true)]);
        assert_eq!(
            what(&mysql, &engine, true),
            vec!["values of column 0: mysql characters engine bytes"]
        );
        // And a case may decline it, which eleven do — a string function over
        // a NULL literal is binary on 8.3 by its operand's collation.
        assert!(differences(&mysql, &engine, true, false, true).is_empty());
    }

    /// #0111's half of the harness: a column's places are compared, and they
    /// are the one field of the definition nothing else here can see — the
    /// name, the type code, the flags and the width are all equal between
    /// these two, which is exactly how a `DECIMAL(10,2)` described with no
    /// scale stayed green for a year.
    #[test]
    fn the_places_a_column_shows_are_reported() {
        let mysql = rows(vec![with_decimals(2)]);
        let engine = rows(vec![with_decimals(0)]);
        assert_eq!(
            what(&mysql, &engine, true),
            vec!["decimals of column 0: mysql 2 engine 0"]
        );
        // And `NOT_FIXED_DEC`, which is a real answer rather than a missing
        // one: an approximate column with no declared scale reports 31.
        let mysql = rows(vec![with_decimals(31)]);
        assert_eq!(
            what(&mysql, &engine, true),
            vec!["decimals of column 0: mysql 31 engine 0"]
        );
    }

    /// A case that opts out of widths keeps everything else. The handful that
    /// do are the views whose column is an expression; see `Case::widths`.
    #[test]
    fn a_case_can_decline_the_width_and_nothing_else() {
        const BINARY: u16 = 0x0080;
        let mysql = rows(vec![column(BINARY, 8)]);
        let engine = rows(vec![column(0, 2048)]);
        assert_eq!(
            what(&mysql, &engine, false),
            vec!["flags of column 0: mysql BINARY engine none"]
        );
    }
}
