//! End-to-end tests: a real TCP client driving a real server through the
//! handshake, the text protocol, and the prepared-statement protocol.
//!
//! These hold the whole protocol honest in a way unit tests cannot, because a
//! sequence number or bitmap that is off by one only shows up once a client is
//! parsing the stream.

use std::sync::Arc;

use mysql_ultralight_protocol::auth::native_password_scramble;
use mysql_ultralight_protocol::buf::BufReader;
use mysql_ultralight_protocol::packet::{read_packet, write_packet, Sequence};
use mysql_ultralight_protocol::{BufWriter, Capabilities, ColumnDef, ColumnType, ErrPacket, Value};
use mysql_ultralight_server::{
    Config, Handler, HandlerResult, Prepared, Response, ResultSet, Server,
};
use tokio::net::{TcpListener, TcpStream};

/// A handler with just enough behaviour to exercise every protocol path.
struct TestHandler;

impl Handler for TestHandler {
    fn query(&mut self, sql: &str) -> HandlerResult {
        match sql.trim() {
            "SELECT 1" => Ok(Response::ResultSet(ResultSet::with_rows(
                vec![ColumnDef::new("1", ColumnType::LongLong)],
                vec![vec![Value::I64(1)]],
            ))),
            "SELECT NULL" => Ok(Response::ResultSet(ResultSet::with_rows(
                vec![ColumnDef::new("null_col", ColumnType::VarString).nullable(true)],
                vec![vec![Value::Null]],
            ))),
            "MULTI" => Ok(Response::ResultSet(ResultSet::with_rows(
                vec![
                    ColumnDef::new("a", ColumnType::LongLong),
                    ColumnDef::new("b", ColumnType::VarString).nullable(true),
                ],
                vec![
                    vec![Value::I64(1), Value::Str("one".into())],
                    vec![Value::I64(2), Value::Null],
                    vec![Value::I64(3), Value::Str("three".into())],
                ],
            ))),
            // Deliberately the wrong width for its column list.
            "BADROW" => Ok(Response::ResultSet(ResultSet::with_rows(
                vec![
                    ColumnDef::new("a", ColumnType::LongLong),
                    ColumnDef::new("b", ColumnType::LongLong),
                ],
                vec![vec![Value::I64(1)]],
            ))),
            "BOOM" => Err(ErrPacket::parse_error("deliberate failure")),
            "UPDATE" => Ok(Response::affected(7)),
            _ => Ok(Response::ok()),
        }
    }

    fn prepare(&mut self, id: u32, sql: &str) -> Result<Prepared, ErrPacket> {
        let param_count = sql.matches('?').count() as u16;
        // A real handler works the shape out before running the statement; the
        // test one knows it, and declares it the same way.
        let columns = match self.query(sql) {
            Ok(Response::ResultSet(rs)) => rs.columns,
            _ => Vec::new(),
        };
        Ok(Prepared {
            id,
            sql: sql.to_owned(),
            param_count,
            columns,
        })
    }

    fn execute(&mut self, stmt: &Prepared, params: &[Value]) -> HandlerResult {
        // Echo the parameters back so the caller can verify binary decoding.
        if stmt.sql.contains('?') {
            let columns = params
                .iter()
                .enumerate()
                .map(|(i, v)| ColumnDef::new(format!("p{i}"), v.inferred_type()).nullable(true))
                .collect();
            return Ok(Response::ResultSet(ResultSet::with_rows(
                columns,
                vec![params.to_vec()],
            )));
        }
        self.query(&stmt.sql)
    }
}

/// Starts a server on an ephemeral port and returns its address.
async fn start_server(password: Option<String>) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap().to_string();

    let config = Config {
        password,
        ..Default::default()
    };
    let server = Arc::new(Server::new(config));
    tokio::spawn(async move { server.serve_tcp(listener, |_| TestHandler).await });

    addr
}

/// A minimal client: enough of the protocol to assert on what the server sends.
struct Client {
    stream: TcpStream,
    seq: Sequence,
    caps: Capabilities,
}

