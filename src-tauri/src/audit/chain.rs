//! Hash-chain primitives for the audit log.
//!
//! Each entry's `hash = SHA256(prev_hash ‖ canonical_row)` where `canonical_row`
//! is a deterministic serialization of the audited fields. Linking every row to
//! its predecessor's hash makes any post-hoc edit detectable: re-hashing the
//! chain will diverge from the stored hashes at the first altered row.
//!
//! This is tamper-EVIDENT, not tamper-proof — an attacker with write access to
//! app.db can recompute the whole chain. It defends against silent edits, not a
//! determined rewrite (that needs an external notary / append-only sink).

use chrono::{DateTime, Utc};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::model::{Engine, QueryKind};

/// The audited fields, borrowed, that feed into one chain link. Kept separate
/// from `model::AuditEntry` so hashing never depends on `id`, `prev_hash`, or
/// `hash` (which would make the chain self-referential).
pub struct AuditFields<'a> {
    pub connection_id: Uuid,
    pub ts: DateTime<Utc>,
    pub engine: Engine,
    pub agent_prompt: Option<&'a str>,
    pub sql: &'a str,
    pub kind: QueryKind,
    pub action: &'a str,
    pub approved_by: Option<&'a str>,
    pub affected_estimate: Option<i64>,
    pub error: Option<&'a str>,
}

/// Deterministic, unambiguous serialization of the audited fields. Uses a unit
/// separator (0x1F) between fields so field boundaries can't be forged by
/// crafting values that contain the delimiter.
fn canonical(f: &AuditFields) -> String {
    const US: char = '\u{1f}';
    format!(
        "{cid}{US}{ts}{US}{eng}{US}{prompt}{US}{sql}{US}{kind}{US}{action}{US}{by}{US}{est}{US}{err}",
        cid = f.connection_id,
        ts = f.ts.to_rfc3339(),
        eng = crate::store::engine_str(f.engine),
        prompt = f.agent_prompt.unwrap_or(""),
        sql = f.sql,
        kind = crate::store::kind_str(f.kind),
        action = f.action,
        by = f.approved_by.unwrap_or(""),
        est = f.affected_estimate.map(|n| n.to_string()).unwrap_or_default(),
        err = f.error.unwrap_or(""),
    )
}

/// `hex(SHA256(prev_hash ‖ canonical(fields)))`. The genesis link uses an empty
/// `prev_hash`.
pub fn compute_hash(prev_hash: Option<&str>, fields: &AuditFields) -> String {
    let mut h = Sha256::new();
    h.update(prev_hash.unwrap_or("").as_bytes());
    h.update([0x1e]); // record separator between prev_hash and the row
    h.update(canonical(fields).as_bytes());
    hex::encode(h.finalize())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample<'a>() -> AuditFields<'a> {
        AuditFields {
            connection_id: Uuid::nil(),
            ts: DateTime::parse_from_rfc3339("2026-07-01T00:00:00Z")
                .unwrap()
                .with_timezone(&Utc),
            engine: Engine::Postgres,
            agent_prompt: Some("count users"),
            sql: "SELECT count(*) FROM users",
            kind: QueryKind::Read,
            action: "execute",
            approved_by: Some("ira1@launcher.capital"),
            affected_estimate: Some(0),
            error: None,
        }
    }

    #[test]
    fn hash_is_deterministic_and_chains() {
        let f = sample();
        let a = compute_hash(None, &f);
        let b = compute_hash(None, &f);
        assert_eq!(a, b, "same inputs → same hash");
        assert_eq!(a.len(), 64, "sha256 hex is 64 chars");

        // A different prev_hash must change the link (that's the whole point).
        let linked = compute_hash(Some(&a), &f);
        assert_ne!(a, linked);

        // Editing any audited field must change the hash.
        let mut edited = sample();
        edited.action = "reject";
        assert_ne!(a, compute_hash(None, &edited));
    }
}
