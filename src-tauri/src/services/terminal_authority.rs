//! Immutable authority captured by one in-app Terminal session.

use uuid::Uuid;

use crate::error::{AppError, AppResult};
use crate::store::PinnedConnection;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TerminalAuthority {
    pub(crate) terminal_session_id: Uuid,
    pub(crate) workspace_id: Uuid,
    pub(crate) account_scope: String,
    pub(crate) connection_id: Uuid,
    pub(crate) connection_revision: i64,
    pub(crate) client_protocol_version: u16,
}

impl TerminalAuthority {
    pub(crate) fn ensure_pin(&self, pin: &PinnedConnection) -> AppResult<()> {
        let matches = pin.scope.workspace_id == self.workspace_id
            && pin.scope.account_scope.storage_key() == self.account_scope
            && pin.connection_id == self.connection_id
            && pin.connection_revision == self.connection_revision;
        if matches {
            Ok(())
        } else {
            Err(AppError::Blocked {
                reason: "Terminal connection authority is no longer current".into(),
            })
        }
    }
}
