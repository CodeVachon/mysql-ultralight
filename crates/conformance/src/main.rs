//! Extracts the SQL conformance surface of a target codebase.
//!
//! Answers two questions at once:
//!
//! 1. What must the engine implement? Every statement is inventoried, so
//!    "done" becomes countable rather than a judgement call.
//! 2. Can sqlparser-rs be the parser? Every statement is fed to it, and the
//!    statements it rejects are reported verbatim. Those failures are the
//!    real deliverable — they are the gap between an off-the-shelf parser and
//!    the SQL this codebase actually contains.

mod scan;
mod sources;
mod split;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use sources::{Origin, Source};
use sqlparser::dialect::MySqlDialect;
use sqlparser::parser::Parser;

/// Statement-initial keywords. Anything else is a fragment rather than a
/// standalone statement.
const STATEMENT_KEYWORDS: &[&str] = &[
    "SELECT",
    "INSERT",
    "UPDATE",
    "DELETE",
    "REPLACE",
    "CREATE",
    "ALTER",
    "DROP",
    "TRUNCATE",
    "RENAME",
    "SET",
    "SHOW",
    "DESCRIBE",
    "DESC",
    "EXPLAIN",
    "USE",
    "GRANT",
    "REVOKE",
    "LOCK",
    "UNLOCK",
    "START",
    "BEGIN",
    "COMMIT",
    "ROLLBACK",
    "SAVEPOINT",
    "PREPARE",
    "EXECUTE",
    "DEALLOCATE",
    "CALL",
    "WITH",
    "FLUSH",
    "ANALYZE",
    "OPTIMIZE",
    "REPAIR",
    "CHECK",
    "DO",
    "HANDLER",
    "LOAD",
    "VALUES",
    "TABLE",
];

#[derive(Default)]
struct Totals {
    statements: usize,
    parsed: usize,
    failed: usize,
    /// SQL that is not a standalone statement: a `Prisma.sql` fragment such as
    /// `cc.areaId IN (?)`, spliced into a larger query at runtime.
    fragments: usize,
    /// Statements whose only problem is a placeholder standing where an
    /// identifier or subquery belongs. Prisma interpolates those positions at
    /// runtime, so the original SQL cannot be recovered statically.
    unrecoverable: usize,
    features: BTreeMap<String, usize>,
    calls: BTreeMap<String, usize>,
    types: BTreeMap<String, usize>,
    kinds: BTreeMap<String, usize>,
}

struct Failure {
    origin: Origin,
    path: PathBuf,
    line: usize,
    error: String,
    excerpt: String,
}

