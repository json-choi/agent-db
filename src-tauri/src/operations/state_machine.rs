//! Pure Operation lifecycle transition validation.

use dopedb_protocol::{OperationKind, OperationState};
use thiserror::Error;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
#[error("operation cannot transition from {from:?} to {to:?}")]
pub struct TransitionError {
    pub from: OperationState,
    pub to: OperationState,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RestartRecovery {
    KeepTerminal,
    Expire,
    MarkFailed,
    OutcomeUnknown,
    ValidateJobCheckpoint,
}

/// Validate a lifecycle transition. Persistence uses compare-and-swap after this
/// check, so two callers cannot both claim the same executable operation.
pub fn ensure_transition(from: OperationState, to: OperationState) -> Result<(), TransitionError> {
    let allowed = matches!(
        (from, to),
        (
            OperationState::Planned,
            OperationState::PendingApproval
                | OperationState::Ready
                | OperationState::Cancelled
                | OperationState::Expired
        ) | (
            OperationState::PendingApproval,
            OperationState::Approved
                | OperationState::Rejected
                | OperationState::Cancelled
                | OperationState::Expired
        ) | (
            OperationState::Ready | OperationState::Approved,
            OperationState::Executing | OperationState::Cancelled | OperationState::Expired
        ) | (
            OperationState::Executing,
            OperationState::Succeeded
                | OperationState::Failed
                | OperationState::Cancelled
                | OperationState::OutcomeUnknown
        )
    );
    if allowed {
        Ok(())
    } else {
        Err(TransitionError { from, to })
    }
}

/// Decide the only safe startup action for an operation owned by an older runtime.
/// The caller persists this projection before accepting new execution claims.
pub const fn restart_recovery(kind: OperationKind, state: OperationState) -> RestartRecovery {
    if state.is_terminal() {
        RestartRecovery::KeepTerminal
    } else if !matches!(state, OperationState::Executing) {
        RestartRecovery::Expire
    } else if kind.is_resumable_job() {
        RestartRecovery::ValidateJobCheckpoint
    } else if kind.may_mutate_target() {
        RestartRecovery::OutcomeUnknown
    } else {
        RestartRecovery::MarkFailed
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ALL_STATES: [OperationState; 11] = [
        OperationState::Planned,
        OperationState::PendingApproval,
        OperationState::Ready,
        OperationState::Approved,
        OperationState::Rejected,
        OperationState::Expired,
        OperationState::Cancelled,
        OperationState::Executing,
        OperationState::Succeeded,
        OperationState::Failed,
        OperationState::OutcomeUnknown,
    ];

    #[test]
    fn exact_happy_paths_are_allowed() {
        for path in [
            [
                OperationState::Planned,
                OperationState::Ready,
                OperationState::Executing,
                OperationState::Succeeded,
            ],
            [
                OperationState::Planned,
                OperationState::PendingApproval,
                OperationState::Approved,
                OperationState::Executing,
            ],
        ] {
            for states in path.windows(2) {
                ensure_transition(states[0], states[1]).unwrap();
            }
        }
    }

    #[test]
    fn terminal_states_never_transition() {
        for from in ALL_STATES.into_iter().filter(|state| state.is_terminal()) {
            for to in ALL_STATES {
                assert_eq!(
                    ensure_transition(from, to),
                    Err(TransitionError { from, to })
                );
            }
        }
    }

    #[test]
    fn execution_cannot_skip_planning_or_approval() {
        for from in [OperationState::Planned, OperationState::PendingApproval] {
            assert!(ensure_transition(from, OperationState::Executing).is_err());
        }
        assert!(
            ensure_transition(OperationState::PendingApproval, OperationState::Succeeded).is_err()
        );
    }

    #[test]
    fn mutation_with_unknown_commit_is_not_failed_or_retried() {
        assert_eq!(
            ensure_transition(OperationState::Executing, OperationState::OutcomeUnknown),
            Ok(())
        );
        assert!(
            ensure_transition(OperationState::OutcomeUnknown, OperationState::Executing).is_err()
        );
    }

    #[test]
    fn restart_recovery_covers_read_write_import_and_export() {
        assert_eq!(
            restart_recovery(OperationKind::ReadQuery, OperationState::Executing),
            RestartRecovery::MarkFailed
        );
        assert_eq!(
            restart_recovery(OperationKind::WriteSql, OperationState::Executing),
            RestartRecovery::OutcomeUnknown
        );
        for kind in [OperationKind::Import, OperationKind::Export] {
            assert_eq!(
                restart_recovery(kind, OperationState::Executing),
                RestartRecovery::ValidateJobCheckpoint
            );
        }
        assert_eq!(
            restart_recovery(OperationKind::Ddl, OperationState::Approved),
            RestartRecovery::Expire
        );
        assert_eq!(
            restart_recovery(OperationKind::WriteSql, OperationState::Succeeded),
            RestartRecovery::KeepTerminal
        );
    }
}
