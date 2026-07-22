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

/// Deterministic, unambiguous serialization of the audited fields. Each field is
/// length-prefixed (`<byte-len>:<field>`), so no arrangement of bytes WITHIN a
/// field can spill across a boundary: shifting a delimiter between two adjacent
/// attacker-controlled fields (sql/prompt/error) changes their byte lengths and
/// thus the serialization, so distinct rows can never hash the same.
fn canonical(f: &AuditFields) -> String {
    let mut out = String::new();
    let mut field = |s: &str| {
        out.push_str(&s.len().to_string());
        out.push(':');
        out.push_str(s);
    };
    field(&f.connection_id.to_string());
    field(&f.ts.to_rfc3339());
    field(crate::store::engine_str(f.engine));
    field(f.agent_prompt.unwrap_or(""));
    field(f.sql);
    field(crate::store::kind_str(f.kind));
    field(f.action);
    field(f.approved_by.unwrap_or(""));
    field(
        &f.affected_estimate
            .map(|n| n.to_string())
            .unwrap_or_default(),
    );
    field(f.error.unwrap_or(""));
    out
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

    #[test]
    fn adjacent_field_boundary_cannot_be_forged() {
        // Two DISTINCT rows that a raw-delimiter join would map to the same bytes:
        // shifting one char across the prompt|sql boundary. Length-prefixing must
        // keep their hashes distinct.
        let mut x = sample();
        x.agent_prompt = Some("ab");
        x.sql = "cd";
        let mut y = sample();
        y.agent_prompt = Some("a");
        y.sql = "bcd";
        assert_ne!(compute_hash(None, &x), compute_hash(None, &y));
    }
}
