//! Runs identical SQL against MySQL and against mysql-ultralight, and reports
//! where the two disagree.
//!
//! This engine's job is to answer the way MySQL answers, and every bug worth
//! the name in it has been a place where it did not. Most were caught by a
//! reviewer who happened to know MySQL; this asks MySQL instead.
//!
//!     differential --reference mysql://root@127.0.0.1:33306 \
//!                  --engine    mysql://root@127.0.0.1:33307 \
//!                  --engine-instance 3f1c0a9b6d2e4517
//!
//! The instance id is the one the engine printed on its `ready` line, and it
//! is required: a run that cannot name the engine it means is a run that can
//! silently compare MySQL against a leftover process from an earlier one.
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
use answer::Protocol;
use cases::GROUPS;
use mysql::prelude::Queryable;
use mysql::Conn;

const DATABASE: &str = "differential";

/// The mode both servers start every group in.
///
/// MySQL 8.3's own default, and the engine's; a group that changes it changes
/// it only for itself, because the two connections outlive every group and a
/// session setting left behind would silently reinterpret the next group's SQL.
const DEFAULT_SQL_MODE: &str = "ONLY_FULL_GROUP_BY,STRICT_TRANS_TABLES,NO_ZERO_IN_DATE,\
                                NO_ZERO_DATE,ERROR_FOR_DIVISION_BY_ZERO,NO_ENGINE_SUBSTITUTION";

struct Options {
    reference: String,
    engine: String,
    /// The `instance=` field from the ready line of the engine the caller
    /// started, which that engine reads back as `@@ultralight_instance`.
    engine_instance: String,
    baseline: PathBuf,
    bless: bool,
}

