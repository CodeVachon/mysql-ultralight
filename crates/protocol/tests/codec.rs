//! Round-trip tests for the wire codec.
//!
//! Framing and length-encoded integers are where desynchronisation bugs come
//! from, and a desynchronised connection fails far away from its cause, so
//! these are checked directly.

use mysql_ultralight_protocol::buf::{BufReader, BufWriter};
use mysql_ultralight_protocol::capabilities::Capabilities;
use mysql_ultralight_protocol::handshake::{Handshake, HandshakeResponse};
use mysql_ultralight_protocol::packet::{
    read_packet, write_packet, ErrPacket, OkPacket, Sequence, MAX_PAYLOAD,
};
use mysql_ultralight_protocol::value::{bitmap_is_null, null_bitmap};
use mysql_ultralight_protocol::Value;

#[test]
fn lenenc_int_round_trips_across_every_width() {
    // Boundaries between the 1, 3, 4 and 9 byte encodings.
    let cases = [
        0,
        1,
        0xfa,
        0xfb,
        0xff,
        0x100,
        0xffff,
        0x1_0000,
        0xff_ffff,
        0x100_0000,
        u64::MAX,
    ];

    for value in cases {
        let mut w = BufWriter::new();
        w.lenenc_int(value);
        let encoded = w.into_inner();

        let mut r = BufReader::new(&encoded);
        assert_eq!(
            r.lenenc_int().unwrap(),
            Some(value),
            "value {value} did not round trip"
        );
        assert_eq!(r.remaining(), 0, "value {value} left trailing bytes");
    }
}

#[test]
fn lenenc_int_encodings_use_the_expected_widths() {
    let width = |v: u64| {
        let mut w = BufWriter::new();
        w.lenenc_int(v);
        w.into_inner().len()
    };

    assert_eq!(width(0xfa), 1);
    // 0xFB is the NULL marker, so 251 must escape to the wide form.
    assert_eq!(width(0xfb), 3);
    assert_eq!(width(0xffff), 3);
    assert_eq!(width(0x1_0000), 4);
    assert_eq!(width(0xff_ffff), 4);
    assert_eq!(width(0x100_0000), 9);
}

#[test]
fn lenenc_null_marker_decodes_as_none() {
    let encoded = [0xfbu8];
    let mut r = BufReader::new(&encoded);
    assert_eq!(r.lenenc_int().unwrap(), None);
}

#[test]
fn reader_reports_truncation_rather_than_panicking() {
    let mut r = BufReader::new(&[0x01]);
    assert!(r.u32().is_err());

    let mut r = BufReader::new(&[0xfc, 0x01]);
    assert!(r.lenenc_int().is_err());

    // A length-encoded string whose length outruns the payload.
    let mut r = BufReader::new(&[0x05, b'a', b'b']);
    assert!(r.lenenc_bytes().is_err());

    // A null-terminated string with no terminator.
    let mut r = BufReader::new(b"unterminated");
    assert!(r.null_bytes().is_err());
}

#[tokio::test]
async fn packet_framing_round_trips() {
    let payload = b"SELECT 1".to_vec();
    let mut buffer = Vec::new();
    let mut seq = Sequence::new();

    write_packet(&mut buffer, &mut seq, &payload).await.unwrap();

    // 4 byte header plus the payload.
    assert_eq!(buffer.len(), 4 + payload.len());
    assert_eq!(buffer[3], 0, "first packet must carry sequence 0");

    let mut cursor = std::io::Cursor::new(buffer);
    let mut read_seq = Sequence::new();
    let decoded = read_packet(&mut cursor, &mut read_seq, 1024).await.unwrap();
    assert_eq!(decoded, Some(payload));
}

#[tokio::test]
async fn empty_payload_still_produces_one_packet() {
    let mut buffer = Vec::new();
    let mut seq = Sequence::new();
    write_packet(&mut buffer, &mut seq, &[]).await.unwrap();

    assert_eq!(buffer, vec![0, 0, 0, 0]);

    let mut cursor = std::io::Cursor::new(buffer);
    let mut read_seq = Sequence::new();
    assert_eq!(
        read_packet(&mut cursor, &mut read_seq, 1024).await.unwrap(),
        Some(Vec::new())
    );
}

