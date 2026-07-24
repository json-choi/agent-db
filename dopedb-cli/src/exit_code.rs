use dopedb_protocol::ErrorCode;

use crate::client::ClientError;

pub(crate) const SUCCESS: u8 = 0;
pub(crate) const USAGE: u8 = 2;
pub(crate) const RUNTIME_UNAVAILABLE: u8 = 3;
pub(crate) const AUTHENTICATION_DENIED: u8 = 4;
pub(crate) const POLICY_BLOCKED: u8 = 5;
pub(crate) const OPERATION_CONFLICT: u8 = 6;
pub(crate) const CANCELLED: u8 = 7;
pub(crate) const TARGET_EXECUTION_FAILED: u8 = 8;
pub(crate) const PROTOCOL_MISMATCH: u8 = 9;
pub(crate) const INTERNAL: u8 = 10;

pub(crate) fn for_client_error(error: &ClientError) -> u8 {
    match error {
        ClientError::InvalidArguments
        | ClientError::ConnectionNotFound
        | ClientError::AmbiguousConnection(_) => USAGE,
        ClientError::RuntimeUnavailable => RUNTIME_UNAVAILABLE,
        ClientError::AuthenticationUnavailable => AUTHENTICATION_DENIED,
        ClientError::ProtocolMismatch => PROTOCOL_MISMATCH,
        ClientError::InvalidResponse | ClientError::Internal => INTERNAL,
        ClientError::Remote(remote) => match remote.code() {
            ErrorCode::InvalidRequest => USAGE,
            ErrorCode::RuntimeUnavailable => RUNTIME_UNAVAILABLE,
            ErrorCode::AuthenticationDenied | ErrorCode::ScopeDenied => AUTHENTICATION_DENIED,
            ErrorCode::PolicyBlocked => POLICY_BLOCKED,
            ErrorCode::OperationExpired | ErrorCode::OperationConflict => OPERATION_CONFLICT,
            ErrorCode::Cancelled | ErrorCode::Timeout => CANCELLED,
            ErrorCode::TargetExecutionFailed => TARGET_EXECUTION_FAILED,
            ErrorCode::ProtocolMismatch => PROTOCOL_MISMATCH,
            ErrorCode::ResponseTooLarge | ErrorCode::Internal => INTERNAL,
        },
    }
}

#[cfg(test)]
mod tests {
    use dopedb_protocol::ProtocolError;

    use super::*;

    #[test]
    fn stable_remote_categories_map_to_documented_exit_codes() {
        let cases = [
            (ErrorCode::AuthenticationDenied, 4),
            (ErrorCode::PolicyBlocked, 5),
            (ErrorCode::OperationExpired, 6),
            (ErrorCode::Cancelled, 7),
            (ErrorCode::TargetExecutionFailed, 8),
            (ErrorCode::ProtocolMismatch, 9),
            (ErrorCode::Internal, 10),
        ];
        for (code, expected) in cases {
            assert_eq!(
                for_client_error(&ClientError::Remote(ProtocolError::new(code, false))),
                expected
            );
        }
    }
}
