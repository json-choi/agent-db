//! Length-prefixed JSON framing and centralized structural limits.

use serde::de::DeserializeOwned;
use serde::Serialize;
use serde_json::Value;
use std::io::{self, Write};
use thiserror::Error;

use crate::{
    RequestEnvelope, ResponseEnvelope, MAX_COLLECTION_ITEMS, MAX_JSON_DEPTH, MAX_JSON_VALUES,
    MAX_REQUEST_BYTES, MAX_RESPONSE_BYTES, MAX_STRING_BYTES,
};

const LENGTH_PREFIX_BYTES: usize = 4;

#[derive(Debug, Error)]
pub enum FrameError {
    #[error("frame is missing its 4-byte length prefix")]
    MissingLengthPrefix,
    #[error("frame payload length must be greater than zero")]
    EmptyPayload,
    #[error("frame payload is {actual} bytes, above the {maximum}-byte limit")]
    PayloadTooLarge { actual: usize, maximum: usize },
    #[error("frame declares {declared} payload bytes but contains {actual}")]
    LengthMismatch { declared: usize, actual: usize },
    #[error("JSON nesting exceeds the depth limit of {maximum}")]
    JsonDepth { maximum: usize },
    #[error("JSON collection has {actual} items, above the {maximum}-item limit")]
    CollectionTooLarge { actual: usize, maximum: usize },
    #[error("JSON value budget exceeds the {maximum}-item limit")]
    ItemBudgetExceeded { maximum: usize },
    #[error("JSON string is {actual} bytes, above the {maximum}-byte limit")]
    StringTooLarge { actual: usize, maximum: usize },
    #[error("control message is not valid JSON at line {line} column {column}")]
    InvalidJson { line: usize, column: usize },
    #[error("control message does not match its command schema")]
    InvalidSchema,
}

/// Validate a network/file length prefix before allocating its payload buffer.
pub fn parse_frame_length(
    prefix: [u8; LENGTH_PREFIX_BYTES],
    maximum: usize,
) -> Result<usize, FrameError> {
    let length = u32::from_be_bytes(prefix) as usize;
    if length == 0 {
        return Err(FrameError::EmptyPayload);
    }
    if length > maximum {
        return Err(FrameError::PayloadTooLarge {
            actual: length,
            maximum,
        });
    }
    Ok(length)
}

/// Serialize one control message with a 4-byte big-endian payload length.
pub fn encode_frame<T: FramePayload + ?Sized>(
    value: &T,
    maximum: usize,
) -> Result<Vec<u8>, FrameError> {
    // Validate every dynamically nested Value before serde's recursive serializer
    // sees it. This prevents a small-but-pathologically-deep in-memory payload from
    // overflowing the process stack before the byte limit can apply.
    value.validate_before_encode()?;
    let maximum = maximum.min(u32::MAX as usize);
    let mut writer = BoundedWriter::new(maximum);
    if let Err(error) = serde_json::to_writer(&mut writer, value) {
        if writer.overflowed {
            return Err(FrameError::PayloadTooLarge {
                actual: maximum.saturating_add(1),
                maximum,
            });
        }
        return Err(safe_json_error(error));
    }
    let payload = writer.into_inner();
    if payload.is_empty() {
        return Err(FrameError::EmptyPayload);
    }
    let json: Value = serde_json::from_slice(&payload).map_err(safe_json_error)?;
    validate_json_limits(&json)?;
    if payload.len() > maximum {
        return Err(FrameError::PayloadTooLarge {
            actual: payload.len(),
            maximum,
        });
    }
    let mut frame = Vec::with_capacity(LENGTH_PREFIX_BYTES + payload.len());
    frame.extend_from_slice(&(payload.len() as u32).to_be_bytes());
    frame.extend_from_slice(&payload);
    Ok(frame)
}

/// Decode a complete control frame. Stream adapters must call `parse_frame_length`
/// before allocating, then pass the exact prefix+payload bytes here.
pub fn decode_frame<T: DeserializeOwned>(frame: &[u8], maximum: usize) -> Result<T, FrameError> {
    if frame.len() < LENGTH_PREFIX_BYTES {
        return Err(FrameError::MissingLengthPrefix);
    }
    let prefix: [u8; LENGTH_PREFIX_BYTES] = frame[..LENGTH_PREFIX_BYTES]
        .try_into()
        .expect("fixed-size prefix");
    let declared = parse_frame_length(prefix, maximum)?;
    let actual = frame.len() - LENGTH_PREFIX_BYTES;
    if declared != actual {
        return Err(FrameError::LengthMismatch { declared, actual });
    }
    let value: Value =
        serde_json::from_slice(&frame[LENGTH_PREFIX_BYTES..]).map_err(safe_json_error)?;
    validate_json_limits(&value)?;
    serde_json::from_value(value).map_err(|_| FrameError::InvalidSchema)
}