/// Reads the value after a flag, refusing a missing one. Silently ignoring it
/// makes `conformance /repo --json` scan everything, write nothing, and exit 0.
fn require_value(args: &[String], flag_at: usize) -> PathBuf {
    match args.get(flag_at + 1) {
        Some(v) if !v.starts_with("--") => PathBuf::from(v),
        _ => {
            eprintln!("{} requires a path", args[flag_at]);
            std::process::exit(2);
        }
    }
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.is_empty() || args[0] == "-h" || args[0] == "--help" {
        eprintln!(
            "conformance <target-repo-root> [--json <path>] [--md <path>]\n\n\
             Scans <root>/apps/api/prisma/migrations, <root>/provisions, and the raw SQL\n\
             embedded in TypeScript under <root>/apps and <root>/packages."
        );
        std::process::exit(2);
    }

    let root = PathBuf::from(&args[0]);
    let mut json_out = None;
    let mut md_out = None;
    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--json" => {
                json_out = Some(require_value(&args, i));
                i += 2;
            }
            "--md" => {
                md_out = Some(require_value(&args, i));
                i += 2;
            }
            other => {
                eprintln!("unknown option: {other}");
                std::process::exit(2);
            }
        }
    }

    let mut all: Vec<Source> = Vec::new();
    all.extend(sources::load_sql_files(
        &root.join("apps/api/prisma/migrations"),
        Origin::Migrations,
    ));
    all.extend(sources::load_sql_files(
        &root.join("provisions"),
        Origin::Provisions,
    ));
    for sub in ["apps", "packages"] {
        all.extend(sources::load_raw_queries(&root.join(sub)));
    }

    if all.is_empty() {
        eprintln!("no SQL found under {}", root.display());
        std::process::exit(1);
    }

    let dialect = MySqlDialect {};
    let mut per_origin: BTreeMap<Origin, Totals> = BTreeMap::new();
    let mut failures: Vec<Failure> = Vec::new();
    let mut seen_files: BTreeMap<Origin, std::collections::BTreeSet<PathBuf>> = BTreeMap::new();

    for source in &all {
        // Application code yields one Source per template, so files must be
        // counted by distinct path rather than by Source.
        seen_files
            .entry(source.origin)
            .or_default()
            .insert(source.path.clone());
        let totals = per_origin.entry(source.origin).or_default();

        for statement in split::split(&source.sql) {
            let normalized = split::normalize(&statement.sql);
            if normalized.is_empty() {
                continue;
            }
            let upper = normalized.to_uppercase();
            // String contents are data, not code: a row value containing
            // "GROUP BY" is not a GROUP BY clause, and an apostrophe inside one
            // would desynchronise the type scanner's quote pairing.
            let masked = split::mask_string_literals(&upper);

            // Fragments are inventoried for their constructs but excluded
            // from the parse rate: judging a parser on `AND x = ?` would say
            // nothing about the parser.
            let is_fragment = !STATEMENT_KEYWORDS.iter().any(|kw| {
                upper.strip_prefix(kw).is_some_and(|rest| {
                    // `COMMIT` alone is a statement, and so is `SELECT(1)`.
                    // Requiring a following space classified both as fragments
                    // and dropped them from the parse rate entirely.
                    rest.is_empty() || !rest.starts_with(|c: char| c.is_alphanumeric() || c == '_')
                })
            });

            if is_fragment {
                totals.fragments += 1;
            } else {
                totals.statements += 1;
            }
            scan::count_features(&masked, &mut totals.features);

            // Column definitions live only in these two statements. Scanning
            // everything would read `UPDATE \`Foo\` SET ...` as a column of
            // type SET.
            if masked.starts_with("CREATE TABLE") || masked.starts_with("ALTER TABLE") {
                scan::count_types(&masked, &mut totals.types);
            }

            let mut calls = BTreeMap::new();
            scan::count_calls(&masked, &mut calls);
            for (name, n) in calls {
                // A type name in call position is part of a column definition
                // or a CAST, already counted by count_types.
                if scan::DATA_TYPES.contains(&name.as_str()) {
                    continue;
                }
                *totals.calls.entry(name).or_insert(0) += n;
            }

            if is_fragment {
                continue;
            }

            match Parser::parse_sql(&dialect, &statement.sql) {
                Ok(parsed) => {
                    totals.parsed += 1;
                    for stmt in parsed {
                        *totals.kinds.entry(statement_kind(&stmt)).or_insert(0) += 1;
                    }
                }
                Err(e) => {
                    let error = e.to_string();
                    // `found: ?` means a placeholder sits where the grammar
                    // wants an identifier — an artifact of recovering SQL from
                    // a template, not a gap in the parser.
                    // A placeholder where the grammar wants an identifier is
                    // an artifact of recovering SQL from a template, so it says
                    // nothing about the parser. Restricted to application code:
                    // in a migration a `found: ?` error is a real gap, and
                    // swallowing it there would hide it.
                    if source.origin == Origin::ApplicationCode && error.contains("found: ?") {
                        totals.unrecoverable += 1;
                        totals.statements -= 1;
                        continue;
                    }
                    totals.failed += 1;
                    failures.push(Failure {
                        origin: source.origin,
                        path: source.path.clone(),
                        line: statement.line + source.line_offset - 1,
                        error,
                        excerpt: excerpt(&normalized),
                    });
                }
            }
        }
    }

    let files: BTreeMap<Origin, usize> = seen_files
        .iter()
        .map(|(origin, paths)| (*origin, paths.len()))
        .collect();

    report(
        &root,
        &per_origin,
        &files,
        &failures,
        json_out.as_deref(),
        md_out.as_deref(),
    );
}

/// The AST variant name, used as a coarse statement classification.
///
/// Taken from the Debug representation rather than an exhaustive match: the
/// classification is for a report, and this keeps the tool from breaking every
/// time sqlparser reshapes an enum variant.
fn statement_kind(stmt: &sqlparser::ast::Statement) -> String {
    let debug = format!("{stmt:?}");
    debug
        .split(['(', '{', ' '])
        .next()
        .unwrap_or("Unknown")
        .to_owned()
}