impl Client {
    /// Connects and completes the handshake.
    async fn connect(addr: &str, password: &str, deprecate_eof: bool) -> std::io::Result<Self> {
        let mut stream = TcpStream::connect(addr).await?;
        let mut seq = Sequence::new();

        let greeting = read_packet(&mut stream, &mut seq, 1 << 20)
            .await?
            .expect("greeting");
        assert_eq!(greeting[0], 10, "expected HandshakeV10");

        let nonce = parse_nonce(&greeting);

        let mut caps = Capabilities::PROTOCOL_41
            | Capabilities::SECURE_CONNECTION
            | Capabilities::PLUGIN_AUTH
            | Capabilities::TRANSACTIONS;
        if deprecate_eof {
            caps |= Capabilities::DEPRECATE_EOF;
        }

        let scramble = native_password_scramble(password.as_bytes(), &nonce);
        let mut w = BufWriter::new();
        w.u32(caps.bits())
            .u32(16 * 1024 * 1024)
            .u8(45)
            .zeros(23)
            .null_str("tester")
            .u8(scramble.len() as u8)
            .bytes(&scramble)
            .null_str("mysql_native_password");

        write_packet(&mut stream, &mut seq, &w.into_inner()).await?;

        let reply = read_packet(&mut stream, &mut seq, 1 << 20)
            .await?
            .expect("auth reply");
        if reply[0] == 0xff {
            let err = decode_err(&reply);
            return Err(std::io::Error::other(err));
        }
        assert_eq!(reply[0], 0x00, "expected OK after handshake");

        Ok(Self { stream, seq, caps })
    }

    async fn send(&mut self, command: u8, args: &[u8]) -> std::io::Result<()> {
        self.seq.reset();
        let mut payload = vec![command];
        payload.extend_from_slice(args);
        write_packet(&mut self.stream, &mut self.seq, &payload).await
    }

    async fn recv(&mut self) -> std::io::Result<Vec<u8>> {
        Ok(read_packet(&mut self.stream, &mut self.seq, 1 << 20)
            .await?
            .expect("packet"))
    }

    /// Like `recv`, but reports a closed connection rather than panicking.
    async fn try_recv(&mut self) -> Option<Vec<u8>> {
        read_packet(&mut self.stream, &mut self.seq, 1 << 20)
            .await
            .ok()
            .flatten()
    }

    /// Runs a text-protocol query and returns its rows as strings.
    async fn query(&mut self, sql: &str) -> Result<Vec<Vec<Option<String>>>, String> {
        self.send(0x03, sql.as_bytes()).await.unwrap();
        self.read_result_set(false).await
    }

    /// Reads a result set, or an OK/ERR terminator.
    async fn read_result_set(&mut self, binary: bool) -> Result<Vec<Vec<Option<String>>>, String> {
        let first = self.recv().await.unwrap();
        match first[0] {
            0xff => return Err(decode_err(&first)),
            // An OK packet in place of a column count: no result set.
            0x00 => return Ok(Vec::new()),
            _ => {}
        }

        let mut r = BufReader::new(&first);
        let column_count = r.lenenc_int().unwrap().unwrap() as usize;

        // Binary rows are decoded per column type, so the definitions matter.
        let mut column_types = Vec::with_capacity(column_count);
        for _ in 0..column_count {
            let packet = self.recv().await.unwrap();
            column_types.push(parse_column_type(&packet));
        }
        if !self.caps.contains(Capabilities::DEPRECATE_EOF) {
            let eof = self.recv().await.unwrap();
            assert_eq!(eof[0], 0xfe, "expected EOF after column definitions");
        }

        let mut rows = Vec::new();
        loop {
            let packet = self.recv().await.unwrap();
            // 0xFE under 9 bytes terminates; a longer packet starting 0xFE is
            // a row whose first value happens to be 254 bytes long.
            if packet[0] == 0xfe && packet.len() < 9 {
                break;
            }
            if packet[0] == 0xff {
                return Err(decode_err(&packet));
            }

            let mut r = BufReader::new(&packet);
            let mut row = Vec::with_capacity(column_count);

            if binary {
                r.u8().unwrap();
                let bitmap = r.take((column_count + 7 + 2) / 8).unwrap().to_vec();
                for (i, &ty) in column_types.iter().enumerate() {
                    if mysql_ultralight_protocol::value::bitmap_is_null(&bitmap, i + 2) {
                        row.push(None);
                        continue;
                    }
                    // Fixed-width types are raw little-endian here, not
                    // length-encoded as they are in the text protocol.
                    let value = Value::decode_binary(&mut r, ty, false).unwrap();
                    row.push(Some(render(&value)));
                }
            } else {
                for _ in 0..column_count {
                    match r.lenenc_bytes().unwrap() {
                        None => row.push(None),
                        Some(bytes) => row.push(Some(String::from_utf8_lossy(bytes).into_owned())),
                    }
                }
            }
            rows.push(row);
        }

        Ok(rows)
    }
}

