//! An in-memory, MySQL-compatible SQL engine.
//!
//! Knows nothing about sockets or packets: it takes SQL text and bound
//! parameters and returns rows. The wire protocol lives in sibling crates.

pub mod catalog;
pub mod ddl;
pub mod error;
pub mod exec;
pub mod expr;
pub mod functions;
pub mod info_schema;
pub mod params;
pub mod relation;
pub mod result;
pub mod session;
pub mod types;
pub mod value;

pub use catalog::{Catalog, Column, Database, Table};
pub use error::{EngineError, EngineResult};
pub use exec::Engine;
pub use result::{OutColumn, Outcome, ResultSet};
pub use session::{ServerConfig, Session};
pub use types::{SqlType, TypeKind};
pub use value::Value;