#[tokio::test]
async fn oversized_payload_splits_and_reassembles() {
    // Exactly MAX_PAYLOAD must be followed by an empty terminator packet, or
    // the reader would wait forever for a continuation.
    let payload = vec![0x41u8; MAX_PAYLOAD];
    let mut buffer = Vec::new();
    let mut seq = Sequence::new();
    write_packet(&mut buffer, &mut seq, &payload).await.unwrap();

    assert_eq!(
        buffer.len(),
        4 + MAX_PAYLOAD + 4,
        "expected a trailing empty packet"
    );

    let mut cursor = std::io::Cursor::new(buffer);
    let mut read_seq = Sequence::new();
    let decoded = read_packet(&mut cursor, &mut read_seq, 128 * 1024 * 1024)
        .await
        .unwrap();
    assert_eq!(decoded, Some(payload));
}

#[tokio::test]
async fn a_message_larger_than_the_cap_is_rejected() {
    let payload = vec![0x41u8; 4096];
    let mut buffer = Vec::new();
    let mut seq = Sequence::new();
    write_packet(&mut buffer, &mut seq, &payload).await.unwrap();

    let mut cursor = std::io::Cursor::new(buffer);
    let mut read_seq = Sequence::new();
    assert!(read_packet(&mut cursor, &mut read_seq, 1024).await.is_err());
}

#[tokio::test]
async fn clean_eof_at_a_packet_boundary_is_not_an_error() {
    let mut cursor = std::io::Cursor::new(Vec::new());
    let mut seq = Sequence::new();
    assert_eq!(
        read_packet(&mut cursor, &mut seq, 1024).await.unwrap(),
        None
    );
}

#[test]
fn sequence_numbers_advance_and_wrap() {
    let mut seq = Sequence::new();
    assert_eq!(seq.advance(), 0);
    assert_eq!(seq.advance(), 1);

    seq.set(255);
    assert_eq!(seq.advance(), 255);
    assert_eq!(seq.advance(), 0, "sequence must wrap rather than overflow");
}

#[test]
fn handshake_round_trips_through_a_client_response() {
    let greeting = Handshake::new("8.3.0-mysql-ultralight", 7);
    let encoded = greeting.encode();

    assert_eq!(encoded[0], 10, "protocol version must be 10");
    // The nonce is transmitted as a null-terminated string, so no byte in it
    // may be zero.
    assert!(greeting.nonce.iter().all(|&b| b != 0));

    let caps = Capabilities::PROTOCOL_41
        | Capabilities::SECURE_CONNECTION
        | Capabilities::PLUGIN_AUTH
        | Capabilities::CONNECT_WITH_DB;

    let mut w = BufWriter::new();
    w.u32(caps.bits())
        .u32(16 * 1024 * 1024)
        .u8(45)
        .zeros(23)
        .null_str("tester")
        .u8(4)
        .bytes(b"scrm")
        .null_str("appdb")
        .null_str("mysql_native_password");

    let response = HandshakeResponse::decode(&w.into_inner()).unwrap();
    assert_eq!(response.username, "tester");
    assert_eq!(response.database.as_deref(), Some("appdb"));
    assert_eq!(
        response.auth_plugin.as_deref(),
        Some("mysql_native_password")
    );
    assert_eq!(response.auth_response, b"scrm");
    assert!(response.capabilities.contains(Capabilities::PROTOCOL_41));
}

#[test]
fn native_password_verification_accepts_the_right_scramble_only() {
    use mysql_ultralight_protocol::auth::{native_password_scramble, verify_native_password};

    let nonce = b"01234567890123456789";
    let scramble = native_password_scramble(b"hunter2", nonce);

    assert_eq!(scramble.len(), 20);
    assert!(verify_native_password(b"hunter2", nonce, &scramble));
    assert!(!verify_native_password(b"hunter3", nonce, &scramble));
    assert!(!verify_native_password(
        b"hunter2",
        b"98765432109876543210",
        &scramble
    ));

    // An empty password produces an empty scramble on both sides.
    assert!(verify_native_password(b"", nonce, &[]));
}

#[test]
fn ok_and_err_packets_carry_their_headers() {
    let caps = Capabilities::PROTOCOL_41 | Capabilities::SESSION_TRACK;

    let ok = OkPacket::new(3, 42).encode(caps);
    assert_eq!(ok[0], 0x00);

    // With DEPRECATE_EOF the same body is sent under a 0xFE header.
    let eof = OkPacket::default().encode_as_eof(caps);
    assert_eq!(eof[0], 0xfe);

    let err = ErrPacket::parse_error("bad syntax").encode(caps);
    assert_eq!(err[0], 0xff);
    assert_eq!(u16::from_le_bytes([err[1], err[2]]), 1064);
    assert_eq!(err[3], b'#');
    assert_eq!(&err[4..9], b"42000");
}