fn main() {
    let options = parse_args();
    let known = baseline::load(&options.baseline);

    let mut reference = connect(&options.reference, "reference");
    let mut engine = connect(&options.engine, "engine");
    check_identities(&mut reference, &mut engine, &options.engine_instance);

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
                if let Answer::Failed { code, state } = Answer::of(conn, statement, Protocol::Text)
                {
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
            // Both protocols, because they are not the same question. The text
            // one sends every value as a string, so a column's declared type
            // changes nothing about the bytes. The binary one encodes *by*
            // that type — a column announced as one byte is sent as one byte
            // whatever it holds — and that is where narrowing a type code can
            // turn 300 into 44. Asking only the first missed a whole class.
            for protocol in [Protocol::Text, Protocol::Binary] {
                compared += 1;
                let r = Answer::of(&mut reference, case.sql, protocol);
                let e = Answer::of(&mut engine, case.sql, protocol);

                for d in compare::differences(&r, &e) {
                    let what = format!("{} [{}]", d.what, protocol.name());
                    let entry = baseline::entry(group.name, case.sql, &what);
                    seen.insert(entry.clone());
                    if !known.contains(&entry) {
                        fresh.push(format!(
                            "{}\n    {}\n    {:<28} mysql {:<26} engine {}",
                            group.name,
                            flat(case.sql),
                            what,
                            d.reference,
                            d.engine
                        ));
                    }
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
    let mut engine_instance = None;
    let mut baseline = None;
    let mut bless = false;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--reference" => reference = args.next(),
            "--engine" => engine = args.next(),
            "--engine-instance" => engine_instance = args.next(),
            "--baseline" => baseline = args.next(),
            "--bless" => bless = true,
            other => {
                eprintln!("unknown argument `{other}`");
                std::process::exit(2);
            }
        }
    }
    let (Some(reference), Some(engine), Some(engine_instance)) =
        (reference, engine, engine_instance)
    else {
        eprintln!(
            "usage: differential --reference <url> --engine <url> \
             --engine-instance <id> [--baseline <path>] [--bless]\n\
             \n\
             <id> is the instance= field the engine printed on its ready line. \
             It is required so that a run cannot compare against whatever \
             happens to be listening; use scripts/differential.sh, which \
             starts the engine and passes its id through."
        );
        std::process::exit(2);
    };
    Options {
        reference,
        engine,
        engine_instance,
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
///
/// Being *a* mysql-ultralight is not enough, which is the second half of the
/// same lesson: a leftover server from an earlier run passes that test and
/// answers from a previous build, and the run reports zero divergences about
/// code nobody is running. So the peer must also be the process named by
/// `--engine-instance`.
fn check_identities(reference: &mut Conn, engine: &mut Conn, expected_instance: &str) {
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

    // The error is carried rather than discarded: a build too old to know the
    // variable refuses it with 1193 and a connection that has gone away fails
    // here too, and telling an operator their build is stale when the server
    // was killed underneath them sends them hunting for a process that does
    // not exist. Either way the run stops — there is no answer here that means
    // "carry on".
    let reported = engine
        .query_first::<String, _>("SELECT @@ultralight_instance")
        .map_err(|e| e.to_string());
    if let Some(complaint) = wrong_instance(expected_instance, reported) {
        eprintln!("{complaint}");
        std::process::exit(2);
    }
}

/// Says why the engine on the other end is not the one that was started, or
/// `None` when it is.
///
/// Split out from the connecting so the decision can be tested: this is the
/// check that stands between a green run and a green run about a binary from
/// twenty minutes ago.
fn wrong_instance(expected: &str, reported: Result<Option<String>, String>) -> Option<String> {
    // Says what happened rather than guessing why, because the two ways of not
    // answering look identical from here and lead opposite ways: an engine
    // older than the variable is the stale-server case, and an engine that has
    // died is not.
    let unanswered = |what: String| {
        Some(format!(
            "--engine would not report @@ultralight_instance, so it cannot be shown to \
             be the engine this run started (instance {expected}): {what}.\nAn engine \
             built before that variable existed refuses it with 1193, which is the \
             stale-server case; a connection that has gone away fails here too. The \
             engine's own output says which."
        ))
    };
    match reported {
        Ok(Some(id)) if id == expected => None,
        Ok(Some(id)) => Some(format!(
            "--engine is a mysql-ultralight, but not the one this run started: it \
             reports instance {id}, and the engine started for this run reports \
             {expected}.\nSomething else is listening on that port — most likely a \
             server left behind by an earlier run. Comparing against it would \
             measure a build nobody is running, so the run stops here."
        )),
        Ok(None) => unanswered("it returned no row at all".to_owned()),
        Err(why) => unanswered(format!("it failed with `{why}`")),
    }
}

fn reset(conn: &mut Conn, group: &str) {
    for statement in [
        format!("SET SESSION sql_mode = '{DEFAULT_SQL_MODE}'"),
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

#[cfg(test)]
mod tests {
    use super::*;

    fn id(s: &str) -> Result<Option<String>, String> {
        Ok(Some(s.to_owned()))
    }

    #[test]
    fn the_engine_that_was_started_is_accepted() {
        assert_eq!(
            wrong_instance("3f1c0a9b6d2e4517", id("3f1c0a9b6d2e4517")),
            None
        );
    }

    /// The bug this exists for: a leftover server on the port, answering as a
    /// mysql-ultralight, from a build that is not the one under test.
    #[test]
    fn a_different_engine_on_the_port_is_refused() {
        let complaint = wrong_instance("3f1c0a9b6d2e4517", id("a0b1c2d3e4f50617"))
            .expect("a server with another id must be refused");
        // Both ids, because the first question asked of the message is which
        // process is on the port and which one was meant.
        assert!(complaint.contains("3f1c0a9b6d2e4517"), "{complaint}");
        assert!(complaint.contains("a0b1c2d3e4f50617"), "{complaint}");
    }

    /// An engine predating `@@ultralight_instance` refuses it with 1193. That
    /// is a stale build by definition, so it must fail rather than be waved
    /// through as "cannot tell".
    #[test]
    fn an_engine_that_cannot_answer_is_refused() {
        let refused = wrong_instance(
            "3f1c0a9b6d2e4517",
            Err("1193 Unknown system variable".into()),
        )
        .expect("an engine that refuses the question must be refused");
        // The server's own words, because "your build is stale" is a guess:
        // a connection that has gone away arrives here identically, and an
        // operator sent after a leftover process that does not exist has been
        // sent the wrong way by the message.
        assert!(
            refused.contains("1193 Unknown system variable"),
            "{refused}"
        );
        assert!(wrong_instance("3f1c0a9b6d2e4517", Ok(None)).is_some());
    }

    /// Ids are compared whole. A prefix or suffix match would let a value the
    /// harness merely *contains* pass, and these are fixed-width hex strings
    /// with no reason to be matched loosely.
    #[test]
    fn a_partial_match_is_not_a_match() {
        assert!(wrong_instance("3f1c0a9b6d2e4517", id("3f1c0a9b")).is_some());
        assert!(wrong_instance("3f1c0a9b", id("3f1c0a9b6d2e4517")).is_some());
    }
}
