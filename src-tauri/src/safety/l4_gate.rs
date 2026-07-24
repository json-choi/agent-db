//! L4 — human approval gate.
//!
//! Produces a [`GateDecision`] the command layer enforces:
//! - read-only SELECT + `auto_run_reads` → [`GateDecision::AutoRun`];
//! - write / DDL:
//!     * `allow_writes` false → [`GateDecision::Block`];
//!     * `allow_writes` true → [`GateDecision::RequireApproval`];
//! - arbitrary privilege SQL → [`GateDecision::Block`]; narrowly scoped official
//!   operations such as `pg_monitor` use their own exact Operation service;
//! - `> 1` statement → always [`GateDecision::Block`].
//!
//! The persisted `require_approval` field is retained only for storage compatibility.
//! It can never bypass the exact Operation approval required for target mutations.
//!
//! This layer only *decides*; it never touches the DB. The approval-card payload
//! (plain-English restatement, risk badge) is assembled frontend-side.

use serde::{Deserialize, Serialize};

use crate::model::{Classification, QueryKind, SafetySettings};

/// What the gate decided. Adjacently tagged so JS gets `{ decision, reason? }`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "decision", rename_all = "camelCase")]
pub enum GateDecision {
    /// Safe to run without prompting (read-only + auto-run enabled).
    AutoRun,
    /// Must show the approval card and get an explicit confirm.
    RequireApproval,
    /// Refused before it reaches a human; `reason` is shown verbatim.
    Block { reason: String },
}

/// Decide how a classified statement may proceed. Pure policy — the authoritative
/// stop is still L2, but this is where connection policy (`allow_writes`,
/// `auto_run_reads`) is applied.
pub fn decide(settings: &SafetySettings, c: &Classification) -> GateDecision {
    if c.statement_count > 1 {
        return GateDecision::Block {
            reason: "multiple statements are not allowed — submit one statement at a time".into(),
        };
    }

    match c.kind {
        QueryKind::Read => {
            if settings.auto_run_reads {
                GateDecision::AutoRun
            } else {
                GateDecision::RequireApproval
            }
        }
        QueryKind::Privilege => GateDecision::Block {
            reason: "arbitrary privilege SQL is blocked; use a supported, narrowly scoped administrative action"
                .into(),
        },
        QueryKind::Write | QueryKind::Ddl => {
            if !settings.allow_writes {
                GateDecision::Block {
                    reason: format!(
                        "{} is disabled for this connection (writes are off by default). \
                         Enable writes in the connection's safety settings to propose it.",
                        kind_label(c.kind)
                    ),
                }
            } else {
                // Target mutations always become exact Operation proposals. The
                // legacy persisted setting must never turn a write into auto-run.
                GateDecision::RequireApproval
            }
        }
    }
}

fn kind_label(kind: QueryKind) -> &'static str {
    match kind {
        QueryKind::Read => "reading",
        QueryKind::Write => "writing",
        QueryKind::Ddl => "schema change (DDL)",
        QueryKind::Privilege => "privilege change",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::RiskLevel;

    fn cls(kind: QueryKind, statement_count: u32) -> Classification {
        Classification {
            kind,
            risk: RiskLevel::Low,
            statement_count,
            no_where: false,
            tables: vec!["orders".into()],
            notes: vec![],
            rollback_safe: matches!(kind, QueryKind::Write),
        }
    }

    #[test]
    fn read_auto_runs_when_enabled() {
        let s = SafetySettings::default(); // auto_run_reads = true
        assert!(matches!(
            decide(&s, &cls(QueryKind::Read, 1)),
            GateDecision::AutoRun
        ));
    }

    #[test]
    fn write_blocked_when_writes_off() {
        let s = SafetySettings::default(); // allow_writes = false
        assert!(matches!(
            decide(&s, &cls(QueryKind::Write, 1)),
            GateDecision::Block { .. }
        ));
    }

    #[test]
    fn write_requires_approval_when_writes_on() {
        let s = SafetySettings {
            allow_writes: true,
            ..SafetySettings::default()
        };
        assert!(matches!(
            decide(&s, &cls(QueryKind::Write, 1)),
            GateDecision::RequireApproval
        ));
    }

    #[test]
    fn target_mutations_require_approval_when_legacy_setting_is_off() {
        for kind in [QueryKind::Write, QueryKind::Ddl] {
            let s = SafetySettings {
                allow_writes: true,
                require_approval: false,
                ..SafetySettings::default()
            };
            assert!(
                matches!(decide(&s, &cls(kind, 1)), GateDecision::RequireApproval),
                "{kind:?} must never auto-run"
            );
        }
    }

    #[test]
    fn arbitrary_privilege_sql_is_blocked_even_when_writes_are_enabled() {
        let s = SafetySettings {
            allow_writes: true,
            require_approval: false,
            ..SafetySettings::default()
        };
        assert!(matches!(
            decide(&s, &cls(QueryKind::Privilege, 1)),
            GateDecision::Block { .. }
        ));
    }

    #[test]
    fn write_blocked_even_when_approval_off_if_writes_off() {
        // allow_writes gates first: no writes means block regardless of require_approval.
        let s = SafetySettings {
            allow_writes: false,
            require_approval: false,
            ..SafetySettings::default()
        };
        assert!(matches!(
            decide(&s, &cls(QueryKind::Write, 1)),
            GateDecision::Block { .. }
        ));
    }

    #[test]
    fn multi_statement_always_blocked() {
        let s = SafetySettings {
            allow_writes: true,
            ..SafetySettings::default()
        };
        assert!(matches!(
            decide(&s, &cls(QueryKind::Read, 2)),
            GateDecision::Block { .. }
        ));
    }
}