fn safe_json_error(error: serde_json::Error) -> FrameError {
    FrameError::InvalidJson {
        line: error.line(),
        column: error.column(),
    }
}

struct BoundedWriter {
    bytes: Vec<u8>,
    maximum: usize,
    overflowed: bool,
}

impl BoundedWriter {
    fn new(maximum: usize) -> Self {
        Self {
            bytes: Vec::with_capacity(maximum.min(8 * 1024)),
            maximum,
            overflowed: false,
        }
    }

    fn into_inner(self) -> Vec<u8> {
        self.bytes
    }
}

impl Write for BoundedWriter {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        let remaining = self.maximum.saturating_sub(self.bytes.len());
        if bytes.len() > remaining {
            self.bytes.extend_from_slice(&bytes[..remaining]);
            self.overflowed = true;
            return Err(io::Error::other("control frame size limit exceeded"));
        }
        self.bytes.extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

fn validate_json_limits(value: &Value) -> Result<(), FrameError> {
    let mut item_budget = 0usize;
    let mut pending = vec![(value, 1usize)];
    while let Some((value, depth)) = pending.pop() {
        if depth > MAX_JSON_DEPTH {
            return Err(FrameError::JsonDepth {
                maximum: MAX_JSON_DEPTH,
            });
        }
        item_budget = item_budget.saturating_add(1);
        if item_budget > MAX_JSON_VALUES {
            return Err(FrameError::ItemBudgetExceeded {
                maximum: MAX_JSON_VALUES,
            });
        }
        match value {
            Value::String(text) => validate_string(text)?,
            Value::Array(values) => {
                validate_collection_len(values.len())?;
                pending.extend(values.iter().rev().map(|value| (value, depth + 1)));
            }
            Value::Object(values) => {
                validate_collection_len(values.len())?;
                for (key, value) in values.iter().rev() {
                    validate_string(key)?;
                    pending.push((value, depth + 1));
                }
            }
            Value::Null | Value::Bool(_) | Value::Number(_) => {}
        }
    }
    Ok(())
}

fn validate_collection_len(actual: usize) -> Result<(), FrameError> {
    if actual > MAX_COLLECTION_ITEMS {
        Err(FrameError::CollectionTooLarge {
            actual,
            maximum: MAX_COLLECTION_ITEMS,
        })
    } else {
        Ok(())
    }
}

fn validate_string(value: &str) -> Result<(), FrameError> {
    let actual = value.len();
    if actual > MAX_STRING_BYTES {
        Err(FrameError::StringTooLarge {
            actual,
            maximum: MAX_STRING_BYTES,
        })
    } else {
        Ok(())
    }
}

mod sealed {
    pub trait Sealed {}
}

/// Payloads whose dynamically nested fields can be checked before serialization.
/// The trait is sealed so callers cannot accidentally bypass the preflight.
pub trait FramePayload: sealed::Sealed + Serialize {
    fn validate_before_encode(&self) -> Result<(), FrameError>;
}

impl sealed::Sealed for Value {}

impl FramePayload for Value {
    fn validate_before_encode(&self) -> Result<(), FrameError> {
        validate_json_limits(self)
    }
}

impl sealed::Sealed for RequestEnvelope {}

impl FramePayload for RequestEnvelope {
    fn validate_before_encode(&self) -> Result<(), FrameError> {
        validate_json_limits(&self.arguments)?;
        if let Some(authentication) = &self.authentication {
            validate_string(authentication.token())?;
        }
        Ok(())
    }
}

impl sealed::Sealed for ResponseEnvelope {}

impl FramePayload for ResponseEnvelope {
    fn validate_before_encode(&self) -> Result<(), FrameError> {
        if let Some(result) = self.result() {
            validate_json_limits(result)?;
        }
        if let Some(error) = self.error() {
            validate_string(error.message())?;
        }
        Ok(())
    }
}

/// Canonical maxima for each envelope direction.
pub const fn request_limit() -> usize {
    MAX_REQUEST_BYTES
}

pub const fn response_limit() -> usize {
    MAX_RESPONSE_BYTES
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::{RequestEnvelope, ResponseEnvelope};

    #[test]
    fn rejects_declared_size_before_payload_allocation() {
        let prefix = ((MAX_REQUEST_BYTES + 1) as u32).to_be_bytes();
        assert!(matches!(
            parse_frame_length(prefix, MAX_REQUEST_BYTES),
            Err(FrameError::PayloadTooLarge { .. })
        ));
    }

    #[test]
    fn rejects_length_mismatch() {
        let mut frame = (10u32).to_be_bytes().to_vec();
        frame.extend_from_slice(b"{}");
        assert!(matches!(
            decode_frame::<serde_json::Value>(&frame, MAX_REQUEST_BYTES),
            Err(FrameError::LengthMismatch { .. })
        ));
    }

    #[test]
    fn rejects_deep_json_before_typed_decode() {
        let mut nested = Value::Null;
        for _ in 0..MAX_JSON_DEPTH {
            nested = json!([nested]);
        }
        assert!(matches!(
            encode_frame(&nested, MAX_REQUEST_BYTES),
            Err(FrameError::JsonDepth { .. })
        ));

        // An untrusted peer can bypass our encoder, so the receiver independently
        // applies the same structural limit before typed decoding.
        let payload = serde_json::to_vec(&nested).unwrap();
        let mut frame = (payload.len() as u32).to_be_bytes().to_vec();
        frame.extend_from_slice(&payload);
        assert!(matches!(
            decode_frame::<Value>(&frame, MAX_REQUEST_BYTES),
            Err(FrameError::JsonDepth { .. })
        ));
    }

    #[test]
    fn rejects_large_strings_and_collection_budgets() {
        let text = "x".repeat(MAX_STRING_BYTES + 1);
        assert!(matches!(
            encode_frame(&json!({"value": text}), MAX_REQUEST_BYTES),
            Err(FrameError::StringTooLarge { .. })
        ));

        let values = Value::Array(vec![Value::Null; MAX_COLLECTION_ITEMS]);
        assert!(matches!(
            encode_frame(&values, MAX_REQUEST_BYTES),
            Err(FrameError::ItemBudgetExceeded { .. })
        ));
    }

    #[test]
    fn outbound_payload_is_bounded_during_serialization() {
        let oversized = Value::String("x".repeat(1024));
        assert!(matches!(
            encode_frame(&oversized, 32),
            Err(FrameError::PayloadTooLarge {
                actual: 33,
                maximum: 32
            })
        ));
    }

    #[test]
    fn pathological_outbound_depth_is_rejected_before_serialization() {
        let mut nested = Value::Null;
        for _ in 0..10_000 {
            nested = Value::Array(vec![nested]);
        }
        let result = encode_frame(&nested, MAX_REQUEST_BYTES);
        assert!(matches!(result, Err(FrameError::JsonDepth { .. })));
        // serde_json::Value itself has a recursive destructor. Keep this adversarial
        // fixture from testing that unrelated implementation detail on this thread.
        std::mem::forget(nested);
    }

    #[test]
    fn attacker_controlled_schema_names_are_not_printed_in_errors() {
        let marker = "SECRET_MARKER_FIELD";
        let value = json!({
            "protocolVersion": 1,
            "commandSchemaVersion": 1,
            "requestId": "018f1111-2222-7333-8444-555566667777",
            "command": "status",
            marker: true
        });
        let frame = encode_frame(&value, MAX_REQUEST_BYTES).unwrap();
        let error = decode_frame::<crate::RequestEnvelope>(&frame, MAX_REQUEST_BYTES).unwrap_err();
        assert!(!format!("{error}").contains(marker));
        assert!(!format!("{error:?}").contains(marker));
    }

    #[test]
    fn request_and_response_golden_envelopes_use_the_only_decoder() {
        let request_frame = encode_frame(
            &serde_json::from_str::<Value>(include_str!(
                "../tests/fixtures/query-plan-request.json"
            ))
            .unwrap(),
            request_limit(),
        )
        .unwrap();
        let request: RequestEnvelope = decode_frame(&request_frame, request_limit()).unwrap();
        assert_eq!(request.command.as_str(), "query.plan");

        let response_frame = encode_frame(
            &serde_json::from_str::<Value>(include_str!("../tests/fixtures/status-success.json"))
                .unwrap(),
            response_limit(),
        )
        .unwrap();
        let response: ResponseEnvelope = decode_frame(&response_frame, response_limit()).unwrap();
        assert!(response.is_ok());
    }
}