/// Collapses a parser error into a signature, so the same gap reported against
/// a hundred different identifiers groups into one line.
fn error_signature(error: &str) -> String {
    let trimmed = match error.find(" at Line:") {
        Some(at) => &error[..at],
        None => error,
    };
    // Replace the backtick-quoted identifier the parser happened to choke on.
    let mut out = String::with_capacity(trimmed.len());
    let mut in_ident = false;
    for ch in trimmed.chars() {
        match ch {
            '`' if !in_ident => {
                in_ident = true;
                out.push_str("`<identifier>");
            }
            '`' => {
                in_ident = false;
                out.push('`');
            }
            c if !in_ident => out.push(c),
            _ => {}
        }
    }
    out
}

/// Groups failures by signature, most common first, with one example each.
fn cluster(failures: &[Failure]) -> Vec<(String, usize, &Failure)> {
    let mut order: Vec<String> = Vec::new();
    let mut counts: BTreeMap<String, usize> = BTreeMap::new();
    let mut example: BTreeMap<String, &Failure> = BTreeMap::new();

    for f in failures {
        let sig = error_signature(&f.error);
        if !counts.contains_key(&sig) {
            order.push(sig.clone());
            example.insert(sig.clone(), f);
        }
        *counts.entry(sig).or_insert(0) += 1;
    }

    let mut out: Vec<(String, usize, &Failure)> = order
        .into_iter()
        .map(|sig| {
            let n = counts[&sig];
            let ex = example[&sig];
            (sig, n, ex)
        })
        .collect();
    // Stable sort, so equally common gaps keep first-seen order and the
    // generated report does not churn between runs.
    out.sort_by_key(|entry| std::cmp::Reverse(entry.1));
    out
}

fn excerpt(normalized: &str) -> String {
    const MAX: usize = 160;
    if normalized.len() <= MAX {
        return normalized.to_owned();
    }
    let mut end = MAX;
    while end > 0 && !normalized.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &normalized[..end])
}

fn report(
    root: &Path,
    per_origin: &BTreeMap<Origin, Totals>,
    files: &BTreeMap<Origin, usize>,
    failures: &[Failure],
    json_out: Option<&Path>,
    md_out: Option<&Path>,
) {
    let total_statements: usize = per_origin.values().map(|t| t.statements).sum();
    let total_parsed: usize = per_origin.values().map(|t| t.parsed).sum();
    let rate = |ok: usize, all: usize| {
        if all == 0 {
            0.0
        } else {
            ok as f64 * 100.0 / all as f64
        }
    };

    println!("Conformance surface of {}", root.display());
    println!();
    println!(
        "{:<18} {:>7} {:>11} {:>8} {:>7} {:>10} {:>14} {:>7}",
        "source", "files", "statements", "parsed", "failed", "fragments", "unrecoverable", "rate"
    );
    for (origin, totals) in per_origin {
        println!(
            "{:<18} {:>7} {:>11} {:>8} {:>7} {:>10} {:>14} {:>6.1}%",
            origin.label(),
            files.get(origin).copied().unwrap_or(0),
            totals.statements,
            totals.parsed,
            totals.failed,
            totals.fragments,
            totals.unrecoverable,
            rate(totals.parsed, totals.statements)
        );
    }
    println!(
        "{:<18} {:>7} {:>11} {:>8} {:>7} {:>10} {:>14} {:>6.1}%",
        "TOTAL",
        files.values().sum::<usize>(),
        total_statements,
        total_parsed,
        per_origin.values().map(|t| t.failed).sum::<usize>(),
        per_origin.values().map(|t| t.fragments).sum::<usize>(),
        per_origin.values().map(|t| t.unrecoverable).sum::<usize>(),
        rate(total_parsed, total_statements)
    );
    println!(
        "\n`fragments` are Prisma.sql pieces spliced together at runtime; `unrecoverable` are\n\
         templates whose interpolation lands where an identifier belongs. Neither is a\n\
         standalone statement, so the parse rate is computed without them."
    );

    let merged = merge(per_origin);
    println!(
        "\nsqlparser-rs rejected {} statement(s), in {} distinct gap(s):",
        failures.len(),
        cluster(failures).len()
    );
    for (signature, count, example) in cluster(failures) {
        println!("  {count:>4}  {signature}");
        println!(
            "        e.g. {}",
            example.excerpt.chars().take(110).collect::<String>()
        );
    }

    if let Some(path) = json_out {
        let json = build_json(root, per_origin, files, failures, &merged);
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        match std::fs::write(
            path,
            serde_json::to_string_pretty(&json).unwrap_or_default(),
        ) {
            Ok(()) => println!("\nwrote {}", path.display()),
            Err(e) => eprintln!("could not write {}: {e}", path.display()),
        }
    }

    if let Some(path) = md_out {
        let md = build_markdown(root, per_origin, files, failures, &merged);
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        match std::fs::write(path, md) {
            Ok(()) => println!("wrote {}", path.display()),
            Err(e) => eprintln!("could not write {}: {e}", path.display()),
        }
    }
}

