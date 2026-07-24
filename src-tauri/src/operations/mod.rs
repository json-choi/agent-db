//! Authoritative Operation Runtime contracts. Adapters may request transitions,
//! but only this module decides whether a stored operation can change state.
//!
//! The execution capability is deliberately inaccessible to external adapters:
//!
//! ```compile_fail
//! use app_lib::operations::ExecutionGrant;
//! ```

mod canonicalize;
mod execute;
#[allow(
    dead_code,
    reason = "the durable projection includes timestamps and ledger DTOs consumed by the upcoming broker operation-status adapter"
)]
mod model;
#[allow(
    dead_code,
    reason = "ledger read/progress APIs are already migration-tested and become production entry points in CLI-01"
)]
mod repository;
#[allow(
    dead_code,
    reason = "runtime status/progress accessors are reserved for the upcoming broker and job adapters"
)]
mod runtime;
pub mod state_machine;

pub(crate) use canonicalize::canonical_hash;
pub use dopedb_protocol::{
    OperationActorKind, OperationEventKind, OperationKind, OperationRiskLevel, OperationState,
};
pub(crate) use execute::ExecutionGrant;
pub(crate) use model::{
    NewOperation, OperationActor, OperationActorProvenance, OperationApprover, OperationRecord,
};
pub(crate) use runtime::{
    ClaimedOperation, ExactApprovalRequest, LocalApprovalAuthority, OperationPlanDisposition,
    OperationRuntime,
};
pub use state_machine::{ensure_transition, restart_recovery, RestartRecovery, TransitionError};
