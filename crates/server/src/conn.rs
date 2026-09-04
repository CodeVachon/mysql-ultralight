//! The per-connection state machine: handshake, authentication, then the
//! command loop until the client quits or the socket closes.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Instant;

use mysql_ultralight_protocol::auth::{self, MYSQL_NATIVE_PASSWORD};
use mysql_ultralight_protocol::buf::BufReader;
use mysql_ultralight_protocol::capabilities::status;
use mysql_ultralight_protocol::handshake::{
    encode_auth_switch_request, Handshake, HandshakeResponse,
};
use mysql_ultralight_protocol::packet::{
    encode_eof, read_packet, write_packet, ErrPacket, OkPacket, Sequence, DEFAULT_MAX_MESSAGE,
};
use mysql_ultralight_protocol::value::{bitmap_is_null, null_bitmap};
use mysql_ultralight_protocol::{BufWriter, Capabilities, Command, Value};
use tokio::io::{AsyncRead, AsyncWrite};

use crate::handler::{Handler, Prepared, Response, ResultSet};
use crate::trace::{Event, NoTrace, Trace};

/// Settings shared by every connection the server accepts.
#[derive(Debug, Clone)]
pub struct Config {
    /// Reported to clients. Drivers gate behaviour on this, so it must parse
    /// as a MySQL 8 version.
    pub server_version: String,
    /// The password required to connect. `None` accepts any credentials, which
    /// is the sane default for a throwaway test database.
    pub password: Option<String>,
    pub max_message_size: usize,
    /// Total bytes one connection may accumulate across all
    /// `COM_STMT_SEND_LONG_DATA` packets. `max_message_size` bounds a single
    /// packet, but a client can send unboundedly many of them.
    pub max_long_data_size: usize,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            server_version: "8.3.0-mysql-ultralight".to_owned(),
            password: None,
            max_message_size: DEFAULT_MAX_MESSAGE,
            max_long_data_size: DEFAULT_MAX_MESSAGE,
        }
    }
}

/// One client connection.
pub struct Connection<S, H> {
    stream: S,
    handler: H,
    config: Arc<Config>,
    trace: Arc<dyn Trace>,
    connection_id: u32,
    seq: Sequence,
    /// The intersection of what the server offers and the client requested.
    caps: Capabilities,
    statements: HashMap<u32, Prepared>,
    next_statement_id: u32,
    /// Whether multi-statement commands are currently accepted.
    ///
    /// Negotiated at handshake and then switchable with `COM_SET_OPTION`. A
    /// client that turns it off reads one result and leaves the rest in the
    /// socket, so running them anyway desynchronises the connection.
    multi_statements: bool,
    /// Parameter types from the last `COM_STMT_EXECUTE` that carried them.
    /// Clients re-executing a statement may omit the types and expect the
    /// server to have remembered them.
    statement_param_types: HashMap<u32, Vec<(u8, bool)>>,
    /// Accumulated `COM_STMT_SEND_LONG_DATA` payloads, keyed by statement and
    /// parameter index.
    long_data: HashMap<(u32, u16), Vec<u8>>,
    /// Running total of `long_data`, so enforcing the cap does not require
    /// walking the map on every packet.
    long_data_bytes: usize,
}

