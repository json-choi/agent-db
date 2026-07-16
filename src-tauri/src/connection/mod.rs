//! Connection management: live sqlx pools (a separate read-only pool per
//! connection), OS credential-store secret storage, and per-provider connection-string
//! tuning. Credentials live only in the credential store; the MCP tool surface operates
//! on connection ids and never exposes stored secrets.

pub mod keychain;
pub mod pool;
pub mod providers;

pub use crate::driver::connect;
pub use keychain::{delete_secret, fetch_secret, store_secret};
pub use pool::{DbPool, LiveConnection};

/// The executor module refers to the engine-tagged pool enum as `Pool`; keep a
/// single definition (`DbPool`) and expose this alias so both names resolve.
pub use pool::DbPool as Pool;

use crate::error::{AppError, AppResult};

/// One open connection of either family: the sqlx SQL stack or the MongoDB
/// document adapter. Callers pull this out of the shared connection map and
/// downcast with [`Live::sql`]/[`Live::mongo`] — a family mismatch is a hard,
/// fail-closed error, never a silent fallthrough.
#[derive(Clone)]
pub enum Live {
    Sql(LiveConnection),
    Mongo(crate::mongo::MongoConnection),
}

impl Live {
    /// The sqlx side of this connection; clear error for document databases.
    pub fn sql(&self) -> AppResult<&LiveConnection> {
        match self {
            Live::Sql(live) => Ok(live),
            Live::Mongo(_) => Err(AppError::Config(
                "this is a MongoDB document connection — SQL operations are not available on it"
                    .into(),
            )),
        }
    }

    /// The MongoDB side of this connection; clear error for SQL engines.
    pub fn mongo(&self) -> AppResult<&crate::mongo::MongoConnection> {
        match self {
            Live::Mongo(conn) => Ok(conn),
            Live::Sql(_) => Err(AppError::Config(
                "this is a SQL connection — document operations are not available on it".into(),
            )),
        }
    }

    /// Liveness probe against the live server.
    pub async fn test(&self) -> AppResult<()> {
        match self {
            Live::Sql(live) => live.test().await,
            Live::Mongo(conn) => conn.ping().await,
        }
    }
}
