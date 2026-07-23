//! Validated broker response envelope.

use std::fmt;

use serde::de::Error as _;
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::Value;
use uuid::Uuid;

use crate::ProtocolError;

#[derive(Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ResponseEnvelope {
    protocol_version: u16,
    request_id: Uuid,
    ok: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    result: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    error: Option<ProtocolError>,
}

impl ResponseEnvelope {
    pub fn success(protocol_version: u16, request_id: Uuid, result: Value) -> Self {
        Self {
            protocol_version,
            request_id,
            ok: true,
            result: Some(result),
            error: None,
        }
    }

    pub fn failure(protocol_version: u16, request_id: Uuid, error: ProtocolError) -> Self {
        Self {
            protocol_version,
            request_id,
            ok: false,
            result: None,
            error: Some(error),
        }
    }

    /// Reject malformed envelopes before they cross an adapter boundary.
    pub fn validate(&self) -> Result<(), &'static str> {
        match (self.ok, self.result.is_some(), self.error.is_some()) {
            (true, true, false) | (false, false, true) => Ok(()),
            _ => Err("response must contain exactly one of result or error matching ok"),
        }
    }

    pub const fn protocol_version(&self) -> u16 {
        self.protocol_version
    }

    pub const fn request_id(&self) -> Uuid {
        self.request_id
    }

    pub const fn is_ok(&self) -> bool {
        self.ok
    }

    pub fn result(&self) -> Option<&Value> {
        self.result.as_ref()
    }

    pub fn error(&self) -> Option<&ProtocolError> {
        self.error.as_ref()
    }
}

impl fmt::Debug for ResponseEnvelope {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ResponseEnvelope")
            .field("protocol_version", &self.protocol_version)
            .field("request_id", &self.request_id)
            .field("ok", &self.ok)
            .field("payload", &"<redacted>")
            .finish()
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct WireResponseEnvelope {
    protocol_version: u16,
    request_id: Uuid,
    ok: bool,
    #[serde(default)]
    result: Option<Value>,
    #[serde(default)]
    error: Option<ProtocolError>,
}

impl<'de> Deserialize<'de> for ResponseEnvelope {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        // `Option<Value>` alone cannot distinguish an absent `result` from an
        // explicitly present JSON `null`. Preserve field presence before typed
        // decoding so successful null results round-trip correctly.
        let value = Value::deserialize(deserializer)?;
        let object = value
            .as_object()
            .ok_or_else(|| D::Error::custom("response envelope must be an object"))?;
        let result_present = object.contains_key("result");
        let error_present = object.contains_key("error");
        let wire = WireResponseEnvelope::deserialize(value).map_err(D::Error::custom)?;
        let response = Self {
            protocol_version: wire.protocol_version,
            request_id: wire.request_id,
            ok: wire.ok,
            result: if result_present {
                Some(wire.result.unwrap_or(Value::Null))
            } else {
                None
            },
            error: wire.error,
        };
        if error_present && response.error.is_none() {
            return Err(D::Error::custom(
                "response error must be a non-null protocol error",
            ));
        }
        response.validate().map_err(D::Error::custom)?;
        Ok(response)
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::ErrorCode;

    const REQUEST_ID: Uuid = Uuid::from_u128(0x018f_1111_2222_7333_8444_5555_6666_7777);

    #[test]
    fn constructors_preserve_response_invariant() {
        let success = ResponseEnvelope::success(1, REQUEST_ID, json!({"ready": true}));
        let failure = ResponseEnvelope::failure(
            1,
            REQUEST_ID,
            ProtocolError::new(ErrorCode::PolicyBlocked, false),
        );
        assert_eq!(success.validate(), Ok(()));
        assert_eq!(failure.validate(), Ok(()));
    }

    #[test]
    fn malformed_wire_response_is_rejected_during_deserialization() {
        let malformed = serde_json::json!({
            "protocolVersion": 1,
            "requestId": REQUEST_ID,
            "ok": true,
            "result": {},
            "error": {
                "code": "internal",
                "message": "must not coexist",
                "retryable": false
            }
        });
        assert!(serde_json::from_value::<ResponseEnvelope>(malformed).is_err());
    }

    #[test]
    fn explicit_null_success_result_round_trips() {
        let response = ResponseEnvelope::success(1, REQUEST_ID, Value::Null);
        let encoded = serde_json::to_value(&response).unwrap();
        assert_eq!(encoded["result"], Value::Null);
        let decoded: ResponseEnvelope = serde_json::from_value(encoded).unwrap();
        assert_eq!(decoded.result(), Some(&Value::Null));
    }

    #[test]
    fn debug_never_prints_result_or_error_message() {
        let response =
            ResponseEnvelope::success(1, REQUEST_ID, json!({"credential": "fixture-secret"}));
        let debug = format!("{response:?}");
        assert!(debug.contains("<redacted>"));
        assert!(!debug.contains("fixture-secret"));
    }
}