impl<S, H> Connection<S, H>
where
    S: AsyncRead + AsyncWrite + Unpin,
    H: Handler,
{
    pub fn new(stream: S, handler: H, config: Arc<Config>, connection_id: u32) -> Self {
        Self {
            stream,
            handler,
            config,
            trace: Arc::new(NoTrace),
            connection_id,
            seq: Sequence::new(),
            caps: Capabilities::EMPTY,
            statements: HashMap::new(),
            next_statement_id: 1,
            multi_statements: true,
            statement_param_types: HashMap::new(),
            long_data: HashMap::new(),
            long_data_bytes: 0,
        }
    }

    pub fn with_trace(mut self, trace: Arc<dyn Trace>) -> Self {
        self.trace = trace;
        self
    }

    /// Drives the connection to completion.
    pub async fn run(mut self, peer: &str) -> std::io::Result<()> {
        self.trace.event(Event::Connected {
            connection_id: self.connection_id,
            peer,
        });

        if let Err(e) = self.handshake().await {
            self.trace.event(Event::Disconnected {
                connection_id: self.connection_id,
            });
            return Err(e);
        }

        let result = self.command_loop().await;
        self.trace.event(Event::Disconnected {
            connection_id: self.connection_id,
        });
        result
    }

    async fn handshake(&mut self) -> std::io::Result<()> {
        let greeting = Handshake::new(&self.config.server_version, self.connection_id);
        let nonce = greeting.nonce;

        self.seq.reset();
        self.write(&greeting.encode()).await?;

        let Some(payload) = self.read().await? else {
            return Err(std::io::Error::new(
                std::io::ErrorKind::UnexpectedEof,
                "client closed during handshake",
            ));
        };

        let response = HandshakeResponse::decode(&payload).map_err(|e| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("bad handshake response: {e}"),
            )
        })?;

        self.caps = greeting.capabilities & response.capabilities;

        // A client that opened with a different plugin (caching_sha2_password
        // is the MySQL 8 default) must be switched before its response can be
        // interpreted.
        let mut auth_response = response.auth_response.clone();
        let plugin = response
            .auth_plugin
            .as_deref()
            .unwrap_or(MYSQL_NATIVE_PASSWORD);
        if plugin != MYSQL_NATIVE_PASSWORD {
            self.write(&encode_auth_switch_request(MYSQL_NATIVE_PASSWORD, &nonce))
                .await?;
            match self.read().await? {
                Some(p) => auth_response = p,
                None => {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::UnexpectedEof,
                        "client closed during auth switch",
                    ))
                }
            }
        }

        self.trace.event(Event::Authenticated {
            connection_id: self.connection_id,
            username: &response.username,
            database: response.database.as_deref(),
            auth_plugin: response.auth_plugin.as_deref(),
            capabilities: self.caps,
            attributes: &response.attributes,
        });

        if let Some(expected) = &self.config.password {
            if !auth::verify_native_password(expected.as_bytes(), &nonce, &auth_response) {
                let err = ErrPacket::access_denied(&response.username, "localhost");
                self.trace.event(Event::Failed {
                    connection_id: self.connection_id,
                    error: &err,
                });
                self.write(&err.encode(self.caps)).await?;
                return Err(std::io::Error::new(
                    std::io::ErrorKind::PermissionDenied,
                    "access denied",
                ));
            }
        }

        // After the password is accepted and before anything is run, which is
        // the only window: a rejected connection never reaches the handler,
        // and `USE <db>` below is already a statement the handler answers.
        self.handler.negotiated(self.caps);

        if let Some(db) = &response.database {
            if let Err(err) = self.handler.init_db(db) {
                self.write(&err.encode(self.caps)).await?;
                return Err(std::io::Error::new(
                    std::io::ErrorKind::NotFound,
                    "unknown database",
                ));
            }
        }

        self.write(&OkPacket::default().encode(self.caps)).await
    }

    async fn command_loop(&mut self) -> std::io::Result<()> {
        loop {
            let Some(payload) = self.read().await? else {
                return Ok(());
            };
            let Some((&first, args)) = payload.split_first() else {
                continue;
            };
            let command = Command::from_byte(first);

            let started = Instant::now();
            let detail = match command {
                Command::Query | Command::StmtPrepare | Command::InitDb => {
                    String::from_utf8_lossy(args).into_owned()
                }
                _ => String::new(),
            };
            self.trace.event(Event::Command {
                connection_id: self.connection_id,
                command,
                detail: &detail,
            });

            match command {
                Command::Quit => return Ok(()),
                Command::Ping => self.send_response(Response::ok(), started).await?,
                Command::Query => self.handle_query(&detail, started).await?,
                Command::InitDb => self.handle_init_db(&detail, started).await?,
                Command::StmtPrepare => self.handle_prepare(&detail, started).await?,
                Command::StmtExecute => self.handle_execute(args, started).await?,
                // Long data and statement close are answered with silence.
                Command::StmtSendLongData => self.handle_send_long_data(args),
                Command::StmtClose => self.handle_stmt_close(args),
                Command::StmtReset => self.handle_stmt_reset(args, started).await?,
                Command::ResetConnection => {
                    self.statements.clear();
                    self.statement_param_types.clear();
                    self.long_data.clear();
                    self.long_data_bytes = 0;
                    self.handler.reset();
                    self.send_response(Response::ok(), started).await?;
                }
                // Deprecated, but the mysql CLI still sends it for completion.
                Command::FieldList => self.write_eof_or_ok(false, 0).await?,
                Command::SetOption => {
                    // The payload is a two-byte little-endian option: 0 turns
                    // multi-statement commands on, 1 turns them off.
                    match detail.as_bytes() {
                        [0, 0] => self.multi_statements = true,
                        [1, 0] => self.multi_statements = false,
                        _ => {
                            self.send_error(&ErrPacket::unsupported("this COM_SET_OPTION option"))
                                .await?;
                            continue;
                        }
                    }
                    self.send_response(Response::ok(), started).await?;
                }
                Command::Statistics => {
                    let stats = self.handler.statistics();
                    self.write(stats.as_bytes()).await?;
                }
                Command::Debug | Command::ProcessKill => {
                    self.send_response(Response::ok(), started).await?;
                }
                other => {
                    let err = ErrPacket::unsupported(format!(
                        "{} is not supported by mysql-ultralight",
                        other.name()
                    ));
                    self.send_error(&err).await?;
                }
            }
        }
    }

    async fn handle_query(&mut self, sql: &str, started: Instant) -> std::io::Result<()> {
        // A client that did not negotiate MULTI_STATEMENTS cannot read a
        // second result, so the handler must refuse rather than run one.
        let allow_multi =
            self.multi_statements && self.caps.contains(Capabilities::MULTI_STATEMENTS);
        let answered = self.handler.query_multi(sql, allow_multi);

        // An empty command is still a command: MySQL answers a bare `;` with
        // one OK rather than nothing, and a client waiting on a reply that
        // never comes hangs.
        if answered.responses.is_empty() && answered.error.is_none() {
            return self.send_response(Response::ok(), started).await;
        }

        // Each result announces the next one. An error is itself the last
        // packet, so everything before it announces too.
        let last = answered.responses.len().saturating_sub(1);
        let failed = answered.error.is_some();
        for (index, response) in answered.responses.into_iter().enumerate() {
            // A refused response ended the command with an error packet that
            // said nothing follows it. The statements after it in the batch
            // have already run, but their answers cannot be delivered — and
            // writing them anyway is what would desynchronise the connection.
            if !self
                .send_response_with(response, started, false, failed || index != last)
                .await?
            {
                return Ok(());
            }
        }
        match answered.error {
            Some(err) => self.send_error(&err).await,
            None => Ok(()),
        }
    }

    async fn handle_init_db(&mut self, db: &str, started: Instant) -> std::io::Result<()> {
        match self.handler.init_db(db) {
            Ok(()) => self.send_response(Response::ok(), started).await,
            Err(err) => self.send_error(&err).await,
        }
    }

    async fn handle_prepare(&mut self, sql: &str, started: Instant) -> std::io::Result<()> {
        let id = self.next_statement_id;
        match self.handler.prepare(id, sql) {
            Ok(mut stmt) => {
                // The wire id is the connection's to assign. A handler that
                // returned a different one would leave every later EXECUTE and
                // long-data packet keyed against an id the client never saw.
                stmt.id = id;
                self.next_statement_id = self.next_statement_id.wrapping_add(1).max(1);
                let encoded = self.encode_prepare_response(&stmt);
                self.statements.insert(id, stmt);
                for packet in encoded {
                    self.write(&packet).await?;
                }
                self.trace.event(Event::Completed {
                    connection_id: self.connection_id,
                    summary: &format!("prepared statement {id}"),
                    elapsed: started.elapsed(),
                });
                Ok(())
            }
            Err(err) => self.send_error(&err).await,
        }
    }

    /// Builds every packet of a `COM_STMT_PREPARE` response: the header, then
    /// parameter definitions, then column definitions, each block terminated
    /// by an EOF unless the client deprecated it.
    fn encode_prepare_response(&self, stmt: &Prepared) -> Vec<Vec<u8>> {
        let mut packets = Vec::new();
        let deprecate_eof = self.caps.contains(Capabilities::DEPRECATE_EOF);

        let mut header = BufWriter::with_capacity(12);
        header
            .u8(0x00)
            .u32(stmt.id)
            .u16(stmt.columns.len() as u16)
            .u16(stmt.param_count)
            .u8(0x00)
            .u16(0);
        packets.push(header.into_inner());

        if stmt.param_count > 0 {
            // Parameter types are not known ahead of execution, so each is
            // described as a nullable string, which is what MySQL does for an
            // unresolved placeholder.
            for i in 0..stmt.param_count {
                let def = mysql_ultralight_protocol::ColumnDef::new(
                    format!("?{i}"),
                    mysql_ultralight_protocol::ColumnType::VarString,
                )
                // Said rather than relied on: `ColumnDef::new` seeds the flag
                // from the type code, so this was a nullable string only
                // because `VarString` happens to be in the arm that sets no
                // flags.
                .nullable(true);
                packets.push(def.encode());
            }
            if !deprecate_eof {
                packets.push(encode_eof(self.caps, 0, status::AUTOCOMMIT));
            }
        }

        if !stmt.columns.is_empty() {
            for column in &stmt.columns {
                packets.push(column.encode());
            }
            if !deprecate_eof {
                packets.push(encode_eof(self.caps, 0, status::AUTOCOMMIT));
            }
        }

        packets
    }

    async fn handle_execute(&mut self, args: &[u8], started: Instant) -> std::io::Result<()> {
        let mut r = BufReader::new(args);
        let Ok(stmt_id) = r.u32() else {
            return self
                .send_error(&ErrPacket::unknown_stmt_handler("malformed"))
                .await;
        };

        let Some(stmt) = self.statements.get(&stmt_id).cloned() else {
            return self
                .send_error(&ErrPacket::unknown_stmt_handler(&stmt_id.to_string()))
                .await;
        };

        let params = match self.decode_execute_params(&mut r, &stmt) {
            Ok(params) => params,
            Err(err) => return self.send_error(&err).await,
        };

        // Long data is consumed by exactly one execution.
        self.forget_long_data(stmt_id);

        match self.handler.execute(&stmt, &params) {
            Ok(response) => {
                // One response, so there is nothing after it to abandon.
                self.send_response_with(response, started, true, false)
                    .await
                    .map(|_| ())
            }
            Err(err) => self.send_error(&err).await,
        }
    }

    /// Parses the bound parameters of a `COM_STMT_EXECUTE` payload.
    fn decode_execute_params(
        &mut self,
        r: &mut BufReader<'_>,
        stmt: &Prepared,
    ) -> Result<Vec<Value>, ErrPacket> {
        let malformed =
            || ErrPacket::new(1210, "HY000", "Incorrect arguments to mysqld_stmt_execute");

        // flags, then iteration count, both ignored: cursors are not supported
        // and the iteration count is always 1 in practice.
        r.u8().map_err(|_| malformed())?;
        r.u32().map_err(|_| malformed())?;

        let count = stmt.param_count as usize;
        if count == 0 {
            return Ok(Vec::new());
        }

        let bitmap = r.take(count.div_ceil(8)).map_err(|_| malformed())?.to_vec();
        let new_params_bound = r.u8().map_err(|_| malformed())? == 1;

        // Types are only re-sent when they change, so a re-execution may omit
        // them and expect the server to recall the previous binding. Assuming
        // VAR_STRING instead would read a raw 8-byte LONGLONG as a
        // length-encoded string: wrong values, and a misaligned reader that
        // corrupts every parameter after it, with no error raised.
        let types: Vec<(u8, bool)> = if new_params_bound {
            let mut parsed = Vec::with_capacity(count);
            for _ in 0..count {
                let ty = r.u8().map_err(|_| malformed())?;
                let flag = r.u8().map_err(|_| malformed())?;
                parsed.push((ty, flag & 0x80 != 0));
            }
            self.statement_param_types.insert(stmt.id, parsed.clone());
            parsed
        } else {
            match self.statement_param_types.get(&stmt.id) {
                Some(remembered) if remembered.len() == count => remembered.clone(),
                _ => {
                    return Err(ErrPacket::new(
                        1210,
                        "HY000",
                        "Incorrect arguments to mysqld_stmt_execute: parameter types were not \
                         sent and none were remembered from a previous execution",
                    ))
                }
            }
        };

        let mut params = Vec::with_capacity(count);
        for (i, (ty, unsigned)) in types.into_iter().enumerate() {
            if bitmap_is_null(&bitmap, i) {
                params.push(Value::Null);
                continue;
            }
            if let Some(bytes) = self.long_data.get(&(stmt.id, i as u16)) {
                params.push(Value::Bytes(bytes.clone()));
                continue;
            }
            params.push(Value::decode_binary(r, ty, unsigned).map_err(|_| malformed())?);
        }

        Ok(params)
    }

    /// Accumulates a long parameter value.
    ///
    /// The protocol gives this command no reply, so a rejection can only be
    /// silent. Both guards matter: without the statement check a client can
    /// grow the map under ids it never prepared, and nothing would ever free
    /// them; without the byte cap it can grow the map without limit.
    fn handle_send_long_data(&mut self, args: &[u8]) {
        let mut r = BufReader::new(args);
        let (Ok(stmt_id), Ok(param_id)) = (r.u32(), r.u16()) else {
            return;
        };

        if !self.statements.contains_key(&stmt_id) {
            return;
        }

        let chunk = r.rest();
        if self.long_data_bytes.saturating_add(chunk.len()) > self.config.max_long_data_size {
            return;
        }

        self.long_data_bytes += chunk.len();
        self.long_data
            .entry((stmt_id, param_id))
            .or_default()
            .extend_from_slice(chunk);
    }

    /// Drops every long-data buffer for a statement, keeping the byte total in
    /// step with the map.
    fn forget_long_data(&mut self, stmt_id: u32) {
        let mut freed = 0usize;
        self.long_data.retain(|(id, _), buffer| {
            if *id == stmt_id {
                freed += buffer.len();
                false
            } else {
                true
            }
        });
        self.long_data_bytes = self.long_data_bytes.saturating_sub(freed);
    }

    fn handle_stmt_close(&mut self, args: &[u8]) {
        let mut r = BufReader::new(args);
        if let Ok(stmt_id) = r.u32() {
            self.statements.remove(&stmt_id);
            self.statement_param_types.remove(&stmt_id);
            self.forget_long_data(stmt_id);
        }
    }

    async fn handle_stmt_reset(&mut self, args: &[u8], started: Instant) -> std::io::Result<()> {
        let mut r = BufReader::new(args);
        match r.u32() {
            Ok(stmt_id) if self.statements.contains_key(&stmt_id) => {
                self.forget_long_data(stmt_id);
                self.send_response(Response::ok(), started).await
            }
            Ok(stmt_id) => {
                self.send_error(&ErrPacket::unknown_stmt_handler(&stmt_id.to_string()))
                    .await
            }
            Err(_) => {
                self.send_error(&ErrPacket::unknown_stmt_handler("malformed"))
                    .await
            }
        }
    }

    async fn send_response(&mut self, response: Response, started: Instant) -> std::io::Result<()> {
        // A single response has nothing after it to abandon, so whether the
        // command was cut short is the caller's answer to nobody.
        self.send_response_with(response, started, false, false)
            .await
            .map(|_| ())
    }

    /// Writes one response. `more` sets `MORE_RESULTS_EXISTS`, which is how a
    /// client knows to read another result rather than treating this as the
    /// end of the command.
    ///
    /// Answers `false` when the response was refused rather than written, and
    /// the caller must stop: an error packet carries no `MORE_RESULTS_EXISTS`,
    /// so it *is* the end of the command whatever the caller intended to send
    /// next. Writing the rest of a batch after one would put packets on the
    /// wire that the client has already stopped reading for this command —
    /// the same desynchronisation the refusal exists to prevent, reached the
    /// long way round.
    async fn send_response_with(
        &mut self,
        response: Response,
        started: Instant,
        binary: bool,
        more: bool,
    ) -> std::io::Result<bool> {
        if matches!(&response, Response::ResultSet(rs) if rs.columns.is_empty()) {
            self.send_error(&ErrPacket::new(1105, "HY000", Self::NO_COLUMNS))
                .await?;
            return Ok(false);
        }

        let summary = match &response {
            Response::ResultSet(rs) => {
                format!("{} row(s), {} column(s)", rs.rows.len(), rs.columns.len())
            }
            Response::Ok(ok) => format!("ok, {} row(s) affected", ok.affected_rows),
        };

        match response {
            Response::Ok(mut ok) => {
                if more {
                    ok.status_flags |= status::MORE_RESULTS_EXISTS;
                }
                self.write(&ok.encode(self.caps)).await?
            }
            Response::ResultSet(rs) => self.write_result_set(&rs, binary, more).await?,
        }

        self.trace.event(Event::Completed {
            connection_id: self.connection_id,
            summary: &summary,
            elapsed: started.elapsed(),
        });
        Ok(true)
    }

    /// A result set with no columns, which the wire protocol has no way to
    /// say.
    ///
    /// The header of a result set is a length-encoded column count, and a
    /// payload beginning `0x00` *is* an OK packet — the two are the same byte.
    /// So writing zero here does not describe an empty result: it tells the
    /// client the command is over, and the column definitions, rows and
    /// terminator that follow are then read as the answers to whatever the
    /// client sends next. That is the failure mode this whole file exists to
    /// prevent, and it surfaces one statement later, on the connection rather
    /// than on the statement that caused it.
    ///
    /// MySQL never has to encode this, because it has no way to produce it.
    /// Every route was measured against 8.3 and every one is refused before it
    /// gets near a result set: `SELECT` with an empty list is 1064, `SELECT *`
    /// with no FROM is 1096, `CREATE TABLE t ()` is 1064, dropping a table's
    /// last column is 1090, and `VALUES ROW()` is 3942. A table with no
    /// columns cannot be made, so a `SELECT *` over one cannot be asked.
    ///
    /// The engine refuses the same routes, and this is the backstop for the
    /// one that has not been thought of yet: refusing *here*, before the
    /// header is written, is what keeps the connection usable — nothing has
    /// been sent, so an error packet is a complete and correct answer and the
    /// next statement still works. Refusing later, as a mismatched row must be
    /// because its header is already gone, cannot offer that.
    const NO_COLUMNS: &'static str =
        "a result set must have at least one column; the wire protocol cannot express one with none";

    async fn write_result_set(
        &mut self,
        rs: &ResultSet,
        binary: bool,
        more: bool,
    ) -> std::io::Result<()> {
        let mut count = BufWriter::with_capacity(9);
        count.lenenc_int(rs.columns.len() as u64);
        self.write(&count.into_inner()).await?;

        for column in &rs.columns {
            self.write(&column.encode()).await?;
        }

        if !self.caps.contains(Capabilities::DEPRECATE_EOF) {
            self.write(&encode_eof(self.caps, 0, status::AUTOCOMMIT))
                .await?;
        }

        for row in &rs.rows {
            // A row of the wrong width produces a stream the client cannot
            // parse, and it fails far from the handler that caused it. Refuse
            // here, where the mistake is still nameable.
            if row.len() != rs.columns.len() {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    format!(
                        "handler returned a row with {} value(s) for {} column(s)",
                        row.len(),
                        rs.columns.len()
                    ),
                ));
            }

            let mut w = BufWriter::with_capacity(32 * row.len().max(1));
            if binary {
                // Binary rows open with a 0x00 marker and a null bitmap that
                // is offset by two reserved bits.
                w.u8(0x00).bytes(&null_bitmap(row, 2));
                for (value, column) in row.iter().zip(&rs.columns) {
                    value.encode_binary(&mut w, column.column_type);
                }
            } else {
                for value in row {
                    value.encode_text(&mut w);
                }
            }
            self.write(&w.into_inner()).await?;
        }

        self.write_eof_or_ok(more, rs.warnings).await
    }

    /// Terminates a result set the way the negotiated capabilities require.
    ///
    /// The terminator carries the status flags, so a result set followed by
    /// another announces it here rather than in its header. It carries the
    /// warning count too, which is the only place a statement returning rows
    /// can report one — there is no OK packet to put it in. Both shapes get
    /// it: a client that deprecated EOF reads the same number out of the OK
    /// that replaces it.
    async fn write_eof_or_ok(&mut self, more: bool, warnings: u16) -> std::io::Result<()> {
        let mut flags = status::AUTOCOMMIT;
        if more {
            flags |= status::MORE_RESULTS_EXISTS;
        }
        let packet = if self.caps.contains(Capabilities::DEPRECATE_EOF) {
            let ok = OkPacket {
                status_flags: flags,
                warnings,
                ..OkPacket::default()
            };
            ok.encode_as_eof(self.caps)
        } else {
            encode_eof(self.caps, warnings, flags)
        };
        self.write(&packet).await
    }

    async fn send_error(&mut self, err: &ErrPacket) -> std::io::Result<()> {
        self.trace.event(Event::Failed {
            connection_id: self.connection_id,
            error: err,
        });
        self.write(&err.encode(self.caps)).await
    }

    async fn write(&mut self, payload: &[u8]) -> std::io::Result<()> {
        write_packet(&mut self.stream, &mut self.seq, payload).await?;
        use tokio::io::AsyncWriteExt;
        self.stream.flush().await
    }

    async fn read(&mut self) -> std::io::Result<Option<Vec<u8>>> {
        read_packet(
            &mut self.stream,
            &mut self.seq,
            self.config.max_message_size,
        )
        .await
    }
}