struct Merged {
    features: BTreeMap<String, usize>,
    calls: BTreeMap<String, usize>,
    types: BTreeMap<String, usize>,
    kinds: BTreeMap<String, usize>,
}

fn merge(per_origin: &BTreeMap<Origin, Totals>) -> Merged {
    let mut m = Merged {
        features: BTreeMap::new(),
        calls: BTreeMap::new(),
        types: BTreeMap::new(),
        kinds: BTreeMap::new(),
    };
    for totals in per_origin.values() {
        for (k, v) in &totals.features {
            *m.features.entry(k.clone()).or_insert(0) += v;
        }
        for (k, v) in &totals.calls {
            *m.calls.entry(k.clone()).or_insert(0) += v;
        }
        for (k, v) in &totals.types {
            *m.types.entry(k.clone()).or_insert(0) += v;
        }
        for (k, v) in &totals.kinds {
            *m.kinds.entry(k.clone()).or_insert(0) += v;
        }
    }
    m
}

fn by_count(map: &BTreeMap<String, usize>) -> Vec<(&String, &usize)> {
    let mut v: Vec<_> = map.iter().collect();
    v.sort_by(|a, b| b.1.cmp(a.1).then(a.0.cmp(b.0)));
    v
}

fn build_json(
    root: &Path,
    per_origin: &BTreeMap<Origin, Totals>,
    files: &BTreeMap<Origin, usize>,
    failures: &[Failure],
    merged: &Merged,
) -> serde_json::Value {
    use serde_json::{json, Map, Value};

    let to_obj = |m: &BTreeMap<String, usize>| {
        let mut o = Map::new();
        for (k, v) in m {
            o.insert(k.clone(), json!(v));
        }
        Value::Object(o)
    };

    let sources: Vec<Value> = per_origin
        .iter()
        .map(|(origin, t)| {
            json!({
                "source": origin.label(),
                "files": files.get(origin).copied().unwrap_or(0),
                "statements": t.statements,
                "parsed": t.parsed,
                "failed": t.failed,
                "fragments": t.fragments,
                "unrecoverable": t.unrecoverable,
            })
        })
        .collect();

    let failed: Vec<Value> = failures
        .iter()
        .map(|f| {
            json!({
                "source": f.origin.label(),
                "file": relative(&f.path, root),
                "line": f.line,
                "error": f.error,
                "sql": f.excerpt,
            })
        })
        .collect();

    json!({
        "sources": sources,
        "statement_kinds": to_obj(&merged.kinds),
        "features": to_obj(&merged.features),
        "functions": to_obj(&merged.calls),
        "data_types": to_obj(&merged.types),
        "parse_gaps": cluster(failures)
            .into_iter()
            .map(|(signature, count, example)| json!({
                "error": signature,
                "statements": count,
                "example": example.excerpt,
            }))
            .collect::<Vec<_>>(),
        "parse_failures": failed,
    })
}

/// Wraps text in a Markdown code span that survives inner backticks.
///
/// MySQL identifiers are backtick-quoted, so a naive single-backtick span
/// terminates at the first one and the rest of the cell renders as prose.
fn code_span(text: &str) -> String {
    let longest_run = text.split(|c| c != '`').map(str::len).max().unwrap_or(0);
    let fence = "`".repeat(longest_run + 1);
    // A span whose content starts or ends with a backtick needs padding.
    let pad = if text.starts_with('`') || text.ends_with('`') {
        " "
    } else {
        ""
    };
    format!("{fence}{pad}{text}{pad}{fence}")
}