#[test]
fn text_protocol_encodes_null_as_the_fb_marker() {
    let mut w = BufWriter::new();
    Value::Null.encode_text(&mut w);
    assert_eq!(w.into_inner(), vec![0xfb]);

    let mut w = BufWriter::new();
    Value::I64(-17).encode_text(&mut w);
    assert_eq!(w.into_inner(), vec![3, b'-', b'1', b'7']);
}

#[test]
fn binary_null_bitmap_respects_the_result_row_offset() {
    let row = vec![Value::Null, Value::I64(1), Value::Null];

    // Bound parameters have no offset.
    let params = null_bitmap(&row, 0);
    assert!(bitmap_is_null(&params, 0));
    assert!(!bitmap_is_null(&params, 1));
    assert!(bitmap_is_null(&params, 2));

    // Result rows reserve the first two bits, shifting every column along.
    let result = null_bitmap(&row, 2);
    assert!(!bitmap_is_null(&result, 0));
    assert!(bitmap_is_null(&result, 2));
    assert!(!bitmap_is_null(&result, 3));
    assert!(bitmap_is_null(&result, 4));
}

#[test]
fn binary_parameters_decode_by_type() {
    use mysql_ultralight_protocol::value::Value as V;

    // LONGLONG, signed.
    let longlong = (-5i64).to_le_bytes();
    let mut r = BufReader::new(&longlong);
    assert_eq!(V::decode_binary(&mut r, 0x08, false).unwrap(), V::I64(-5));

    // TINY, unsigned.
    let mut r = BufReader::new(&[200u8]);
    assert_eq!(V::decode_binary(&mut r, 0x01, true).unwrap(), V::U64(200));

    // TINY, signed: the same byte is negative.
    let mut r = BufReader::new(&[200u8]);
    assert_eq!(V::decode_binary(&mut r, 0x01, false).unwrap(), V::I64(-56));

    // A 7 byte DATETIME.
    let encoded = [7u8, 0xe8, 0x07, 3, 14, 15, 9, 26];
    let mut r = BufReader::new(&encoded);
    assert_eq!(
        V::decode_binary(&mut r, 0x0c, false).unwrap(),
        V::Str("2024-03-14 15:09:26".to_owned())
    );

    // VAR_STRING arrives length-encoded.
    let mut r = BufReader::new(&[5u8, b'h', b'e', b'l', b'l', b'o']);
    assert_eq!(
        V::decode_binary(&mut r, 0xfd, false).unwrap(),
        V::Str("hello".to_owned())
    );
}

// ---------------------------------------------------------------------------
// Regression tests for issues found in code review on 2026-08-18.
// ---------------------------------------------------------------------------

/// A client can send any `days` value it likes in a binary TIME parameter.
/// Computing the hour total in `u32` overflows and panics above 178_956_970,
/// which in a `panic = "abort"` release build takes the whole server down.
#[test]
fn a_huge_day_count_in_a_time_parameter_is_rejected_not_panicked_on() {
    let mut encoded = vec![12u8, 0];
    encoded.extend_from_slice(&u32::MAX.to_le_bytes());
    encoded.extend_from_slice(&[0, 0, 0]);
    encoded.extend_from_slice(&0u32.to_le_bytes());

    let mut r = BufReader::new(&encoded);
    assert!(Value::decode_binary(&mut r, 0x0b, false).is_err());
}

/// Only 0, 4, 7 and 11 are legal DATETIME lengths. Falling through to the
/// widest layout for anything else consumes bytes belonging to the next
/// parameter, silently corrupting the rest of the statement.
#[test]
fn an_illegal_datetime_length_is_rejected() {
    for bad_len in [1u8, 2, 3, 5, 6, 8, 9, 10, 12] {
        let mut encoded = vec![bad_len];
        encoded.extend_from_slice(&[0u8; 16]);
        let mut r = BufReader::new(&encoded);
        assert!(
            Value::decode_binary(&mut r, 0x0c, false).is_err(),
            "DATETIME length {bad_len} should be rejected"
        );
    }

    for good_len in [0u8, 4, 7, 11] {
        let mut encoded = vec![good_len];
        encoded.extend_from_slice(&[0u8; 16]);
        let mut r = BufReader::new(&encoded);
        assert!(
            Value::decode_binary(&mut r, 0x0c, false).is_ok(),
            "DATETIME length {good_len} should be accepted"
        );
    }
}

