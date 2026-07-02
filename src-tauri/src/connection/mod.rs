//! Connection management: live sqlx pools (a separate read-only pool per
//! connection), macOS Keychain secret storage, and per-provider connection-string
//! tuning. Credentials live only in the Keychain; the MCP tool surface operates
//! on connection ids and never exposes stored secrets.

pub mod keychain;
pub mod pool;
pub mod providers;

pub use keychain::{delete_secret, fetch_secret, store_secret};
pub use pool::{connect, DbPool, LiveConnection};

/// The executor module refers to the engine-tagged pool enum as `Pool`; keep a
/// single definition (`DbPool`) and expose this alias so both names resolve.
pub use pool::DbPool as Pool;