/// A path relative to the scanned root.
///
/// Every Prisma migration file is named `migration.sql`, so a bare file name
/// makes all 170 rows of the failure list identical and unusable.
fn relative(path: &Path, root: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .to_string_lossy()
        .into_owned()
}

fn build_markdown(
    root: &Path,
    per_origin: &BTreeMap<Origin, Totals>,
    files: &BTreeMap<Origin, usize>,
    failures: &[Failure],
    merged: &Merged,
) -> String {
    let mut s = String::new();
    let total: usize = per_origin.values().map(|t| t.statements).sum();
    let parsed: usize = per_origin.values().map(|t| t.parsed).sum();
    let pct = if total == 0 {
        0.0
    } else {
        parsed as f64 * 100.0 / total as f64
    };

    s.push_str("# Conformance surface\n\n");
    s.push_str(&format!(
        "Generated by `cargo run -p mysql-ultralight-conformance -- {}`.\n\n",
        root.display()
    ));
    s.push_str("Every statement below was extracted from real SQL and fed to sqlparser-rs with the MySQL dialect. Counts come from comment-stripped, whitespace-collapsed text, so statements wrapping across lines are counted correctly.\n\n");

    s.push_str("## Coverage\n\n");
    s.push_str("`fragments` counts `Prisma.sql` pieces such as ``cc.areaId IN (?)``, which are spliced into a larger query at runtime and are not standalone statements. `unrecoverable` counts templates whose interpolation lands where an identifier belongs, so the original SQL cannot be recovered statically. Both are inventoried for their constructs but excluded from the parse rate; the rate is therefore computed over the `statements` column only.\n\n");
    s.push_str("| source | files | statements | parsed by sqlparser-rs | failed | fragments | unrecoverable |\n");
    s.push_str("|---|---|---|---|---|---|---|\n");
    for (origin, t) in per_origin {
        let r = if t.statements == 0 {
            0.0
        } else {
            t.parsed as f64 * 100.0 / t.statements as f64
        };
        s.push_str(&format!(
            "| {} | {} | {} | {} ({:.1}%) | {} | {} | {} |\n",
            origin.label(),
            files.get(origin).copied().unwrap_or(0),
            t.statements,
            t.parsed,
            r,
            t.failed,
            t.fragments,
            t.unrecoverable
        ));
    }
    s.push_str(&format!(
        "| **total** | {} | {} | {} ({:.1}%) | {} | {} | {} |\n\n",
        files.values().sum::<usize>(),
        total,
        parsed,
        pct,
        per_origin.values().map(|t| t.failed).sum::<usize>(),
        per_origin.values().map(|t| t.fragments).sum::<usize>(),
        per_origin.values().map(|t| t.unrecoverable).sum::<usize>()
    ));

    let section = |s: &mut String, title: &str, map: &BTreeMap<String, usize>, limit: usize| {
        s.push_str(&format!(
            "## {title}\n\n| item | occurrences |\n|---|---|\n"
        ));
        for (name, count) in by_count(map).into_iter().take(limit) {
            s.push_str(&format!("| {} | {} |\n", code_span(name), count));
        }
        s.push('\n');
    };

    section(&mut s, "Statement kinds", &merged.kinds, 100);
    section(&mut s, "Constructs", &merged.features, 100);
    section(&mut s, "Data types", &merged.types, 100);
    section(&mut s, "Functions", &merged.calls, 200);

    s.push_str(&format!(
        "## Parser gaps ({} statement(s) rejected)\n\n",
        failures.len()
    ));
    s.push_str("Each row is one gap between sqlparser-rs with the MySQL dialect and the SQL this codebase contains. Closing these is the Phase 3 work list.\n\n");
    s.push_str("| statements | parser error | example |\n|---|---|---|\n");
    for (signature, count, example) in cluster(failures) {
        s.push_str(&format!(
            "| {} | {} | {} |\n",
            count,
            signature.replace('|', "\\|"),
            code_span(&example.excerpt).replace('|', "\\|")
        ));
    }
    s.push('\n');

    s.push_str("<details>\n<summary>Every rejected statement</summary>\n\n");
    for f in failures {
        s.push_str(&format!(
            "- `{}:{}` ({}) — {}\n  ```sql\n  {}\n  ```\n",
            relative(&f.path, root),
            f.line,
            f.origin.label(),
            f.error,
            f.excerpt
        ));
    }
    s.push_str("\n</details>\n");

    s
}
