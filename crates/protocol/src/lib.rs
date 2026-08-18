//! The MySQL 8 client/server wire protocol.
//!
//! This crate is transport and engine agnostic: it turns bytes into protocol
//! structures and back, and knows nothing about SQL, storage, or sockets
//! beyond the async read/write traits used for framing.

pub mod auth;
pub mod buf;
pub mod capabilities;
pub mod column;
pub mod handshake;
pub mod packet;
pub mod value;

pub use buf::{BufReader, BufWriter};
pub use capabilities::{status, Capabilities};
pub use column::{ColumnDef, ColumnType};
pub use handshake::{Handshake, HandshakeResponse};
pub use packet::{ErrPacket, OkPacket, Sequence};
pub use value::Value;

/// Client command bytes, the first byte of every packet in the command phase.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Command {
    Quit,
    InitDb,
    Query,
    FieldList,
    Ping,
    Statistics,
    ProcessInfo,
    ProcessKill,
    Debug,
    ChangeUser,
    ResetConnection,
    SetOption,
    StmtPrepare,
    StmtExecute,
    StmtSendLongData,
    StmtClose,
    StmtReset,
    StmtFetch,
    Unknown(u8),
}

impl Command {
    pub fn from_byte(b: u8) -> Self {
        match b {
            0x01 => Command::Quit,
            0x02 => Command::InitDb,
            0x03 => Command::Query,
            0x04 => Command::FieldList,
            0x0e => Command::Ping,
            0x09 => Command::Statistics,
            0x0a => Command::ProcessInfo,
            0x0c => Command::ProcessKill,
            0x0d => Command::Debug,
            0x11 => Command::ChangeUser,
            0x1f => Command::ResetConnection,
            0x1b => Command::SetOption,
            0x16 => Command::StmtPrepare,
            0x17 => Command::StmtExecute,
            0x18 => Command::StmtSendLongData,
            0x19 => Command::StmtClose,
            0x1a => Command::StmtReset,
            0x1c => Command::StmtFetch,
            other => Command::Unknown(other),
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Command::Quit => "COM_QUIT",
            Command::InitDb => "COM_INIT_DB",
            Command::Query => "COM_QUERY",
            Command::FieldList => "COM_FIELD_LIST",
            Command::Ping => "COM_PING",
            Command::Statistics => "COM_STATISTICS",
            Command::ProcessInfo => "COM_PROCESS_INFO",
            Command::ProcessKill => "COM_PROCESS_KILL",
            Command::Debug => "COM_DEBUG",
            Command::ChangeUser => "COM_CHANGE_USER",
            Command::ResetConnection => "COM_RESET_CONNECTION",
            Command::SetOption => "COM_SET_OPTION",
            Command::StmtPrepare => "COM_STMT_PREPARE",
            Command::StmtExecute => "COM_STMT_EXECUTE",
            Command::StmtSendLongData => "COM_STMT_SEND_LONG_DATA",
            Command::StmtClose => "COM_STMT_CLOSE",
            Command::StmtReset => "COM_STMT_RESET",
            Command::StmtFetch => "COM_STMT_FETCH",
            Command::Unknown(_) => "COM_UNKNOWN",
        }
    }
}
