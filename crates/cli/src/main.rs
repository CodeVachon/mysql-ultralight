//! The `mysql-ultralight` server binary.

mod handler;

use std::sync::Arc;
use std::time::Instant;

use handler::EngineHandler;
use mysql_ultralight_engine::{Engine, ServerConfig};
use mysql_ultralight_server::{Config, Server, StderrTrace, Trace};

const DEFAULT_VERSION: &str = "8.3.0-mysql-ultralight";

struct Options {
    port: u16,
    database: Option<String>,
    socket: Option<String>,
    version: String,
    password: Option<String>,
    trace: bool,
    state: Option<String>,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            // Port 0 asks the OS for a free port, which is what a test harness
            // spawning many servers wants.
            port: 0,
            database: Some("test".to_owned()),
            socket: None,
            version: DEFAULT_VERSION.to_owned(),
            password: None,
            trace: false,
            state: None,
        }
    }
}

fn usage_text() -> String {
    format!(
        "mysql-ultralight — a fast-starting, MySQL 8 compatible engine for tests

USAGE:
    mysql-ultralight [OPTIONS]

OPTIONS:
    --port <PORT>        TCP port to listen on; 0 picks a free one (default: 0)
    --socket <PATH>      Also listen on a unix socket at PATH
    --database <NAME>    Database to create at startup (default: test)
    --state <PATH>       Load a baked state image instead of an empty catalog
    --password <PASS>    Require this password; omit to accept any credentials
    --server-version <V> Version string reported to clients (default: {DEFAULT_VERSION})
    --trace              Log every connection, command, and result to stderr
    -h, --help           Print this message

On startup the server prints one line to stdout:
    ready port=<PORT> socket=<PATH|-> startup_ms=<MS> instance=<ID>
so a harness can parse the port and wait for readiness without polling. The
instance id is fresh per process and is also served as @@ultralight_instance,
so a harness can check that the server it reached is the one it started rather
than a leftover process holding the same port.

STATE IMAGES
    Replaying a migration history costs seconds; loading its result costs
    milliseconds. Bake once, then boot from the image:

        mysql-ultralight --port 3306 &
        npx prisma migrate deploy
        mysql -h 127.0.0.1 -P 3306 -e \"SELECT ultralight_snapshot('schema.mus')\"

        mysql-ultralight --state schema.mus     # every test process

    `SELECT ultralight_restore('schema.mus')` does the same to a running server,
    which resets it between test files without paying for a new process."
    )
}

/// Help that was asked for: stdout, and a successful exit.
///
/// `--help` exiting non-zero aborts any caller running under `set -e`, which
/// is every GitHub Actions `run:` step — the release workflow's smoke test is
/// exactly `mysql-ultralight --help`, and it took the whole release down with
/// it.
fn help() -> ! {
    println!("{}", usage_text());
    std::process::exit(0);
}

/// The same text for a misuse: stderr, and a failing exit, so a script can
/// tell "you typed it wrong" from "here is what you asked for".
fn usage() -> ! {
    eprintln!("{}", usage_text());
    std::process::exit(2);
}

fn parse_args() -> Options {
    let mut options = Options::default();
    let mut args = std::env::args().skip(1);

    while let Some(arg) = args.next() {
        let mut value = || args.next().unwrap_or_else(|| usage());
        match arg.as_str() {
            "--port" => {
                options.port = value().parse().unwrap_or_else(|_| usage());
            }
            "--socket" => options.socket = Some(value()),
            "--state" => options.state = Some(value()),
            "--database" => options.database = Some(value()),
            "--password" => options.password = Some(value()),
            "--server-version" => options.version = value(),
            "--trace" => options.trace = true,
            "-h" | "--help" => help(),
            _ => usage(),
        }
    }

    options
}

/// The stack a connection's thread gets.
///
/// tokio's default is 2 MB, and the server used to take it. That is not enough
/// to *parse* a very long statement: sqlparser's descent over a left-deep
/// chain like `1 AND 1 AND ...` runs out somewhere between 60000 and 70000
/// terms, and it runs out inside the parser -- before the engine has an AST,
/// and so before `expr::within_walking_depth` can refuse it. The connection
/// did not get an error; the process aborted and took every other connection
/// with it.
///
/// Measured: at 32 MB both a 70000-term and a 100000-term chain come back as a
/// clean 1436, because the parser survives and the depth cap then does its
/// job. So this is not a second limit bolted on -- it is the room the existing
/// one needs in order to be reached.
///
/// A bound on the statement's *size* was measured and rejected: a 731 KB flat
/// INSERT of 20000 rows parses fine and agrees with 8.3, while a 410 KB chain
/// dies, and the flat one has more tokens than the chain. Neither bytes nor
/// tokens separate the two -- only the shape of the tree does, which is not
/// knowable without parsing. See #0247.
const CONNECTION_STACK: usize = 32 * 1024 * 1024;

fn main() -> std::io::Result<()> {
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .thread_stack_size(CONNECTION_STACK)
        .build()?
        .block_on(serve())
}

