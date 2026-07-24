//! Authoritative Operation Runtime contracts. Adapters may request transitions,
//! but only this module decides whether a stored operation can change state.
//!
//! The execution capability is deliberately inaccessible to external adapters:
//!
//! ```compile_fail
//! use app_lib::operations::ExecutionGrant;
//! ```

#[allow(
    dead_code,
    reason = "activated by the exact-approval runtime adapter in FND-04"
)]
mod canonicalize;
#[allow(
    dead_code,
    reason = "activated by target executor cutover in the next FND-04 slice"
)]
mod execute;
#[allow(
    dead_code,
    reason = "activated by the exact-approval runtime adapter in FND-04"
)]
mod model;
#[allow(
    dead_code,
    reason = "activated by the exact-approval runtime adapter in FND-04"
)]
mod repository;
#[allow(
    dead_code,
    reason = "activated by Tauri and CLI adapters in the next FND-04 slice"
)]
mod runtime;
pub mod state_machine;

pub use dopedb_protocol::{
    OperationActorKind, OperationEventKind, OperationKind, OperationRiskLevel, OperationState,
};
#[allow(
    unused_imports,
    reason = "consumed by service adapters in the next FND-04 slice"
)]
pub(crate) use execute::ExecutionGrant;
#[allow(
    unused_imports,
    reason = "consumed by service adapters in the next FND-04 slice"
)]
pub(crate) use model::{
    NewOperation, OperationActor, OperationActorProvenance, OperationApprover, OperationRecord,
};
#[allow(
    unused_imports,
    reason = "consumed by service adapters in the next FND-04 slice"
)]
pub(crate) use runtime::{
    ClaimedOperation, ExactApprovalRequest, LocalApprovalAuthority, OperationPlanDisposition,
    OperationRuntime,
};
pub use state_machine::{ensure_transition, restart_recovery, RestartRecovery, TransitionError};
