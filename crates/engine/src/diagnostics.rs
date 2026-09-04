//! The diagnostics area: the conditions the last statement raised.
//!
//! MySQL keeps one of these per connection and `SHOW WARNINGS` reads it. The
//! rules below are measured on 8.3 rather than remembered, because every one
//! of them decides whether a client sees a condition at all:
//!
//! * **A statement replaces the area with its own conditions.** Not appends —
//!   replaces, and it replaces it with *nothing* when it raised nothing. A
//!   `SELECT 1` between a truncating INSERT and a `SHOW WARNINGS` is why the
//!   first probe written for this returned an empty list and read like an
//!   engine that agreed.
//! * **The readers are the exception.** `SHOW WARNINGS`, `SHOW ERRORS` and
//!   `SHOW COUNT(*) WARNINGS` leave the area exactly as they found it, so the
//!   same list can be read twice.
//! * **`@@warning_count` is not one of them.** `SELECT @@warning_count` reads
//!   the count and then clears the area like any other statement: measured,
//!   three reads in a row are 1, 0, 0.
//! * **A statement that fails leaves its error behind as a condition**, at
//!   level Error, after whatever it raised before it failed. Under
//!   `STRICT_ALL_TABLES` an INSERT whose first row trims trailing spaces and
//!   whose second is too long leaves a Note 1265 *and* an Error 1406.
//! * **`max_error_count` caps the list and not the count.** With it at 2 and
//!   five offending rows, `SHOW WARNINGS` returns two rows and both
//!   `@@warning_count` and the OK packet say 5. At 0 nothing is kept at all,
//!   the statement's own error included.
//!
//! Conditions are raised several calls below the only place that holds the
//! session — a value is written into a column by `SqlType::store`, an alias is
//! cut by `name::alias` — so they land in a thread-local for the length of one
//! statement, exactly as the SQL modes in `exec.rs` are read from one. What
//! that scope collected is then moved onto the session by `Engine::execute`.

use std::cell::RefCell;
use std::collections::HashSet;

/// How serious a condition is, in the three words `SHOW WARNINGS` prints.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Level {
    /// Something MySQL did that lost nothing a client wrote. A `VARCHAR(3)`
    /// given `'abc  '` keeps every non-space character, and 8.3 calls that a
    /// Note where the same column given `'abcdef'` gets a Warning.
    Note,
    Warning,
    /// The statement's own error, which is a condition too: after a failed
    /// statement `SHOW WARNINGS` shows it.
    Error,
}

impl Level {
    pub fn as_str(self) -> &'static str {
        match self {
            Level::Note => "Note",
            Level::Warning => "Warning",
            Level::Error => "Error",
        }
    }
}

/// One row of `SHOW WARNINGS`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Condition {
    pub level: Level,
    pub code: u16,
    pub message: String,
}

/// The conditions one statement raised, and how many it raised.
///
/// The two are not the same number: `max_error_count` caps what is *kept*
/// while the count keeps counting, which is what `SHOW WARNINGS` returning two
/// rows beside an OK packet saying 5 means.
#[derive(Debug, Clone, Default)]
pub struct Diagnostics {
    conditions: Vec<Condition>,
    count: u64,
    errors: u64,
}

impl Diagnostics {
    /// Every condition kept, in the order they were raised.
    pub fn conditions(&self) -> &[Condition] {
        &self.conditions
    }

    /// Only the Error-level ones, which is what `SHOW ERRORS` shows.
    pub fn errors(&self) -> impl Iterator<Item = &Condition> {
        self.conditions.iter().filter(|c| c.level == Level::Error)
    }

    /// `@@warning_count`: every condition raised, at every level, capped by
    /// nothing.
    pub fn count(&self) -> u64 {
        self.count
    }

    /// `@@error_count`: the Error-level ones.
    pub fn error_count(&self) -> u64 {
        self.errors
    }

    /// The number an OK or EOF packet carries.
    ///
    /// Saturating rather than wrapping: the field is sixteen bits and a
    /// statement can raise more conditions than that, and a client reading
    /// `65536 as u16` would be told a statement that warned about everything
    /// warned about nothing.
    pub fn wire_count(&self) -> u16 {
        u16::try_from(self.count).unwrap_or(u16::MAX)
    }

    fn push(&mut self, limit: usize, condition: Condition) {
        self.count += 1;
        if condition.level == Level::Error {
            self.errors += 1;
        }
        // The cap applies to the statement's own error as well, measured: with
        // `max_error_count = 0` a failing statement leaves `SHOW WARNINGS` and
        // `SHOW ERRORS` both empty.
        if self.conditions.len() < limit {
            self.conditions.push(condition);
        }
    }
}

/// What one statement is raising, while it runs.
struct Collector {
    area: Diagnostics,
    limit: usize,
    /// Whether this statement reads the area instead of replacing it.
    keep: bool,
    /// Where an alias has already been warned about, so the cut is reported
    /// once per alias and not once per reader of it.
    ///
    /// `name::alias` is a pure function called from six places — planning a
    /// projection, describing it, resolving an ORDER BY against it — and every
    /// one of them cuts the same name again. Keyed on the identifier's
    /// position in the statement rather than on its text, because two aliases
    /// spelled identically are two conditions on 8.3: `SELECT 1 AS <300>, 2 AS
    /// <the same 300>` answers `@@warning_count` 2.
    warned: HashSet<(u64, u64)>,
}

