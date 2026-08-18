//! A MySQL-speaking server front end.
//!
//! Owns listeners and connection lifecycle; knows nothing about SQL. Supply a
//! [`Handler`] factory and it will speak the protocol on that handler's behalf
//! over TCP or a unix socket.

pub mod conn;
pub mod handler;
pub mod trace;

use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;

pub use conn::{Config, Connection};
pub use handler::{Handler, HandlerResult, Prepared, Response, ResultSet};
pub use trace::{Event, NoTrace, StderrTrace, Trace};

use tokio::net::TcpListener;

/// Accepts connections and hands each one a fresh handler.
pub struct Server {
    config: Arc<Config>,
    trace: Arc<dyn Trace>,
    next_connection_id: AtomicU32,
}

impl Server {
    pub fn new(config: Config) -> Self {
        Self {
            config: Arc::new(config),
            trace: Arc::new(NoTrace),
            next_connection_id: AtomicU32::new(1),
        }
    }

    pub fn with_trace(mut self, trace: Arc<dyn Trace>) -> Self {
        self.trace = trace;
        self
    }

    fn next_id(&self) -> u32 {
        // Connection id 0 is reserved, so wrap to 1 rather than 0.
        let id = self.next_connection_id.fetch_add(1, Ordering::Relaxed);
        if id == 0 {
            1
        } else {
            id
        }
    }

    /// Serves TCP connections until the listener fails.
    ///
    /// `make_handler` is called once per accepted connection, so each session
    /// gets independent state.
    pub async fn serve_tcp<H, F>(
        self: Arc<Self>,
        listener: TcpListener,
        make_handler: F,
    ) -> std::io::Result<()>
    where
        H: Handler + 'static,
        F: Fn() -> H + Send + Sync + 'static,
    {
        let make_handler = Arc::new(make_handler);
        loop {
            let (stream, peer) = listener.accept().await?;
            // Latency matters more than throughput for a test database: a
            // 40ms Nagle delay on a small result set is a disaster.
            let _ = stream.set_nodelay(true);

            let server = Arc::clone(&self);
            let make_handler = Arc::clone(&make_handler);
            let connection_id = self.next_id();

            tokio::spawn(async move {
                let handler = make_handler();
                let connection =
                    Connection::new(stream, handler, Arc::clone(&server.config), connection_id)
                        .with_trace(Arc::clone(&server.trace));

                if let Err(e) = connection.run(&peer.to_string()).await {
                    // A client vanishing is routine, not an error worth noise.
                    if !is_disconnect(&e) {
                        eprintln!("[{connection_id}] connection error: {e}");
                    }
                }
            });
        }
    }

    /// Serves unix socket connections. Preferred over TCP for parallel test
    /// workers, which would otherwise race for ports.
    #[cfg(unix)]
    pub async fn serve_unix<H, F>(
        self: Arc<Self>,
        listener: tokio::net::UnixListener,
        make_handler: F,
    ) -> std::io::Result<()>
    where
        H: Handler + 'static,
        F: Fn() -> H + Send + Sync + 'static,
    {
        let make_handler = Arc::new(make_handler);
        loop {
            let (stream, _) = listener.accept().await?;
            let server = Arc::clone(&self);
            let make_handler = Arc::clone(&make_handler);
            let connection_id = self.next_id();

            tokio::spawn(async move {
                let handler = make_handler();
                let connection =
                    Connection::new(stream, handler, Arc::clone(&server.config), connection_id)
                        .with_trace(Arc::clone(&server.trace));

                if let Err(e) = connection.run("unix").await {
                    if !is_disconnect(&e) {
                        eprintln!("[{connection_id}] connection error: {e}");
                    }
                }
            });
        }
    }
}

fn is_disconnect(e: &std::io::Error) -> bool {
    matches!(
        e.kind(),
        std::io::ErrorKind::UnexpectedEof
            | std::io::ErrorKind::ConnectionReset
            | std::io::ErrorKind::BrokenPipe
    )
}