async fn serve() -> std::io::Result<()> {
    // Measured from as early as possible so the reported figure is the number
    // a test harness actually pays.
    let started = Instant::now();
    let options = parse_args();

    let config = Config {
        server_version: options.version.clone(),
        password: options.password.clone(),
        ..Default::default()
    };

    let listener = tokio::net::TcpListener::bind(("127.0.0.1", options.port)).await?;
    let port = listener.local_addr()?.port();

    #[cfg(unix)]
    let unix_listener = match &options.socket {
        Some(path) => {
            // The kernel caps unix socket paths at roughly 104 bytes on macOS
            // and 108 on Linux. Harnesses that build socket paths under a deep
            // temp directory sail past that, and the raw OS error names
            // neither the limit nor the path, so check it here.
            const SUN_PATH_MAX: usize = 100;
            if path.len() > SUN_PATH_MAX {
                eprintln!(
                    "socket path is {} bytes, which exceeds the {SUN_PATH_MAX} byte kernel limit:\n  {path}\n\
                     Use a shorter path, for example under /tmp.",
                    path.len()
                );
                std::process::exit(2);
            }
            // A leftover socket file from a killed process would otherwise
            // make every subsequent start fail. Only unlink an actual socket
            // though: `--socket ./data.db` must not destroy a real file.
            match std::fs::symlink_metadata(path) {
                Ok(meta) => {
                    use std::os::unix::fs::FileTypeExt;
                    if !meta.file_type().is_socket() {
                        eprintln!("refusing to use {path}: it already exists and is not a socket");
                        std::process::exit(2);
                    }
                    let _ = std::fs::remove_file(path);
                }
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => {
                    eprintln!("cannot inspect {path}: {e}");
                    std::process::exit(2);
                }
            }
            Some(tokio::net::UnixListener::bind(path)?)
        }
        None => None,
    };

    let trace: Arc<dyn Trace> = if options.trace {
        Arc::new(StderrTrace)
    } else {
        Arc::new(mysql_ultralight_server::NoTrace)
    };

    let engine_config = ServerConfig {
        version: options.version.clone(),
        port,
        socket: options.socket.clone(),
        ..Default::default()
    };
    let engine = Engine::new(engine_config.clone());

    // The image carries its own databases, so it is loaded before the
    // bootstrap: a `--database` that the image already holds must not be
    // recreated empty over the top of the baked one.
    if let Some(path) = &options.state {
        if let Err(e) = engine.load_image(std::path::Path::new(path)) {
            eprintln!("could not load state image {path}: {e}");
            std::process::exit(1);
        }
    }

    // A server with no database is unusable, and every client expects the one
    // named in its connection string to exist.
    if let Some(name) = &options.database {
        if let Err(e) = engine.bootstrap_database(name) {
            eprintln!("could not create database {name}: {e}");
            std::process::exit(1);
        }
    }

    let server = Arc::new(Server::new(config).with_trace(trace));

    // The instance id goes out over the pipe to whoever started this process,
    // which is what makes it proof: a leftover server on the same port never
    // gets to say this, and `@@ultralight_instance` reads the same value back.
    println!(
        "ready port={port} socket={} startup_ms={:.3} instance={}",
        options.socket.as_deref().unwrap_or("-"),
        started.elapsed().as_secs_f64() * 1000.0,
        engine_config.instance
    );
    use std::io::Write;
    let _ = std::io::stdout().flush();

    let tcp = tokio::spawn({
        let server = Arc::clone(&server);
        let engine = Arc::clone(&engine);
        let engine_config = engine_config.clone();

        async move {
            server
                // The id comes from the server, so the engine session and
                // the handshake packet cannot disagree about it.
                .serve_tcp(listener, move |id| {
                    EngineHandler::new(Arc::clone(&engine), engine_config.clone(), id)
                })
                .await
        }
    });

    #[cfg(unix)]
    let unix = unix_listener.map(|listener| {
        let server = Arc::clone(&server);
        let engine = Arc::clone(&engine);
        let engine_config = engine_config.clone();

        tokio::spawn(async move {
            server
                .serve_unix(listener, move |id| {
                    EngineHandler::new(Arc::clone(&engine), engine_config.clone(), id)
                })
                .await
        })
    });

    tokio::select! {
        result = tcp => { result??; }
        _ = tokio::signal::ctrl_c() => {}
    }

    #[cfg(unix)]
    if let Some(handle) = unix {
        handle.abort();
    }
    #[cfg(unix)]
    if let Some(path) = &options.socket {
        let _ = std::fs::remove_file(path);
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::CONNECTION_STACK;

    /// A statement too long to parse is refused, not fatal.
    ///
    /// Run on a thread of the size a connection actually gets, because that is
    /// the whole subject: on the 2 MB `cargo test` gives a thread, sqlparser's
    /// descent over this chain overflows *inside the parser* and aborts the
    /// process — which would take this test binary with it rather than fail an
    /// assertion. With the stack a connection has, the parser survives and
    /// `expr::within_walking_depth` refuses it with 1436.
    ///
    /// Measured against the real server before and after: at 70000 terms the
    /// client used to see `Lost connection to MySQL server during query`, and
    /// now sees the 1436. See #0247.
    #[test]
    fn a_statement_too_long_to_parse_is_refused_rather_than_fatal() {
        let handle = std::thread::Builder::new()
            .stack_size(CONNECTION_STACK)
            .spawn(|| {
                let engine = mysql_ultralight_engine::Engine::new(
                    mysql_ultralight_engine::ServerConfig::default(),
                );
                engine.bootstrap_database("t").unwrap();
                let mut session = mysql_ultralight_engine::Session::new(
                    mysql_ultralight_engine::ServerConfig::default(),
                    1,
                );
                session.current_database = Some("t".to_owned());

                let chain = std::iter::repeat_n("1", 70_000)
                    .collect::<Vec<_>>()
                    .join(" AND ");
                let refused = engine
                    .execute(&mut session, &format!("SELECT {chain}"), &[])
                    .unwrap_err();
                assert_eq!(refused.code, 1436, "{refused}");

                // And the connection is still usable afterwards, which is the
                // difference between a refusal and an abort.
                engine
                    .execute(&mut session, "SELECT 1", &[])
                    .expect("the session survives the refusal");
            })
            .expect("spawn");
        handle.join().expect("the thread must refuse, not abort");
    }
}
