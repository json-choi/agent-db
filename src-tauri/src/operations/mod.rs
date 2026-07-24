//! Authoritative Operation Runtime contracts. Adapters may request transitions,
//! but only this module decides whether a stored operation can change state.

#[allow(
    dead_code,
    reason = "activated by the exact-approval runtime adapter in FND-04"
)]
mod canonicalize;
#[allow(
    dead_code,
    reason = "activated by the exact-approval runtime adapter in FND-04"
)]
pub(crate) mod model;
#[allow(
    dead_code,
    reason = "activated by the exact-approval runtime adapter in FND-04"
)]
pub(crate) mod repository;
pub mod state_machine;

pub use dopedb_protocol::{
    OperationActorKind, OperationEventKind, OperationKind, OperationRiskLevel, OperationState,
};
pub use state_machine::{ensure_transition, restart_recovery, RestartRecovery, TransitionError};
