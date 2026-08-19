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

fn usage() -> ! {
    eprintln!(
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
    ready port=<PORT> socket=<PATH|-> startup_ms=<MS>
so a harness can parse the port and wait for readiness without polling.

STATE IMAGES
    Replaying a migration history costs seconds; loading its result costs
    milliseconds. Bake once, then boot from the image:

        mysql-ultralight --port 3306 &
        npx prisma migrate deploy
        mysql -h 127.0.0.1 -P 3306 -e \"SELECT ultralight_snapshot('schema.mus')\"

        mysql-ultralight --state schema.mus     # every test process

    `SELECT ultralight_restore('schema.mus')` does the same to a running server,
    which resets it between test files without paying for a new process."
    );
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
            "-h" | "--help" => usage(),
            _ => usage(),
        }
    }

    options
}

#[tokio::main]
async fn main() -> std::io::Result<()> {
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

    println!(
        "ready port={port} socket={} startup_ms={:.3}",
        options.socket.as_deref().unwrap_or("-"),
        started.elapsed().as_secs_f64() * 1000.0
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
