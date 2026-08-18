//! Client/server capability flags negotiated during the handshake.
//!
//! The server advertises what it can do, the client replies with the subset it
//! wants, and the intersection governs the shape of every packet afterwards.
//! `DEPRECATE_EOF` in particular changes how result sets terminate.

/// A set of MySQL capability flags.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Capabilities(pub u32);

macro_rules! caps {
    ($($name:ident = $value:expr, $doc:literal;)*) => {
        impl Capabilities {
            $(
                #[doc = $doc]
                pub const $name: Capabilities = Capabilities($value);
            )*

            /// Renders the set as flag names, for logging handshakes.
            pub fn names(self) -> Vec<&'static str> {
                let mut out = Vec::new();
                $(
                    if self.contains(Capabilities::$name) {
                        out.push(stringify!($name));
                    }
                )*
                out
            }
        }
    };
}

caps! {
    LONG_PASSWORD = 0x0000_0001, "Use the improved version of Old Password Authentication.";
    FOUND_ROWS = 0x0000_0002, "Return the number of matched rows, not the number changed.";
    LONG_FLAG = 0x0000_0004, "Longer flags in column definitions.";
    CONNECT_WITH_DB = 0x0000_0008, "The handshake response carries a default schema.";
    NO_SCHEMA = 0x0000_0010, "Do not permit `db.table.column`.";
    COMPRESS = 0x0000_0020, "Compression protocol supported.";
    ODBC = 0x0000_0040, "Special handling of ODBC behaviour.";
    LOCAL_FILES = 0x0000_0080, "Can use LOAD DATA LOCAL.";
    IGNORE_SPACE = 0x0000_0100, "Ignore spaces before '('.";
    PROTOCOL_41 = 0x0000_0200, "The 4.1 protocol. Always required here.";
    INTERACTIVE = 0x0000_0400, "Use the interactive timeout.";
    SSL = 0x0000_0800, "Switch to TLS after the handshake.";
    IGNORE_SIGPIPE = 0x0000_1000, "Client only; unused server side.";
    TRANSACTIONS = 0x0000_2000, "Status flags carry transaction state.";
    RESERVED = 0x0000_4000, "Unused legacy 4.1 protocol flag.";
    SECURE_CONNECTION = 0x0000_8000, "4.1 authentication.";
    MULTI_STATEMENTS = 0x0001_0000, "Multiple statements per COM_QUERY.";
    MULTI_RESULTS = 0x0002_0000, "Multiple result sets from COM_QUERY.";
    PS_MULTI_RESULTS = 0x0004_0000, "Multiple result sets from COM_STMT_EXECUTE.";
    PLUGIN_AUTH = 0x0008_0000, "Authentication plugins are negotiated by name.";
    CONNECT_ATTRS = 0x0010_0000, "The handshake response carries connection attributes.";
    PLUGIN_AUTH_LENENC_CLIENT_DATA = 0x0020_0000, "Auth response is length-encoded.";
    CAN_HANDLE_EXPIRED_PASSWORDS = 0x0040_0000, "Client understands expired-password state.";
    SESSION_TRACK = 0x0080_0000, "OK packets carry session state changes.";
    DEPRECATE_EOF = 0x0100_0000, "Result sets end with an OK packet, not an EOF packet.";
    OPTIONAL_RESULTSET_METADATA = 0x0200_0000, "Column metadata may be omitted.";
    ZSTD_COMPRESSION_ALGORITHM = 0x0400_0000, "zstd compression supported.";
    QUERY_ATTRIBUTES = 0x0800_0000, "COM_QUERY may carry query attributes.";
}

impl Capabilities {
    pub const EMPTY: Capabilities = Capabilities(0);

    /// What this server advertises.
    ///
    /// Compression and TLS are deliberately absent: this engine is for local
    /// test processes, where both cost startup time and buy nothing.
    ///
    /// MULTI_STATEMENTS and MULTI_RESULTS are advertised: Prisma's migration
    /// engine sends each migration file as one command, so without them it
    /// cannot apply anything. The command loop sets
    /// `status::MORE_RESULTS_EXISTS` on every response but the last, which is
    /// what makes the extra results readable rather than silently lost.
    ///
    /// PS_MULTI_RESULTS stays absent: a prepared statement here is always one
    /// statement, so there is never a second result to announce.
    pub fn server_default() -> Self {
        Self::LONG_PASSWORD
            | Self::FOUND_ROWS
            | Self::LONG_FLAG
            | Self::CONNECT_WITH_DB
            | Self::NO_SCHEMA
            | Self::ODBC
            | Self::IGNORE_SPACE
            | Self::PROTOCOL_41
            | Self::INTERACTIVE
            | Self::TRANSACTIONS
            | Self::SECURE_CONNECTION
            | Self::PLUGIN_AUTH
            | Self::CONNECT_ATTRS
            | Self::PLUGIN_AUTH_LENENC_CLIENT_DATA
            | Self::CAN_HANDLE_EXPIRED_PASSWORDS
            | Self::SESSION_TRACK
            | Self::DEPRECATE_EOF
            | Self::MULTI_STATEMENTS
            | Self::MULTI_RESULTS
    }

    pub fn contains(self, other: Capabilities) -> bool {
        self.0 & other.0 == other.0
    }

    pub fn bits(self) -> u32 {
        self.0
    }

    pub fn lower(self) -> u16 {
        self.0 as u16
    }

    pub fn upper(self) -> u16 {
        (self.0 >> 16) as u16
    }

    pub fn from_parts(lower: u16, upper: u16) -> Self {
        Capabilities(((upper as u32) << 16) | lower as u32)
    }
}

impl std::ops::BitOr for Capabilities {
    type Output = Capabilities;
    fn bitor(self, rhs: Capabilities) -> Capabilities {
        Capabilities(self.0 | rhs.0)
    }
}

impl std::ops::BitOrAssign for Capabilities {
    fn bitor_assign(&mut self, rhs: Capabilities) {
        self.0 |= rhs.0;
    }
}

impl std::ops::BitAnd for Capabilities {
    type Output = Capabilities;
    fn bitand(self, rhs: Capabilities) -> Capabilities {
        Capabilities(self.0 & rhs.0)
    }
}

/// Server status flags returned in OK and EOF packets.
pub mod status {
    pub const IN_TRANS: u16 = 0x0001;
    pub const AUTOCOMMIT: u16 = 0x0002;
    pub const MORE_RESULTS_EXISTS: u16 = 0x0008;
    pub const NO_INDEX_USED: u16 = 0x0020;
    pub const CURSOR_EXISTS: u16 = 0x0040;
    pub const LAST_ROW_SENT: u16 = 0x0080;
    pub const DB_DROPPED: u16 = 0x0100;
    pub const NO_BACKSLASH_ESCAPES: u16 = 0x0200;
    pub const SESSION_STATE_CHANGED: u16 = 0x0400;
}
