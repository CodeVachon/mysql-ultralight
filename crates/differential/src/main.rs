//! Runs identical SQL against MySQL and against mysql-ultralight, and reports
//! where the two disagree.
//!
//! This engine's job is to answer the way MySQL answers, and every bug worth
//! the name in it has been a place where it did not. Most were caught by a
//! reviewer who happened to know MySQL; this asks MySQL instead.
//!
//!     differential --reference mysql://root@127.0.0.1:33306 \
//!                  --engine    mysql://root@127.0.0.1:33307
//!
//! Not part of `scripts/check.sh`: it needs a MySQL to compare against, which
//! a checkout does not have. `scripts/differential.sh` starts one in Docker
//! and tears it down again.

mod answer;
mod baseline;
mod cases;
mod compare;

use std::collections::BTreeSet;
use std::path::PathBuf;

use answer::Answer;
use cases::GROUPS;
use mysql::prelude::Queryable;
use mysql::Conn;

const DATABASE: &str = "differential";

struct Options {
    reference: String,
    engine: String,
    baseline: PathBuf,
    bless: bool,
}

fn main() {
    let options = parse_args();
    let known = baseline::load(&options.baseline);

    let mut reference = connect(&options.reference, "reference");
    let mut engine = connect(&options.engine, "engine");
    check_identities(&mut reference, &mut engine);

    // A baseline line is keyed by group, statement and what differed, so two
    // cases in one group with the same statement would share a key and the
    // second would be invisible. Refused up front rather than deduplicated:
    // the corpus is written by hand, and a case that silently does not count
    // is exactly the kind of thing this tool exists to stop.
    if let Err(duplicate) = unique_statements() {
        eprintln!("{duplicate}");
        std::process::exit(2);
    }

    let mut seen: BTreeSet<String> = BTreeSet::new();
    let mut fresh: Vec<String> = Vec::new();
    let mut compared = 0usize;
    let mut broken_setup = 0usize;

    for group in GROUPS {
        // A fresh database per group, so one group's tables cannot answer
        // another's query and a failed setup cannot look like a divergence.
        for conn in [&mut reference, &mut engine] {
            reset(conn, group.name);
        }
        for statement in group.setup {
            for (which, conn) in [("reference", &mut reference), ("engine", &mut engine)] {
                if let Answer::Failed { code, state } = Answer::of(conn, statement) {
                    // Counted, not just printed. A setup that fails on *both*
                    // servers leaves every case in the group failing
                    // identically, which compares equal and reports a group
                    // that tested nothing as a group that agreed.
                    broken_setup += 1;
                    println!("SETUP FAILED on {which} ({code} {state})\n    {statement}");
                }
            }
        }

        for case in group.cases {
            compared += 1;
            let r = Answer::of(&mut reference, case.sql);
            let e = Answer::of(&mut engine, case.sql);

            for d in compare::differences(&r, &e) {
                let entry = baseline::entry(group.name, case.sql, &d.what);
                seen.insert(entry.clone());
                if !known.contains(&entry) {
                    fresh.push(format!(
                        "{}\n    {}\n    {:<22} mysql {:<26} engine {}",
                        group.name,
                        flat(case.sql),
                        d.what,
                        d.reference,
                        d.engine
                    ));
                }
            }
        }
    }

    // A baseline line that no longer matches is a claim about the engine that
    // has stopped being true, and a stale claim is worse than none.
    let healed: Vec<&String> = known.difference(&seen).collect();

    if options.bless {
        // Blessing a run whose setup failed writes the wreckage in as the
        // accepted state, and the file is then a record of a broken run that
        // nobody will read again.
        if broken_setup > 0 {
            eprintln!(
                "\nrefusing to bless: {broken_setup} setup statement(s) failed, so what \
                 the cases measured is not what they describe"
            );
            std::process::exit(2);
        }
        if let Err(e) = baseline::write(&options.baseline, &seen) {
            eprintln!("could not write {}: {e}", options.baseline.display());
            std::process::exit(2);
        }
        println!(
            "blessed {} divergence(s) across {compared} statement(s) into {}",
            seen.len(),
            options.baseline.display()
        );
        return;
    }

    for report in &fresh {
        println!("\nNEW  {report}");
    }
    for entry in &healed {
        println!("\nFIXED  {entry}\n    no longer differs; remove it from the baseline");
    }

    // Counted by asking the baseline rather than by subtracting: `fresh` is a
    // list of reports and `seen` a set of keys, and with no baseline at all
    // the subtraction underflowed to 18446744073709551615.
    let recognised = seen.iter().filter(|e| known.contains(*e)).count();
    println!(
        "\n{compared} statement(s) · {} divergence(s), {recognised} of them known · {} new · {} fixed",
        seen.len(),
        fresh.len(),
        healed.len()
    );
    if broken_setup > 0 {
        println!(
            "{broken_setup} setup statement(s) failed; the run above measured less than it looks"
        );
    }

    if !fresh.is_empty() || !healed.is_empty() || broken_setup > 0 {
        std::process::exit(1);
    }
}

