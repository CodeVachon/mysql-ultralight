//! The divergences that are already known about.
//!
//! A harness that reports fifty differences every run teaches the reader to
//! ignore it. The baseline is the set already accounted for — recorded in a
//! file, shrinking as each one is fixed — so a run is quiet until something
//! *new* appears, which is the only signal worth interrupting anyone for.
//!
//! It is deliberately a flat text file rather than annotations in the corpus:
//! the corpus should read as "what MySQL does", and the accounting of what
//! this engine has not caught up on yet belongs beside it, not inside it.

use std::collections::BTreeSet;
use std::path::Path;

/// One accepted divergence, as `group :: statement :: what`.
pub type Entry = String;

pub fn entry(group: &str, sql: &str, what: &str) -> Entry {
    // The statement is flattened so a reformatted case still matches, and so
    // the file stays greppable by the SQL a reader remembers writing.
    let flat = sql.split_whitespace().collect::<Vec<_>>().join(" ");
    format!("{group} :: {flat} :: {what}")
}

pub fn load(path: &Path) -> BTreeSet<Entry> {
    let Ok(text) = std::fs::read_to_string(path) else {
        return BTreeSet::new();
    };
    text.lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .map(str::to_owned)
        .collect()
}

pub fn write(path: &Path, entries: &BTreeSet<Entry>) -> std::io::Result<()> {
    let mut out = String::from(
        "# Divergences between MySQL and mysql-ultralight that are already known.\n\
         #\n\
         # Each line is `group :: statement :: what`. Regenerate with\n\
         # `./scripts/differential.sh --bless`, and expect the file to get\n\
         # shorter: a line disappearing means a difference was fixed, and the\n\
         # harness fails if a line here no longer differs so the file cannot\n\
         # quietly describe a state that has passed.\n\n",
    );
    for entry in entries {
        out.push_str(entry);
        out.push('\n');
    }
    std::fs::write(path, out)
}
