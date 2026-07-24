//! Versioned canonical JSON encoding for immutable Operation payloads and ledger
//! records. Object keys are recursively sorted before compact serialization so a
//! semantically identical request always receives the same SHA-256.

use serde_json::{Map, Value};
use sha2::{Digest, Sha256};

use crate::error::{AppError, AppResult};

/// Canonical JSON plus the lowercase SHA-256 of its exact stored bytes.
pub(crate) struct CanonicalJson {
    json: String,
    sha256: String,
}

impl CanonicalJson {
    pub(crate) fn from_value(value: &Value) -> AppResult<Self> {
        let canonical = canonical_value(value);
        let json = serde_json::to_string(&canonical)?;
        let sha256 = lower_hex(&Sha256::digest(json.as_bytes()));
        Ok(Self { json, sha256 })
    }

    /// Validate that persisted JSON is canonical and still matches its immutable
    /// digest. This detects direct file edits before an Operation is executed.
    pub(crate) fn from_stored(json: &str, expected_sha256: &str) -> AppResult<Self> {
        let value: Value = serde_json::from_str(json)?;
        let canonical = Self::from_value(&value)?;
        if canonical.json != json || canonical.sha256 != expected_sha256 {
            return Err(AppError::Config(
                "stored operation payload failed canonical integrity validation".into(),
            ));
        }
        Ok(canonical)
    }

    pub(crate) fn json(&self) -> &str {
        &self.json
    }

    pub(crate) fn sha256(&self) -> &str {
        &self.sha256
    }

    pub(crate) fn into_value(self) -> AppResult<Value> {
        Ok(serde_json::from_str(&self.json)?)
    }
}

/// Canonicalize JSON that is stored but not independently hashed, such as previews
/// and policy snapshots.
pub(crate) fn canonical_json(value: &Value) -> AppResult<String> {
    CanonicalJson::from_value(value).map(|canonical| canonical.json)
}

fn canonical_value(value: &Value) -> Value {
    match value {
        Value::Array(values) => Value::Array(values.iter().map(canonical_value).collect()),
        Value::Object(values) => {
            let mut keys = values.keys().collect::<Vec<_>>();
            keys.sort_unstable();
            let mut canonical = Map::with_capacity(values.len());
            for key in keys {
                canonical.insert(key.clone(), canonical_value(&values[key]));
            }
            Value::Object(canonical)
        }
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => value.clone(),
    }
}

fn lower_hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        output.push(HEX[(byte >> 4) as usize] as char);
        output.push(HEX[(byte & 0x0f) as usize] as char);
    }
    output
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn recursively_sorted_objects_have_one_stable_hash() {
        let left = json!({
            "sql": "SELECT 1",
            "options": {"timeout": 30, "tags": ["a", "b"]},
        });
        let right = json!({
            "options": {"tags": ["a", "b"], "timeout": 30},
            "sql": "SELECT 1",
        });
        let left = CanonicalJson::from_value(&left).unwrap();
        let right = CanonicalJson::from_value(&right).unwrap();
        assert_eq!(left.json(), right.json());
        assert_eq!(left.sha256(), right.sha256());
        assert_eq!(left.sha256().len(), 64);
        assert!(left
            .sha256()
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)));
    }

    #[test]
    fn array_order_and_payload_changes_remain_significant() {
        let first = CanonicalJson::from_value(&json!({"ids": [1, 2]})).unwrap();
        let reordered = CanonicalJson::from_value(&json!({"ids": [2, 1]})).unwrap();
        let changed = CanonicalJson::from_value(&json!({"ids": [1, 3]})).unwrap();
        assert_ne!(first.sha256(), reordered.sha256());
        assert_ne!(first.sha256(), changed.sha256());
    }

    #[test]
    fn stored_payload_requires_both_canonical_bytes_and_matching_digest() {
        let canonical = CanonicalJson::from_value(&json!({"a": 1, "b": 2})).unwrap();
        assert!(
            CanonicalJson::from_stored(canonical.json(), canonical.sha256())
                .unwrap()
                .into_value()
                .is_ok()
        );
        assert!(CanonicalJson::from_stored(r#"{"b":2,"a":1}"#, canonical.sha256()).is_err());
        assert!(CanonicalJson::from_stored(canonical.json(), &"0".repeat(64)).is_err());
    }
}