fn parse_args() -> Options {
    let mut reference = None;
    let mut engine = None;
    let mut baseline = None;
    let mut bless = false;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--reference" => reference = args.next(),
            "--engine" => engine = args.next(),
            "--baseline" => baseline = args.next(),
            "--bless" => bless = true,
            other => {
                eprintln!("unknown argument `{other}`");
                std::process::exit(2);
            }
        }
    }
    let (Some(reference), Some(engine)) = (reference, engine) else {
        eprintln!(
            "usage: differential --reference <url> --engine <url> \
             [--baseline <path>] [--bless]"
        );
        std::process::exit(2);
    };
    Options {
        reference,
        engine,
        baseline: baseline
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("crates/differential/baseline.txt")),
        bless,
    }
}

fn unique_statements() -> Result<(), String> {
    for group in GROUPS {
        let mut seen: BTreeSet<String> = BTreeSet::new();
        for case in group.cases {
            if !seen.insert(flat(case.sql)) {
                return Err(format!(
                    "`{}` asks the same statement twice, which the baseline cannot tell \
                     apart:\n    {}\nGive one of them a distinguishing alias.",
                    group.name,
                    flat(case.sql)
                ));
            }
        }
    }
    Ok(())
}

fn flat(sql: &str) -> String {
    sql.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn connect(url: &str, which: &str) -> Conn {
    match Conn::new(url) {
        Ok(conn) => conn,
        Err(e) => {
            eprintln!("cannot reach the {which} server at {url}: {e}");
            std::process::exit(2);
        }
    }
}

/// Refuses to run unless the two ends really are what they claim.
///
/// Pointing `--engine` at the reference by mistake — a port already in use, a
/// copied command line — compares MySQL with itself, which agrees on
/// everything and reports the whole baseline as fixed. That looks like
/// spectacular progress and means nothing, so it is made impossible rather
/// than warned about.
fn check_identities(reference: &mut Conn, engine: &mut Conn) {
    let comment = |conn: &mut Conn, which: &str| -> String {
        conn.query_first::<String, _>("SELECT @@version_comment")
            .ok()
            .flatten()
            .unwrap_or_else(|| {
                eprintln!("the {which} server would not say what it is");
                std::process::exit(2);
            })
    };
    const MARK: &str = "mysql-ultralight";
    if !comment(engine, "engine").contains(MARK) {
        eprintln!("--engine is not mysql-ultralight; it reports itself as a real MySQL");
        std::process::exit(2);
    }
    if comment(reference, "reference").contains(MARK) {
        eprintln!("--reference is mysql-ultralight; there is nothing to compare against");
        std::process::exit(2);
    }
}

fn reset(conn: &mut Conn, group: &str) {
    for statement in [
        format!("DROP DATABASE IF EXISTS `{DATABASE}`"),
        format!("CREATE DATABASE `{DATABASE}`"),
        format!("USE `{DATABASE}`"),
    ] {
        if let Err(e) = conn.query_drop(&statement) {
            eprintln!("could not prepare a database for `{group}`: {e}");
            std::process::exit(2);
        }
    }
}
