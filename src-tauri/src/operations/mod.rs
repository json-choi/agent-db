//! Authoritative Operation Runtime contracts. Adapters may request transitions,
//! but only this module decides whether a stored operation can change state.

pub mod state_machine;

pub use dopedb_protocol::{
    OperationActorKind, OperationEventKind, OperationKind, OperationRiskLevel, OperationState,
};
pub use state_machine::{ensure_transition, restart_recovery, RestartRecovery, TransitionError};