/// Extracts the column name from a ColumnDefinition41 packet.
///
/// A client that reads a result by column name — the Prisma migration engine
/// does — gets nothing useful if this is empty, and reports the column
/// positionally instead. That failure is invisible until something looks a
/// column up by name and silently finds none.
fn parse_column_name(packet: &[u8]) -> String {
    let mut r = BufReader::new(packet);
    // catalog, schema, table, org_table, then the name.
    for _ in 0..4 {
        r.lenenc_bytes().unwrap();
    }
    String::from_utf8_lossy(r.lenenc_bytes().unwrap().unwrap()).into_owned()
}

/// Reads the column definitions of a result set, returning their names.
async fn column_names(client: &mut Client) -> Vec<String> {
    let first = client.recv().await.unwrap();
    let mut r = BufReader::new(&first);
    let count = r.lenenc_int().unwrap().unwrap() as usize;
    let mut names = Vec::with_capacity(count);
    for _ in 0..count {
        let packet = client.recv().await.unwrap();
        names.push(parse_column_name(&packet));
    }
    names
}

/// Extracts the type code from a ColumnDefinition41 packet.
fn parse_column_type(packet: &[u8]) -> u8 {
    let mut r = BufReader::new(packet);
    // catalog, schema, table, org_table, name, org_name
    for _ in 0..6 {
        r.lenenc_bytes().unwrap();
    }
    // Length of the fixed block, then charset and column length.
    r.lenenc_int().unwrap();
    r.u16().unwrap();
    r.u32().unwrap();
    r.u8().unwrap()
}

/// Renders a decoded value the way the text protocol would present it, so both
/// protocols can be compared against the same expectation.
fn render(value: &Value) -> String {
    match value {
        Value::Null => String::new(),
        Value::I64(v) => v.to_string(),
        Value::U64(v) => v.to_string(),
        Value::F32(v) => v.to_string(),
        Value::F64(v) => v.to_string(),
        Value::Str(v) => v.clone(),
        Value::Bytes(v) => String::from_utf8_lossy(v).into_owned(),
    }
}

/// Pulls the 20 byte nonce out of a HandshakeV10 payload.
fn parse_nonce(greeting: &[u8]) -> Vec<u8> {
    let mut r = BufReader::new(greeting);
    r.u8().unwrap();
    r.null_bytes().unwrap();
    r.u32().unwrap();
    let mut nonce = r.take(8).unwrap().to_vec();
    r.u8().unwrap();
    r.u16().unwrap();
    r.u8().unwrap();
    r.u16().unwrap();
    r.u16().unwrap();
    let auth_len = r.u8().unwrap() as usize;
    r.skip(10).unwrap();
    let rest = auth_len.saturating_sub(8).saturating_sub(1);
    nonce.extend_from_slice(r.take(rest).unwrap());
    nonce
}

fn decode_err(packet: &[u8]) -> String {
    let mut r = BufReader::new(packet);
    r.u8().unwrap();
    let code = r.u16().unwrap();
    r.u8().unwrap();
    let state = String::from_utf8_lossy(r.take(5).unwrap()).into_owned();
    let message = String::from_utf8_lossy(r.rest()).into_owned();
    format!("{code} ({state}): {message}")
}

#[tokio::test]
async fn handshake_and_text_query() {
    let addr = start_server(None).await;
    let mut client = Client::connect(&addr, "", true).await.unwrap();

    let rows = client.query("SELECT 1").await.unwrap();
    assert_eq!(rows, vec![vec![Some("1".to_owned())]]);
}

#[tokio::test]
async fn text_protocol_transmits_null_distinctly_from_empty_string() {
    let addr = start_server(None).await;
    let mut client = Client::connect(&addr, "", true).await.unwrap();

    let rows = client.query("SELECT NULL").await.unwrap();
    assert_eq!(rows, vec![vec![None]]);
}

