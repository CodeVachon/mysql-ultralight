//! What the binary tells a shell.
//!
//! Every GitHub Actions `run:` step is `bash -e`, so a non-zero exit from a
//! command nobody thought of as fallible aborts the step. The release
//! workflow smoke-tests a freshly built binary with `mysql-ultralight --help`,
//! and that step is the gate between building the four platform binaries and
//! publishing them.

use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant};

/// Every argument here should make the binary exit at once. Waiting without a
/// bound would turn "this argument started a server instead" — the exact way
/// argument handling regresses — into a hang rather than a failure.
const DEADLINE: Duration = Duration::from_secs(10);

fn run(args: &[&str]) -> Output {
    let mut child = Command::new(env!("CARGO_BIN_EXE_mysql-ultralight"))
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("the binary under test should run");

    let started = Instant::now();
    loop {
        match child
            .try_wait()
            .expect("waiting on the child should not fail")
        {
            Some(_) => break,
            None if started.elapsed() >= DEADLINE => {
                let _ = child.kill();
                let _ = child.wait();
                panic!("mysql-ultralight {args:?} did not exit within {DEADLINE:?}");
            }
            None => std::thread::sleep(Duration::from_millis(10)),
        }
    }

    child
        .wait_with_output()
        .expect("the child should yield its output")
}

#[test]
fn help_succeeds_on_stdout() {
    for flag in ["-h", "--help"] {
        let output = run(&[flag]);
        assert_eq!(
            output.status.code(),
            Some(0),
            "{flag} must exit 0: a caller under `set -e` treats anything else as a failure"
        );
        let stdout = String::from_utf8(output.stdout).expect("help should be UTF-8");
        assert!(
            stdout.contains("USAGE:"),
            "{flag} printed no usage: {stdout}"
        );
        // Not stderr: help that was asked for is output, not a diagnostic, and
        // the workflow redirects stdout to /dev/null expecting silence.
        assert_eq!(output.stderr, b"", "{flag} should print nothing to stderr");
    }
}

#[test]
fn a_bad_flag_fails_on_stderr() {
    let output = run(&["--nonsense"]);
    assert_eq!(
        output.status.code(),
        Some(2),
        "an unknown flag must fail, or a typo starts a server with default options"
    );
    let stderr = String::from_utf8(output.stderr).expect("usage should be UTF-8");
    assert!(stderr.contains("USAGE:"), "no usage on stderr: {stderr}");
    assert_eq!(
        output.stdout, b"",
        "a misuse should print nothing to stdout"
    );
}

#[test]
fn a_flag_missing_its_value_fails() {
    // `--database` rather than `--port`, because `--port` proves nothing here:
    // an empty value fails its `parse()` and reaches the same exit through a
    // different branch, so the test passes even with the missing-value check
    // removed. `--database` takes its value verbatim, so only the check itself
    // stops the server from starting on a truncated command line.
    for flag in [
        "--database",
        "--socket",
        "--state",
        "--password",
        "--server-version",
    ] {
        let output = run(&[flag]);
        assert_eq!(
            output.status.code(),
            Some(2),
            "{flag} with no value must be a misuse, not a silent empty value"
        );
    }
}
