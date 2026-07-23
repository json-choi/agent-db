//! Stable broker error codes and redacted error envelopes.

use serde::de::Error as _;
use serde::{Deserialize, Deserializer, Serialize};
use std::fmt;

/// Stable machine-readable error categories. Human messages may improve over time,
/// while these values remain compatible within protocol version 1.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCode {
    InvalidRequest,
    RuntimeUnavailable,
    AuthenticationDenied,
    ScopeDenied,
    PolicyBlocked,
    OperationExpired,
    OperationConflict,
    Cancelled,
    Timeout,
    TargetExecutionFailed,
    ProtocolMismatch,
    ResponseTooLarge,
    Internal,
}

/// A broker error safe to serialize to CLI callers. Callers must not put raw
/// credentials, connection URLs, certificates, or provider tokens in `message`.
#[derive(Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ProtocolError {
    code: ErrorCode,
    message: String,
    retryable: bool,
}

impl ProtocolError {
    /// Build an externally safe error using only the stable message assigned to its
    /// code. Raw driver/provider errors must remain in redacted internal telemetry.
    pub fn new(code: ErrorCode, retryable: bool) -> Self {
        Self {
            code,
            message: code.safe_message().into(),
            retryable,
        }
    }

    pub const fn code(&self) -> ErrorCode {
        self.code
    }

    pub fn message(&self) -> &str {
        &self.message
    }

    pub const fn is_retryable(&self) -> bool {
        self.retryable
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct WireProtocolError {
    code: ErrorCode,
    message: String,
    retryable: bool,
}

impl<'de> Deserialize<'de> for ProtocolError {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let wire = WireProtocolError::deserialize(deserializer)?;
        if wire.message != wire.code.safe_message() {
            return Err(D::Error::custom(
                "protocol error message does not match its stable error code",
            ));
        }
        Ok(Self::new(wire.code, wire.retryable))
    }
}

impl ErrorCode {
    pub const fn safe_message(self) -> &'static str {
        match self {
            Self::InvalidRequest => "the request is invalid",
            Self::RuntimeUnavailable => "the DopeDB runtime is unavailable",
            Self::AuthenticationDenied => "runtime authentication was denied",
            Self::ScopeDenied => "the requested resource is outside the active scope",
            Self::PolicyBlocked => "the operation is blocked by the active safety policy",
            Self::OperationExpired => "the operation has expired",
            Self::OperationConflict => "the operation state changed before it could be claimed",
            Self::Cancelled => "the operation was cancelled",
            Self::Timeout => "the operation timed out",
            Self::TargetExecutionFailed => "the target database operation failed",
            Self::ProtocolMismatch => "the client and runtime protocols are incompatible",
            Self::ResponseTooLarge => "the response exceeds the configured limit",
            Self::Internal => "the DopeDB runtime encountered an internal error",
        }
    }
}

impl fmt::Debug for ProtocolError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ProtocolError")
            .field("code", &self.code)
            .field("message", &"<redacted>")
            .field("retryable", &self.retryable)
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn constructor_never_accepts_or_serializes_an_internal_error_string() {
        let raw = "postgresql://reader:fixture-secret@private.example/app";
        let error = ProtocolError::new(ErrorCode::TargetExecutionFailed, false);
        let serialized = serde_json::to_string(&error).unwrap();
        assert!(!serialized.contains(raw));
        assert!(!serialized.contains("fixture-secret"));
        assert_eq!(
            error.message,
            "the target database operation failed".to_string()
        );
    }

    #[test]
    fn deserialization_cannot_bypass_the_safe_message_mapping() {
        let error = serde_json::json!({
            "code": "target_execution_failed",
            "message": "postgresql://reader:fixture-secret@private.example/app",
            "retryable": false
        });
        assert!(serde_json::from_value::<ProtocolError>(error).is_err());
    }
}