thread_local! {
    /// `None` outside a statement, which is what `describe` and any other
    /// caller that is not executing runs under: a condition raised there has
    /// no statement to belong to and is dropped rather than leaking into the
    /// next one.
    static AREA: RefCell<Option<Collector>> = const { RefCell::new(None) };
}

/// Collects the conditions of one statement, for the length of a scope.
///
/// Restored through a guard rather than a trailing assignment for the reason
/// `WriteScope` and `Source` give: a statement that fails halfway must not
/// leave the next one collecting into its area.
pub(crate) struct Collecting(Option<Collector>);

impl Collecting {
    /// Opens an area that keeps at most `limit` conditions, which is the
    /// session's `max_error_count`.
    pub(crate) fn open(limit: usize) -> Self {
        Self(AREA.with(|a| {
            a.borrow_mut().replace(Collector {
                area: Diagnostics::default(),
                limit,
                keep: false,
                warned: HashSet::new(),
            })
        }))
    }

    /// What has been raised so far, and whether the statement asked for the
    /// previous area to be left alone.
    pub(crate) fn taken(&self) -> (Diagnostics, bool) {
        AREA.with(|a| {
            a.borrow()
                .as_ref()
                .map(|c| (c.area.clone(), c.keep))
                .unwrap_or_default()
        })
    }
}

impl Drop for Collecting {
    fn drop(&mut self) {
        AREA.with(|a| *a.borrow_mut() = self.0.take());
    }
}

/// Records a condition against the statement being run.
///
/// A no-op with no statement in scope, which is the same choice the SQL-mode
/// readers in `exec.rs` make: there is nowhere for it to go and inventing one
/// would attach it to whatever ran next.
pub(crate) fn raise(level: Level, code: u16, message: impl Into<String>) {
    AREA.with(|a| {
        if let Some(collector) = a.borrow_mut().as_mut() {
            let limit = collector.limit;
            collector.area.push(
                limit,
                Condition {
                    level,
                    code,
                    message: message.into(),
                },
            );
        }
    });
}

/// [`raise`], but at most once for each `at` within one statement.
///
/// For a condition raised by a *pure function* that the executor calls more
/// than once for the same piece of the statement. See `Collector::warned`.
pub(crate) fn raise_once(
    at: (u64, u64),
    level: Level,
    code: u16,
    message: impl FnOnce() -> String,
) {
    let first = AREA.with(|a| {
        a.borrow_mut()
            .as_mut()
            .map(|collector| collector.warned.insert(at))
            .unwrap_or(false)
    });
    if first {
        raise(level, code, message());
    }
}

/// Says that this statement reads the diagnostics area rather than replacing
/// it, so what it found is still there for the next reader.
///
/// `SHOW WARNINGS`, `SHOW ERRORS` and the two `SHOW COUNT(*)` forms, and
/// nothing else: `SELECT @@warning_count` reads the count and still clears.
pub(crate) fn keep_area() {
    AREA.with(|a| {
        if let Some(collector) = a.borrow_mut().as_mut() {
            collector.keep = true;
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nothing_is_collected_without_a_statement_in_scope() {
        // The `describe` path runs here. A condition raised with no statement
        // to belong to must be dropped rather than kept for whatever runs
        // next, which is the one way a thread-local like this produces a wrong
        // answer in a *later* statement.
        raise(Level::Warning, 1265, "no statement");
        let collecting = Collecting::open(1024);
        assert_eq!(collecting.taken().0.count(), 0);
    }

    #[test]
    fn the_cap_holds_the_list_and_not_the_count() {
        let collecting = Collecting::open(2);
        for i in 0..5 {
            raise(Level::Warning, 1265, format!("row {i}"));
        }
        let (area, keep) = collecting.taken();
        assert_eq!(area.count(), 5);
        assert_eq!(area.conditions().len(), 2);
        assert!(!keep);
    }

    /// With `max_error_count` at 0, 8.3 keeps nothing at all — the statement's
    /// own error included.
    #[test]
    fn a_cap_of_zero_keeps_nothing() {
        let collecting = Collecting::open(0);
        raise(Level::Error, 1146, "Table 'x' doesn't exist");
        let (area, _) = collecting.taken();
        assert_eq!(area.count(), 1);
        assert_eq!(area.error_count(), 1);
        assert!(area.conditions().is_empty());
    }

    #[test]
    fn an_alias_is_warned_about_once_per_position() {
        let collecting = Collecting::open(1024);
        // The same alias, read six times by six callers.
        for _ in 0..6 {
            raise_once((1, 10), Level::Warning, 1466, || "first".to_owned());
        }
        // A second alias spelled the same way, somewhere else in the text.
        raise_once((1, 400), Level::Warning, 1466, || "second".to_owned());
        let (area, _) = collecting.taken();
        assert_eq!(area.count(), 2);
        assert_eq!(area.conditions()[0].message, "first");
        assert_eq!(area.conditions()[1].message, "second");
    }

    /// A nested scope must not swallow what the outer one collected: the
    /// guard restores rather than clearing.
    #[test]
    fn a_scope_restores_the_one_it_replaced() {
        let outer = Collecting::open(1024);
        raise(Level::Warning, 1265, "outer");
        {
            let inner = Collecting::open(1024);
            raise(Level::Warning, 1264, "inner");
            assert_eq!(inner.taken().0.count(), 1);
        }
        let (area, _) = outer.taken();
        assert_eq!(area.count(), 1);
        assert_eq!(area.conditions()[0].message, "outer");
    }

    #[test]
    fn a_reader_says_so() {
        let collecting = Collecting::open(1024);
        keep_area();
        assert!(collecting.taken().1);
    }
}