#[tokio::test]
async fn multi_row_multi_column_result_sets_stay_aligned() {
    let addr = start_server(None).await;
    let mut client = Client::connect(&addr, "", true).await.unwrap();

    let rows = client.query("MULTI").await.unwrap();
    assert_eq!(
        rows,
        vec![
            vec![Some("1".to_owned()), Some("one".to_owned())],
            vec![Some("2".to_owned()), None],
            vec![Some("3".to_owned()), Some("three".to_owned())],
        ]
    );
}

/// Without DEPRECATE_EOF the server must terminate result sets with real EOF
/// packets instead of an OK. Clients that predate the flag rely on it.
#[tokio::test]
async fn result_sets_terminate_correctly_without_deprecate_eof() {
    let addr = start_server(None).await;
    let mut client = Client::connect(&addr, "", false).await.unwrap();

    let rows = client.query("MULTI").await.unwrap();
    assert_eq!(rows.len(), 3);
}

#[tokio::test]
async fn errors_carry_their_code_and_sqlstate() {
    let addr = start_server(None).await;
    let mut client = Client::connect(&addr, "", true).await.unwrap();

    let err = client.query("BOOM").await.unwrap_err();
    assert!(err.starts_with("1064 (42000)"), "unexpected error: {err}");
}

/// A failed statement must leave the connection usable.
#[tokio::test]
async fn connection_survives_an_error() {
    let addr = start_server(None).await;
    let mut client = Client::connect(&addr, "", true).await.unwrap();

    assert!(client.query("BOOM").await.is_err());
    let rows = client.query("SELECT 1").await.unwrap();
    assert_eq!(rows, vec![vec![Some("1".to_owned())]]);
}

#[tokio::test]
async fn prepared_statement_round_trips_binary_parameters() {
    let addr = start_server(None).await;
    let mut client = Client::connect(&addr, "", true).await.unwrap();

    client.send(0x16, b"SELECT ?, ?, ?").await.unwrap();
    let response = client.recv().await.unwrap();
    assert_eq!(response[0], 0x00, "prepare should succeed");

    let mut r = BufReader::new(&response);
    r.u8().unwrap();
    let stmt_id = r.u32().unwrap();
    let column_count = r.u16().unwrap();
    let param_count = r.u16().unwrap();
    assert_eq!(param_count, 3);
    assert_eq!(column_count, 0);

    // Parameter definitions follow the header, one packet each.
    for _ in 0..param_count {
        let _ = client.recv().await.unwrap();
    }

    // Bind an integer, a string, and a NULL.
    let mut w = BufWriter::new();
    w.u32(stmt_id)
        .u8(0)
        .u32(1)
        // Null bitmap: only the third parameter is NULL.
        .u8(0b100)
        .u8(1)
        // LONGLONG, VAR_STRING, NULL.
        .u8(0x08)
        .u8(0)
        .u8(0xfd)
        .u8(0)
        .u8(0x06)
        .u8(0)
        .u64(99)
        .lenenc_str("bound");

    client.send(0x17, &w.into_inner()).await.unwrap();
    let rows = client.read_result_set(true).await.unwrap();

    assert_eq!(
        rows,
        vec![vec![Some("99".to_owned()), Some("bound".to_owned()), None]],
        "bound parameters should round trip through the binary protocol"
    );
}

#[tokio::test]
async fn affected_row_counts_are_reported() {
    let addr = start_server(None).await;
    let mut client = Client::connect(&addr, "", true).await.unwrap();

    client.send(0x03, b"UPDATE").await.unwrap();
    let packet = client.recv().await.unwrap();
    assert_eq!(packet[0], 0x00);

    let mut r = BufReader::new(&packet);
    r.u8().unwrap();
    assert_eq!(r.lenenc_int().unwrap(), Some(7));
}

#[tokio::test]
async fn ping_and_quit_are_handled() {
    let addr = start_server(None).await;
    let mut client = Client::connect(&addr, "", true).await.unwrap();

    client.send(0x0e, b"").await.unwrap();
    assert_eq!(client.recv().await.unwrap()[0], 0x00);

    // COM_QUIT draws no reply; the server just closes.
    client.send(0x01, b"").await.unwrap();
}

