//! Opaque execution capability issued only after the durable Operation projection
//! has been atomically claimed. Adapters cannot deserialize or construct this type.

use uuid::Uuid;

use super::model::OperationRecord;
use super::OperationState;
use crate::error::{AppError, AppResult};

/// Compile-time capability required by target-mutating executor paths.
///
/// All fields are private and the constructor is visible only inside the Operation
/// Runtime module tree. The type intentionally implements neither `Serialize`,
/// `Deserialize`, `Default`, nor `Clone`.
pub(crate) struct ExecutionGrant {
    operation_id: Uuid,
    payload_sha256: String,
    connection_id: Uuid,
}

impl ExecutionGrant {
    pub(crate) const fn operation_id(&self) -> Uuid {
        self.operation_id
    }

    pub(crate) fn payload_sha256(&self) -> &str {
        &self.payload_sha256
    }

    pub(crate) const fn connection_id(&self) -> Uuid {
        self.connection_id
    }
}

pub(super) fn issue(record: &OperationRecord) -> AppResult<ExecutionGrant> {
    if record.state != OperationState::Executing {
        return Err(AppError::Config(
            "an execution grant requires an atomically claimed operation".into(),
        ));
    }
    Ok(ExecutionGrant {
        operation_id: record.id,
        payload_sha256: record.payload_hash.clone(),
        connection_id: record.connection_id,
    })
}
