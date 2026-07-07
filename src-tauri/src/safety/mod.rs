//! The 4-layer safety engine.
//!
//! Governing principle: **L1 is a UX pre-filter; L2 (the database's own
//! read-only session) is the authoritative security boundary.** A parser cannot
//! see through functions, writable CTEs, or dialect quirks — so nothing in this
//! module trusts the model, and every read runs inside a DB-enforced read-only
//! session that rejects a write even when L1 misclassified it.
//!
//! - [`l1_parse`] parse & classify (sqlparser, per-engine dialect)
//! - [`l2_enforce`] authoritative read-only execution
//! - [`l3_preview`] EXPLAIN / execute+rollback impact preview
//! - [`l4_gate`] human-approval decision (the approval card is built frontend-side)

pub mod l1_parse;
pub mod l2_enforce;
pub mod l3_preview;
pub mod l4_gate;

pub use l1_parse::classify;
pub use l2_enforce::run_read_only;
pub use l3_preview::preview;
pub use l4_gate::{decide, GateDecision};

use crate::model::Engine;

/// A borrowed handle to one live target-database pool.
///
/// The engine is carried by the variant, so callers never pass `engine`
/// separately — the pool is the single source of truth. The `connection`
/// module constructs this from its `LiveConnection`; the safety engine only
/// borrows it. (For SQLite the read path relies on the connection module
/// having opened a `read_only(true)` pool; L2 additionally sets
/// `PRAGMA query_only` as belt-and-suspenders.)
#[derive(Clone, Copy)]
pub enum PoolRef<'a> {
    Postgres(&'a sqlx::PgPool),
    Mysql(&'a sqlx::MySqlPool),
    Sqlite(&'a sqlx::SqlitePool),
}

impl PoolRef<'_> {
    pub fn engine(&self) -> Engine {
        match self {
            PoolRef::Postgres(_) => Engine::Postgres,
            PoolRef::Mysql(_) => Engine::Mysql,
            PoolRef::Sqlite(_) => Engine::Sqlite,
        }
    }
}

/// Session statement-timeout applied by L2/L3 (ms). Bounds runaway queries at the
/// DB level (PG `statement_timeout`, MySQL `max_execution_time`); SQLite has no
/// server timeout so L2/L3 add a wall-clock guard.
// ponytail: one fixed budget for the whole app; make it a per-connection setting
// only if a real workload needs longer previews.
pub const STATEMENT_TIMEOUT_MS: u64 = 15_000;