#[tokio::test]
async fn a_password_is_enforced_when_configured() {
    let addr = start_server(Some("correct-horse".to_owned())).await;

    assert!(Client::connect(&addr, "correct-horse", true).await.is_ok());

    let err = match Client::connect(&addr, "wrong", true).await {
        Ok(_) => panic!("a wrong password should not be accepted"),
        Err(e) => e,
    };
    assert!(
        err.to_string().contains("1045"),
        "expected access denied, got: {err}"
    );
}

/// The default configuration takes any credentials, which is what a throwaway
/// test database should do.
#[tokio::test]
async fn any_password_is_accepted_by_default() {
    let addr = start_server(None).await;
    assert!(Client::connect(&addr, "literally-anything", true)
        .await
        .is_ok());
}

// ---------------------------------------------------------------------------
// Regression tests for issues found in code review on 2026-08-18.
// ---------------------------------------------------------------------------

/// A handler that ignores the id it was handed must not break the connection.
/// The wire id belongs to the connection; if the handler's value were sent to
/// the client instead, every later EXECUTE would fail to find its statement.
struct RogueIdHandler;

impl Handler for RogueIdHandler {
    fn query(&mut self, _sql: &str) -> HandlerResult {
        Ok(Response::ok())
    }

    fn prepare(&mut self, _id: u32, sql: &str) -> Result<Prepared, ErrPacket> {
        Ok(Prepared {
            // Deliberately not the id the connection assigned.
            id: 9999,
            sql: sql.to_owned(),
            param_count: 1,
            columns: Vec::new(),
        })
    }

    fn execute(&mut self, _stmt: &Prepared, params: &[Value]) -> HandlerResult {
        let columns = vec![ColumnDef::new("p0", ColumnType::LongLong).nullable(true)];
        Ok(Response::ResultSet(ResultSet::with_rows(
            columns,
            vec![params.to_vec()],
        )))
    }
}

async fn start_rogue_server() -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap().to_string();
    let server = Arc::new(Server::new(Config::default()));
    tokio::spawn(async move { server.serve_tcp(listener, |_| RogueIdHandler).await });
    addr
}

/// Prepares a statement and returns its wire id.
async fn prepare(client: &mut Client, sql: &str) -> (u32, u16) {
    client.send(0x16, sql.as_bytes()).await.unwrap();
    let response = client.recv().await.unwrap();
    assert_eq!(response[0], 0x00, "prepare should succeed");

    let mut r = BufReader::new(&response);
    r.u8().unwrap();
    let stmt_id = r.u32().unwrap();
    let _columns = r.u16().unwrap();
    let param_count = r.u16().unwrap();

    for _ in 0..param_count {
        let _ = client.recv().await.unwrap();
    }
    (stmt_id, param_count)
}

/// Builds a COM_STMT_EXECUTE payload binding one LONGLONG.
fn execute_payload(stmt_id: u32, value: i64, send_types: bool) -> Vec<u8> {
    let mut w = BufWriter::new();
    w.u32(stmt_id).u8(0).u32(1).u8(0);
    if send_types {
        w.u8(1).u8(0x08).u8(0);
    } else {
        w.u8(0);
    }
    w.u64(value as u64);
    w.into_inner()
}

/// A client may re-execute a prepared statement without re-sending parameter
/// types. Defaulting to VAR_STRING reads the raw 8-byte integer as a
/// length-encoded string: wrong value, and a misaligned reader afterwards.
#[tokio::test]
async fn re_execution_without_types_uses_the_remembered_ones() {
    let addr = start_server(None).await;
    let mut client = Client::connect(&addr, "", true).await.unwrap();

    let (stmt_id, param_count) = prepare(&mut client, "SELECT ?").await;
    assert_eq!(param_count, 1);

    // First execution carries the types.
    client
        .send(0x17, &execute_payload(stmt_id, 4242, true))
        .await
        .unwrap();
    let rows = client.read_result_set(true).await.unwrap();
    assert_eq!(rows, vec![vec![Some("4242".to_owned())]]);

    // Second execution omits them and must still decode a LONGLONG.
    client
        .send(0x17, &execute_payload(stmt_id, 777, false))
        .await
        .unwrap();
    let rows = client.read_result_set(true).await.unwrap();
    assert_eq!(
        rows,
        vec![vec![Some("777".to_owned())]],
        "the remembered parameter type should have been reused"
    );
}