#[test]
fn an_illegal_time_length_is_rejected() {
    for bad_len in [1u8, 4, 7, 9, 11, 13] {
        let mut encoded = vec![bad_len];
        encoded.extend_from_slice(&[0u8; 16]);
        let mut r = BufReader::new(&encoded);
        assert!(
            Value::decode_binary(&mut r, 0x0b, false).is_err(),
            "TIME length {bad_len} should be rejected"
        );
    }
}

/// The binary protocol takes its width from the column's declared type. A
/// handler may hand back a `Value::I64` for a column declared `Long`; writing
/// 8 bytes where the client reads 4 desynchronises the whole result set.
#[test]
fn binary_encoding_width_follows_the_column_type_not_the_value() {
    use mysql_ultralight_protocol::ColumnType;

    let widths = [
        (ColumnType::Tiny, 1),
        (ColumnType::Short, 2),
        (ColumnType::Year, 2),
        (ColumnType::Long, 4),
        (ColumnType::Int24, 4),
        (ColumnType::LongLong, 8),
        (ColumnType::Float, 4),
        (ColumnType::Double, 8),
    ];

    for (ty, expected) in widths {
        let mut w = BufWriter::new();
        Value::I64(1).encode_binary(&mut w, ty);
        assert_eq!(w.into_inner().len(), expected, "wrong width for {ty:?}");
    }

    // Strings stay length-encoded.
    let mut w = BufWriter::new();
    Value::Str("abc".into()).encode_binary(&mut w, ColumnType::VarString);
    assert_eq!(w.into_inner(), vec![3, b'a', b'b', b'c']);

    // NULL occupies no bytes; the row's bitmap carries it.
    let mut w = BufWriter::new();
    Value::Null.encode_binary(&mut w, ColumnType::LongLong);
    assert!(w.into_inner().is_empty());
}

/// A pre-4.1 client sends a different handshake layout. Parsing it as
/// HandshakeResponse41 yields a garbage username and an auth response that
/// cannot be verified, so the connection must be refused instead.
#[test]
fn a_handshake_response_without_protocol_41_is_refused() {
    let mut w = BufWriter::new();
    w.u32(Capabilities::SECURE_CONNECTION.bits())
        .u32(16 * 1024 * 1024)
        .u8(45)
        .zeros(23)
        .null_str("tester")
        .u8(0);

    assert!(HandshakeResponse::decode(&w.into_inner()).is_err());
}

/// Multi-statement commands are advertised because Prisma's migration engine
/// sends a whole migration file as one command. PS_MULTI_RESULTS is not: a
/// prepared statement here is always one statement, so a client must never be
/// told to look for a second result from `COM_STMT_EXECUTE`.
#[test]
fn multi_statement_capabilities_match_what_the_command_loop_does() {
    let caps = Capabilities::server_default();

    assert!(caps.contains(Capabilities::MULTI_STATEMENTS));
    assert!(caps.contains(Capabilities::MULTI_RESULTS));
    assert!(!caps.contains(Capabilities::PS_MULTI_RESULTS));

    // The capabilities the protocol genuinely depends on are still there.
    assert!(caps.contains(Capabilities::PROTOCOL_41));
    assert!(caps.contains(Capabilities::PLUGIN_AUTH));
    assert!(caps.contains(Capabilities::DEPRECATE_EOF));
}

/// A client can store arbitrary text in a DATETIME column, so the binary
/// encoder must survive a fraction that is not digits. Counting characters and
/// then measuring bytes overflowed the scale subtraction and panicked.
#[test]
fn a_datetime_with_a_multibyte_fraction_does_not_panic() {
    use mysql_ultralight_protocol::ColumnType;

    for (text, ty) in [
        ("2024-01-01 00:00:00.ééééé", ColumnType::DateTime),
        ("10:00:00.ééééé", ColumnType::Time),
        ("not a date at all", ColumnType::DateTime),
        ("", ColumnType::DateTime),
    ] {
        let mut w = BufWriter::new();
        Value::Str(text.to_owned()).encode_binary(&mut w, ty);
        assert!(!w.into_inner().is_empty(), "{text} produced no bytes");
    }
}

#[test]
fn a_short_fraction_scales_up_rather_than_truncating() {
    use mysql_ultralight_protocol::ColumnType;

    let mut w = BufWriter::new();
    Value::Str("2024-01-01 00:00:00.5".to_owned()).encode_binary(&mut w, ColumnType::DateTime);
    let bytes = w.into_inner();
    assert_eq!(bytes[0], 11, "a fractional datetime uses the 11 byte form");
    // .5 seconds is 500000 microseconds, not 5.
    assert_eq!(
        u32::from_le_bytes([bytes[8], bytes[9], bytes[10], bytes[11]]),
        500_000
    );
}