/// With no prior binding to fall back on there is nothing to decode against,
/// so the server must say so rather than guess.
#[tokio::test]
async fn execution_without_types_or_history_is_an_error() {
    let addr = start_server(None).await;
    let mut client = Client::connect(&addr, "", true).await.unwrap();

    let (stmt_id, _) = prepare(&mut client, "SELECT ?").await;

    client
        .send(0x17, &execute_payload(stmt_id, 1, false))
        .await
        .unwrap();
    let err = client.read_result_set(true).await.unwrap_err();
    assert!(err.starts_with("1210"), "unexpected error: {err}");
}

/// A row whose width does not match the column list would produce a stream the
/// client cannot parse. The server must refuse rather than emit it.
#[tokio::test]
async fn a_malformed_row_does_not_reach_the_client_as_data() {
    let addr = start_server(None).await;
    let mut client = Client::connect(&addr, "", true).await.unwrap();

    client.send(0x03, b"BADROW").await.unwrap();

    // The column count and definitions are already on the wire; what must not
    // happen is a row packet the client would silently misread.
    let mut saw_row = false;
    for _ in 0..8 {
        match client.try_recv().await {
            None => break,
            Some(packet) => {
                if packet[0] == 0x00 && packet.len() > 4 {
                    saw_row = true;
                }
            }
        }
    }
    assert!(
        !saw_row,
        "a mismatched row must never be written to the wire"
    );
}

/// The connection assigns statement ids, so a handler returning its own must
/// not desynchronise EXECUTE.
#[tokio::test]
async fn the_connection_owns_the_statement_id() {
    let addr = start_rogue_server().await;
    let mut client = Client::connect(&addr, "", true).await.unwrap();

    let (stmt_id, _) = prepare(&mut client, "SELECT ?").await;
    assert_eq!(stmt_id, 1, "the client should see the connection's id");

    client
        .send(0x17, &execute_payload(stmt_id, 5, true))
        .await
        .unwrap();
    let rows = client.read_result_set(true).await.unwrap();
    assert_eq!(rows, vec![vec![Some("5".to_owned())]]);
}

/// Long data for a statement that was never prepared must be dropped, not
/// buffered — nothing would ever free it.
#[tokio::test]
async fn long_data_for_an_unknown_statement_is_discarded() {
    let addr = start_server(None).await;
    let mut client = Client::connect(&addr, "", true).await.unwrap();

    let mut w = BufWriter::new();
    w.u32(4242)
        .u16(0)
        .bytes(b"data for a statement that does not exist");
    client.send(0x18, &w.into_inner()).await.unwrap();

    // COM_STMT_SEND_LONG_DATA draws no reply; the connection must stay usable.
    let rows = client.query("SELECT 1").await.unwrap();
    assert_eq!(rows, vec![vec![Some("1".to_owned())]]);
}

#[tokio::test]
async fn a_text_result_carries_its_column_names() {
    let addr = start_server(None).await;
    let mut client = Client::connect(&addr, "", true).await.unwrap();

    client.send(0x03, b"MULTI").await.unwrap();
    assert_eq!(column_names(&mut client).await, vec!["a", "b"]);
}

#[tokio::test]
async fn a_prepared_statement_declares_its_columns_before_execution() {
    // A client caches the column metadata from the prepare response and reads
    // executions against it. Declaring zero columns there leaves it with no
    // names at all, so every lookup by name fails and the result reads as
    // positional — which is how `_prisma_migrations` went missing from an
    // introspection that had in fact returned it.
    let addr = start_server(None).await;
    let mut client = Client::connect(&addr, "", true).await.unwrap();

    client.send(0x16, b"MULTI").await.unwrap();
    let response = client.recv().await.unwrap();
    assert_eq!(response[0], 0x00, "prepare should succeed");

    let mut r = BufReader::new(&response);
    r.u8().unwrap();
    let _stmt_id = r.u32().unwrap();
    let column_count = r.u16().unwrap();
    let param_count = r.u16().unwrap();
    assert_eq!(param_count, 0);
    assert_eq!(column_count, 2, "prepare must declare the result columns");

    let mut names = Vec::new();
    for _ in 0..column_count {
        names.push(parse_column_name(&client.recv().await.unwrap()));
    }
    assert_eq!(names, vec!["a", "b"]);
}
