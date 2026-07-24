//! SQLite persistence for immutable Operations and their append-only lifecycle
//! ledger. Every state mutation is compare-and-swap and validated by the single
//! authoritative Rust state machine before the projection changes.

use std::sync::Arc;

use chrono::{DateTime, SecondsFormat, Utc};
use dopedb_protocol::{OperationEventKind, OperationState, MAX_REQUEST_BYTES, MAX_RESPONSE_BYTES};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use sqlx::{Row, Sqlite, SqlitePool, Transaction};
use tokio::sync::Mutex;
use uuid::Uuid;

use super::canonicalize::{canonical_json, CanonicalJson};
use super::model::{
    actor_kind_str, event_kind_str, operation_kind_str, parse_actor_kind, parse_event_kind,
    parse_operation_kind, parse_risk_level, parse_state, risk_level_str, state_str, NewOperation,
    OperationActor, OperationActorProvenance, OperationEventRecord, OperationRecord,
    RestartRecoveryReport,
};
use super::{ensure_transition, restart_recovery, RestartRecovery};
use crate::error::{AppError, AppResult};
use crate::store::Store;

#[derive(Clone)]
pub(crate) struct OperationRepository {
    pool: SqlitePool,
    write_lock: Arc<Mutex<()>>,
}

impl OperationRepository {
    pub(crate) fn new(store: &Store) -> Self {
        Self {
            pool: store.pool().clone(),
            write_lock: Arc::new(Mutex::new(())),
        }
    }

    #[cfg(test)]
    fn from_pool(pool: SqlitePool) -> Self {
        Self {
            pool,
            write_lock: Arc::new(Mutex::new(())),
        }
    }

    /// Persist the first durable state. The repository computes canonical JSON and
    /// its hash itself, so an adapter cannot pair one payload with another digest.
    pub(crate) async fn insert_planned(
        &self,
        operation: NewOperation,
    ) -> AppResult<OperationRecord> {
        let prepared = PreparedOperation::new(operation)?;
        let _guard = self.write_lock.lock().await;
        let mut tx = self.pool.begin().await?;

        if let Some(row) = sqlx::query(
            "SELECT * FROM operations
             WHERE workspace_id = ?1
               AND actor_kind = ?2
               AND actor_id = ?3
               AND idempotency_key = ?4",
        )
        .bind(prepared.operation.workspace_id.to_string())
        .bind(actor_kind_str(prepared.operation.actor.kind))
        .bind(&prepared.operation.actor.id)
        .bind(&prepared.operation.idempotency_key)
        .fetch_optional(&mut *tx)
        .await?
        {
            let existing = row_to_operation(&row)?;
            if prepared.matches(&existing) {
                tx.commit().await?;
                return Ok(existing);
            }
            return Err(operation_conflict(
                "the idempotency key is already bound to a different immutable operation",
            ));
        }

        let created_at = Utc::now();
        let created_at_text = timestamp(created_at);
        sqlx::query(
            "INSERT INTO operations (
                id, runtime_id, workspace_id, account_scope, connection_id,
                connection_revision, terminal_session_id, actor_kind, actor_id,
                actor_provenance_json, operation_kind, payload_schema_version,
                payload_json, payload_hash, schema_fingerprint, risk_level,
                preview_json, policy_snapshot_json, policy_revision, state,
                single_use, idempotency_key, expires_at, started_at, finished_at,
                created_at, updated_at
             ) VALUES (
                ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13,
                ?14, ?15, ?16, ?17, ?18, ?19, 'planned', ?20, ?21, ?22,
                NULL, NULL, ?23, ?23
             )",
        )
        .bind(prepared.operation.id.to_string())
        .bind(prepared.operation.runtime_id.to_string())
        .bind(prepared.operation.workspace_id.to_string())
        .bind(&prepared.operation.account_scope)
        .bind(prepared.operation.connection_id.to_string())
        .bind(prepared.operation.connection_revision)
        .bind(
            prepared
                .operation
                .terminal_session_id
                .map(|id| id.to_string()),
        )
        .bind(actor_kind_str(prepared.operation.actor.kind))
        .bind(&prepared.operation.actor.id)
        .bind(&prepared.actor_provenance_json)
        .bind(operation_kind_str(prepared.operation.kind))
        .bind(i64::from(prepared.operation.payload_schema_version))
        .bind(prepared.payload.json())
        .bind(prepared.payload.sha256())
        .bind(&prepared.operation.schema_fingerprint)
        .bind(risk_level_str(prepared.operation.risk_level))
        .bind(&prepared.preview_json)
        .bind(&prepared.policy_snapshot_json)
        .bind(&prepared.operation.policy_revision)
        .bind(prepared.operation.single_use)
        .bind(&prepared.operation.idempotency_key)
        .bind(prepared.operation.expires_at.map(timestamp))
        .bind(&created_at_text)
        .execute(&mut *tx)
        .await?;

        self.append_event_tx(
            &mut tx,
            prepared.operation.id,
            OperationEventKind::Planned,
            OperationState::Planned,
            &json!({
                "payloadHash": prepared.payload.sha256(),
                "riskLevel": risk_level_str(prepared.operation.risk_level),
            }),
            created_at,
        )
        .await?;
        let record = fetch_operation_tx(&mut tx, prepared.operation.id).await?;
        tx.commit().await?;
        Ok(record)
    }

    pub(crate) async fn find(&self, operation_id: Uuid) -> AppResult<Option<OperationRecord>> {
        let row = sqlx::query("SELECT * FROM operations WHERE id = ?1")
            .bind(operation_id.to_string())
            .fetch_optional(&self.pool)
            .await?;
        row.as_ref().map(row_to_operation).transpose()
    }

    pub(crate) async fn get(&self, operation_id: Uuid) -> AppResult<OperationRecord> {
        self.find(operation_id)
            .await?
            .ok_or_else(|| AppError::NotFound(format!("operation {operation_id}")))
    }

    /// Move a non-execution lifecycle state. Entering `executing` is deliberately
    /// excluded; only `claim_execution` may perform that compare-and-swap.
    pub(crate) async fn transition(
        &self,
        operation_id: Uuid,
        runtime_id: Uuid,
        target: OperationState,
        details: &Value,
    ) -> AppResult<OperationRecord> {
        if target == OperationState::Executing {
            return Err(AppError::Config(
                "execution must be entered through the atomic claim API".into(),
            ));
        }
        let _guard = self.write_lock.lock().await;
        let mut tx = self.pool.begin().await?;
        let current = fetch_operation_tx(&mut tx, operation_id).await?;
        ensure_runtime(&current, runtime_id)?;
        let updated = self
            .transition_tx(&mut tx, &current, target, details, Utc::now())
            .await?;
        tx.commit().await?;
        Ok(updated)
    }

    /// Atomically claim the exact payload previously shown to the caller. Only one
    /// contender can change `ready|approved` into `executing`.
    pub(crate) async fn claim_execution(
        &self,
        operation_id: Uuid,
        runtime_id: Uuid,
        expected_payload_hash: &str,
        now: DateTime<Utc>,
    ) -> AppResult<OperationRecord> {
        let _guard = self.write_lock.lock().await;
        let mut tx = self.pool.begin().await?;
        let current = fetch_operation_tx(&mut tx, operation_id).await?;
        ensure_runtime(&current, runtime_id)?;
        if current.payload_hash != expected_payload_hash {
            return Err(operation_conflict(
                "the operation payload hash no longer matches the reviewed payload",
            ));
        }
        if !matches!(
            current.state,
            OperationState::Ready | OperationState::Approved
        ) {
            return Err(operation_conflict(
                "the operation is not in an executable state",
            ));
        }
        if current
            .expires_at
            .is_some_and(|expires_at| expires_at <= now)
        {
            self.transition_tx(
                &mut tx,
                &current,
                OperationState::Expired,
                &json!({"reason": "operation_expired"}),
                now,
            )
            .await?;
            tx.commit().await?;
            return Err(operation_conflict("the operation has expired"));
        }

        let updated = self
            .transition_tx(
                &mut tx,
                &current,
                OperationState::Executing,
                &json!({"payloadHash": expected_payload_hash}),
                now,
            )
            .await?;
        tx.commit().await?;
        Ok(updated)
    }

    /// Record bounded progress without changing the current projection state.
    pub(crate) async fn append_progress(
        &self,
        operation_id: Uuid,
        runtime_id: Uuid,
        details: &Value,
    ) -> AppResult<OperationEventRecord> {
        let _guard = self.write_lock.lock().await;
        let mut tx = self.pool.begin().await?;
        let current = fetch_operation_tx(&mut tx, operation_id).await?;
        ensure_runtime(&current, runtime_id)?;
        if current.state != OperationState::Executing {
            return Err(operation_conflict(
                "progress can only be recorded for an executing operation",
            ));
        }
        let event = self
            .append_event_tx(
                &mut tx,
                operation_id,
                OperationEventKind::Progress,
                current.state,
                details,
                Utc::now(),
            )
            .await?;
        tx.commit().await?;
        Ok(event)
    }

    pub(crate) async fn events(&self, operation_id: Uuid) -> AppResult<Vec<OperationEventRecord>> {
        let rows = sqlx::query(
            "SELECT * FROM operation_events
             WHERE operation_id = ?1
             ORDER BY sequence ASC",
        )
        .bind(operation_id.to_string())
        .fetch_all(&self.pool)
        .await?;
        rows.iter().map(row_to_event).collect()
    }

    /// Verify sequence continuity, every hash link, canonical event JSON, and the
    /// agreement between the ledger tail and the current projection.
    pub(crate) async fn verify_event_chain(&self, operation_id: Uuid) -> AppResult<bool> {
        let _guard = self.write_lock.lock().await;
        let projection = self.get(operation_id).await?;
        let events = self.events(operation_id).await?;
        if events.is_empty() {
            return Ok(false);
        }
        let mut previous_hash: Option<&str> = None;
        for (index, event) in events.iter().enumerate() {
            if event.sequence != (index as i64) + 1
                || event.prev_hash.as_deref() != previous_hash
                || (index == 0
                    && (event.kind != OperationEventKind::Planned
                        || event.state != OperationState::Planned))
            {
                return Ok(false);
            }
            let event_json = canonical_json(&event.details)?;
            let expected = event_hash(EventHashInput {
                event_id: event.id,
                operation_id: event.operation_id,
                sequence: event.sequence,
                kind: event.kind,
                state: event.state,
                event_json: &event_json,
                created_at: event.created_at,
                prev_hash: event.prev_hash.as_deref(),
            })?;
            if expected != event.hash {
                return Ok(false);
            }
            previous_hash = Some(&event.hash);
        }
        Ok(events
            .last()
            .is_some_and(|event| event.state == projection.state))
    }

    /// Recover only operations owned by older runtimes. Mutations with an uncertain
    /// commit become `outcome_unknown`; resumable jobs remain untouched until their
    /// checkpoint is independently validated.
    pub(crate) async fn recover_previous_runtimes(
        &self,
        current_runtime_id: Uuid,
    ) -> AppResult<RestartRecoveryReport> {
        let _guard = self.write_lock.lock().await;
        let mut tx = self.pool.begin().await?;
        let rows = sqlx::query(
            "SELECT * FROM operations
             WHERE runtime_id <> ?1
               AND state NOT IN (
                   'rejected', 'expired', 'cancelled', 'succeeded', 'failed',
                   'outcome_unknown'
               )
             ORDER BY created_at ASC, id ASC",
        )
        .bind(current_runtime_id.to_string())
        .fetch_all(&mut *tx)
        .await?;
        let mut report = RestartRecoveryReport::default();
        for row in rows {
            let operation = row_to_operation(&row)?;
            match restart_recovery(operation.kind, operation.state) {
                RestartRecovery::KeepTerminal => {}
                RestartRecovery::Expire => {
                    self.transition_tx(
                        &mut tx,
                        &operation,
                        OperationState::Expired,
                        &json!({"reason": "runtime_restarted"}),
                        Utc::now(),
                    )
                    .await?;
                    report.expired.push(operation.id);
                }
                RestartRecovery::MarkFailed => {
                    self.transition_tx(
                        &mut tx,
                        &operation,
                        OperationState::Failed,
                        &json!({"reason": "runtime_interrupted_before_receipt"}),
                        Utc::now(),
                    )
                    .await?;
                    report.failed.push(operation.id);
                }
                RestartRecovery::OutcomeUnknown => {
                    self.transition_tx(
                        &mut tx,
                        &operation,
                        OperationState::OutcomeUnknown,
                        &json!({"reason": "target_commit_status_unknown_after_restart"}),
                        Utc::now(),
                    )
                    .await?;
                    report.outcome_unknown.push(operation.id);
                }
                RestartRecovery::ValidateJobCheckpoint => {
                    report.checkpoint_validation_required.push(operation.id);
                }
            }
        }
        tx.commit().await?;
        Ok(report)
    }

    async fn transition_tx(
        &self,
        tx: &mut Transaction<'_, Sqlite>,
        current: &OperationRecord,
        target: OperationState,
        details: &Value,
        now: DateTime<Utc>,
    ) -> AppResult<OperationRecord> {
        ensure_transition(current.state, target)
            .map_err(|error| operation_conflict(&error.to_string()))?;
        let now_text = timestamp(now);
        let terminal = target.is_terminal();
        let result = sqlx::query(
            "UPDATE operations
             SET state = ?1,
                 updated_at = ?2,
                 started_at = CASE WHEN ?1 = 'executing' THEN ?2 ELSE started_at END,
                 finished_at = CASE WHEN ?3 THEN ?2 ELSE finished_at END
             WHERE id = ?4 AND state = ?5",
        )
        .bind(state_str(target))
        .bind(&now_text)
        .bind(terminal)
        .bind(current.id.to_string())
        .bind(state_str(current.state))
        .execute(&mut **tx)
        .await?;
        if result.rows_affected() != 1 {
            return Err(operation_conflict(
                "the operation state changed before it could be updated",
            ));
        }
        self.append_event_tx(
            tx,
            current.id,
            transition_event_kind(target),
            target,
            details,
            now,
        )
        .await?;
        fetch_operation_tx(tx, current.id).await
    }

    async fn append_event_tx(
        &self,
        tx: &mut Transaction<'_, Sqlite>,
        operation_id: Uuid,
        kind: OperationEventKind,
        state: OperationState,
        details: &Value,
        created_at: DateTime<Utc>,
    ) -> AppResult<OperationEventRecord> {
        let details_json = canonical_json(details)?;
        if details_json.len() > MAX_RESPONSE_BYTES {
            return Err(AppError::Config(
                "operation event details exceed the local control-message limit".into(),
            ));
        }
        let tail = sqlx::query(
            "SELECT sequence, hash FROM operation_events
             WHERE operation_id = ?1
             ORDER BY sequence DESC
             LIMIT 1",
        )
        .bind(operation_id.to_string())
        .fetch_optional(&mut **tx)
        .await?;
        let (sequence, prev_hash) = match tail {
            Some(row) => {
                let sequence: i64 = row.try_get("sequence")?;
                let next = sequence.checked_add(1).ok_or_else(|| {
                    AppError::Config("operation event sequence overflowed".into())
                })?;
                (next, Some(row.try_get::<String, _>("hash")?))
            }
            None => (1, None),
        };
        let event_id = Uuid::new_v4();
        let hash = event_hash(EventHashInput {
            event_id,
            operation_id,
            sequence,
            kind,
            state,
            event_json: &details_json,
            created_at,
            prev_hash: prev_hash.as_deref(),
        })?;
        let created_at_text = timestamp(created_at);
        sqlx::query(
            "INSERT INTO operation_events (
                id, operation_id, sequence, event_kind, state, event_json,
                created_at, prev_hash, hash
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
        )
        .bind(event_id.to_string())
        .bind(operation_id.to_string())
        .bind(sequence)
        .bind(event_kind_str(kind))
        .bind(state_str(state))
        .bind(&details_json)
        .bind(&created_at_text)
        .bind(&prev_hash)
        .bind(&hash)
        .execute(&mut **tx)
        .await?;
        Ok(OperationEventRecord {
            id: event_id,
            operation_id,
            sequence,
            kind,
            state,
            details: serde_json::from_str(&details_json)?,
            created_at,
            prev_hash,
            hash,
        })
    }
}

struct PreparedOperation {
    operation: NewOperation,
    payload: CanonicalJson,
    actor_provenance_json: String,
    preview_json: String,
    policy_snapshot_json: String,
}

impl PreparedOperation {
    fn new(operation: NewOperation) -> AppResult<Self> {
        validate_new_operation(&operation)?;
        let payload = CanonicalJson::from_value(&operation.payload)?;
        if payload.json().len() > MAX_REQUEST_BYTES {
            return Err(AppError::Config(
                "operation payload exceeds the local control-message limit".into(),
            ));
        }
        let actor_provenance_json =
            canonical_json(&serde_json::to_value(&operation.actor.provenance)?)?;
        let preview_json = canonical_json(&operation.preview)?;
        let policy_snapshot_json = canonical_json(&operation.policy_snapshot)?;
        let metadata_bytes = actor_provenance_json
            .len()
            .saturating_add(preview_json.len())
            .saturating_add(policy_snapshot_json.len());
        if metadata_bytes > MAX_RESPONSE_BYTES {
            return Err(AppError::Config(
                "operation metadata exceeds the local control-message limit".into(),
            ));
        }
        Ok(Self {
            operation,
            payload,
            actor_provenance_json,
            preview_json,
            policy_snapshot_json,
        })
    }

    fn matches(&self, existing: &OperationRecord) -> bool {
        existing.runtime_id == self.operation.runtime_id
            && existing.workspace_id == self.operation.workspace_id
            && existing.account_scope == self.operation.account_scope
            && existing.connection_id == self.operation.connection_id
            && existing.connection_revision == self.operation.connection_revision
            && existing.terminal_session_id == self.operation.terminal_session_id
            && existing.actor == self.operation.actor
            && existing.kind == self.operation.kind
            && existing.payload_schema_version == self.operation.payload_schema_version
            && existing.payload_hash == self.payload.sha256()
            && existing.schema_fingerprint == self.operation.schema_fingerprint
            && existing.risk_level == self.operation.risk_level
            && existing.preview == self.operation.preview
            && existing.policy_snapshot == self.operation.policy_snapshot
            && existing.policy_revision == self.operation.policy_revision
            && existing.single_use == self.operation.single_use
            && existing.idempotency_key == self.operation.idempotency_key
            && existing.expires_at == self.operation.expires_at
    }
}

fn validate_new_operation(operation: &NewOperation) -> AppResult<()> {
    for (name, value) in [
        ("account scope", operation.account_scope.as_str()),
        ("actor id", operation.actor.id.as_str()),
        (
            "actor origin surface",
            operation.actor.provenance.origin_surface.as_str(),
        ),
        ("policy revision", operation.policy_revision.as_str()),
        ("idempotency key", operation.idempotency_key.as_str()),
    ] {
        if value.trim().is_empty() {
            return Err(AppError::Config(format!(
                "operation {name} cannot be empty"
            )));
        }
    }
    if operation.connection_revision < 1 {
        return Err(AppError::Config(
            "operation connection revision must be positive".into(),
        ));
    }
    if operation.payload_schema_version == 0 {
        return Err(AppError::Config(
            "operation payload schema version must be positive".into(),
        ));
    }
    if operation
        .schema_fingerprint
        .as_deref()
        .is_some_and(|value| {
            value.len() != 64
                || !value
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        })
    {
        return Err(AppError::Config(
            "operation schema fingerprint must be lowercase SHA-256".into(),
        ));
    }
    Ok(())
}

async fn fetch_operation_tx(
    tx: &mut Transaction<'_, Sqlite>,
    operation_id: Uuid,
) -> AppResult<OperationRecord> {
    let row = sqlx::query("SELECT * FROM operations WHERE id = ?1")
        .bind(operation_id.to_string())
        .fetch_optional(&mut **tx)
        .await?
        .ok_or_else(|| AppError::NotFound(format!("operation {operation_id}")))?;
    row_to_operation(&row)
}

fn row_to_operation(row: &sqlx::sqlite::SqliteRow) -> AppResult<OperationRecord> {
    let payload_json: String = row.try_get("payload_json")?;
    let payload_hash: String = row.try_get("payload_hash")?;
    let payload = CanonicalJson::from_stored(&payload_json, &payload_hash)?.into_value()?;
    let actor_provenance_json: String = row.try_get("actor_provenance_json")?;
    let actor_provenance_value = parse_canonical_json(&actor_provenance_json)?;
    let actor_provenance: OperationActorProvenance =
        serde_json::from_value(actor_provenance_value)?;
    let preview: Value = parse_canonical_json(row.try_get("preview_json")?)?;
    let policy_snapshot: Value = parse_canonical_json(row.try_get("policy_snapshot_json")?)?;

    Ok(OperationRecord {
        id: parse_uuid(row.try_get("id")?, "operation id")?,
        runtime_id: parse_uuid(row.try_get("runtime_id")?, "operation runtime id")?,
        workspace_id: parse_uuid(row.try_get("workspace_id")?, "operation workspace id")?,
        account_scope: row.try_get("account_scope")?,
        connection_id: parse_uuid(row.try_get("connection_id")?, "operation connection id")?,
        connection_revision: row.try_get("connection_revision")?,
        terminal_session_id: row
            .try_get::<Option<String>, _>("terminal_session_id")?
            .map(|value| parse_uuid(value, "operation terminal session id"))
            .transpose()?,
        actor: OperationActor {
            kind: parse_actor_kind(row.try_get::<String, _>("actor_kind")?.as_str())
                .ok_or_else(|| AppError::Config("invalid stored operation actor kind".into()))?,
            id: row.try_get("actor_id")?,
            provenance: actor_provenance,
        },
        kind: parse_operation_kind(row.try_get::<String, _>("operation_kind")?.as_str())
            .ok_or_else(|| AppError::Config("invalid stored operation kind".into()))?,
        payload_schema_version: u32::try_from(row.try_get::<i64, _>("payload_schema_version")?)
            .map_err(|_| AppError::Config("invalid operation payload schema version".into()))?,
        payload,
        payload_hash,
        schema_fingerprint: row.try_get("schema_fingerprint")?,
        risk_level: parse_risk_level(row.try_get::<String, _>("risk_level")?.as_str())
            .ok_or_else(|| AppError::Config("invalid stored operation risk level".into()))?,
        preview,
        policy_snapshot,
        policy_revision: row.try_get("policy_revision")?,
        state: parse_state(row.try_get::<String, _>("state")?.as_str())
            .ok_or_else(|| AppError::Config("invalid stored operation state".into()))?,
        single_use: parse_bool(row.try_get("single_use")?, "operation single_use")?,
        idempotency_key: row.try_get("idempotency_key")?,
        expires_at: parse_optional_timestamp(row.try_get("expires_at")?, "operation expiry")?,
        started_at: parse_optional_timestamp(row.try_get("started_at")?, "operation start")?,
        finished_at: parse_optional_timestamp(row.try_get("finished_at")?, "operation finish")?,
        created_at: parse_timestamp(row.try_get("created_at")?, "operation creation")?,
        updated_at: parse_timestamp(row.try_get("updated_at")?, "operation update")?,
    })
}

fn row_to_event(row: &sqlx::sqlite::SqliteRow) -> AppResult<OperationEventRecord> {
    let event_json: String = row.try_get("event_json")?;
    Ok(OperationEventRecord {
        id: parse_uuid(row.try_get("id")?, "operation event id")?,
        operation_id: parse_uuid(row.try_get("operation_id")?, "operation event operation id")?,
        sequence: row.try_get("sequence")?,
        kind: parse_event_kind(row.try_get::<String, _>("event_kind")?.as_str())
            .ok_or_else(|| AppError::Config("invalid stored operation event kind".into()))?,
        state: parse_state(row.try_get::<String, _>("state")?.as_str())
            .ok_or_else(|| AppError::Config("invalid stored operation event state".into()))?,
        details: parse_canonical_json(&event_json)?,
        created_at: parse_timestamp(row.try_get("created_at")?, "operation event creation")?,
        prev_hash: row.try_get("prev_hash")?,
        hash: row.try_get("hash")?,
    })
}

fn parse_canonical_json(json: &str) -> AppResult<Value> {
    let value: Value = serde_json::from_str(json)?;
    if canonical_json(&value)? != json {
        return Err(AppError::Config(
            "stored operation JSON is not canonical".into(),
        ));
    }
    Ok(value)
}

fn parse_uuid(value: String, field: &str) -> AppResult<Uuid> {
    Uuid::parse_str(&value)
        .map_err(|_| AppError::Config(format!("invalid {field} in local operation store")))
}

fn parse_bool(value: i64, field: &str) -> AppResult<bool> {
    match value {
        0 => Ok(false),
        1 => Ok(true),
        _ => Err(AppError::Config(format!(
            "invalid {field} in local operation store"
        ))),
    }
}

fn timestamp(value: DateTime<Utc>) -> String {
    value.to_rfc3339_opts(SecondsFormat::Nanos, true)
}

fn parse_timestamp(value: String, field: &str) -> AppResult<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(&value)
        .map(|value| value.with_timezone(&Utc))
        .map_err(|_| AppError::Config(format!("invalid {field} timestamp")))
}

fn parse_optional_timestamp(
    value: Option<String>,
    field: &str,
) -> AppResult<Option<DateTime<Utc>>> {
    value.map(|value| parse_timestamp(value, field)).transpose()
}

fn ensure_runtime(operation: &OperationRecord, runtime_id: Uuid) -> AppResult<()> {
    if operation.runtime_id == runtime_id {
        Ok(())
    } else {
        Err(operation_conflict(
            "the operation belongs to a previous application runtime",
        ))
    }
}

fn transition_event_kind(target: OperationState) -> OperationEventKind {
    match target {
        OperationState::Planned | OperationState::Ready => OperationEventKind::Planned,
        OperationState::PendingApproval => OperationEventKind::ApprovalRequested,
        OperationState::Approved => OperationEventKind::Approved,
        OperationState::Rejected => OperationEventKind::Rejected,
        OperationState::Expired => OperationEventKind::Expired,
        OperationState::Cancelled => OperationEventKind::Cancelled,
        OperationState::Executing => OperationEventKind::ExecutionStarted,
        OperationState::Succeeded => OperationEventKind::Succeeded,
        OperationState::Failed => OperationEventKind::Failed,
        OperationState::OutcomeUnknown => OperationEventKind::OutcomeUnknown,
    }
}

struct EventHashInput<'a> {
    event_id: Uuid,
    operation_id: Uuid,
    sequence: i64,
    kind: OperationEventKind,
    state: OperationState,
    event_json: &'a str,
    created_at: DateTime<Utc>,
    prev_hash: Option<&'a str>,
}

fn event_hash(input: EventHashInput<'_>) -> AppResult<String> {
    let canonical = canonical_json(&json!({
        "createdAt": timestamp(input.created_at),
        "eventId": input.event_id,
        "eventJson": input.event_json,
        "eventKind": event_kind_str(input.kind),
        "operationId": input.operation_id,
        "prevHash": input.prev_hash,
        "sequence": input.sequence,
        "state": state_str(input.state),
    }))?;
    Ok(lower_hex(&Sha256::digest(canonical.as_bytes())))
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

fn operation_conflict(reason: &str) -> AppError {
    AppError::Blocked {
        reason: reason.into(),
    }
}

#[cfg(test)]
mod tests {
    use std::str::FromStr;

    use chrono::Duration;
    use dopedb_protocol::{OperationActorKind, OperationKind, OperationRiskLevel};
    use serde_json::json;
    use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};

    use super::*;
    use crate::store::TEST_SCHEMA;

    async fn repository() -> (OperationRepository, SqlitePool) {
        let options = SqliteConnectOptions::from_str("sqlite::memory:")
            .unwrap()
            .foreign_keys(true);
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(options)
            .await
            .unwrap();
        sqlx::raw_sql(TEST_SCHEMA).execute(&pool).await.unwrap();
        (OperationRepository::from_pool(pool.clone()), pool)
    }

    fn operation(kind: OperationKind, runtime_id: Uuid, idempotency_key: &str) -> NewOperation {
        NewOperation {
            id: Uuid::new_v4(),
            runtime_id,
            workspace_id: Uuid::parse_str("00000000-0000-0000-0000-000000000001").unwrap(),
            account_scope: "personal".into(),
            connection_id: Uuid::new_v4(),
            connection_revision: 1,
            terminal_session_id: Some(Uuid::new_v4()),
            actor: OperationActor {
                kind: OperationActorKind::LocalUser,
                id: "local-owner".into(),
                provenance: OperationActorProvenance {
                    client_protocol_version: Some(1),
                    origin_surface: "sql_editor".into(),
                    ..OperationActorProvenance::default()
                },
            },
            kind,
            payload_schema_version: 1,
            payload: json!({"sql": "SELECT 1", "parameters": []}),
            schema_fingerprint: Some("a".repeat(64)),
            risk_level: OperationRiskLevel::Low,
            preview: json!({"statementCount": 1}),
            policy_snapshot: json!({"allowWrites": false}),
            policy_revision: "local-policy-v1".into(),
            single_use: true,
            idempotency_key: idempotency_key.into(),
            expires_at: Some(Utc::now() + Duration::minutes(5)),
        }
    }

    #[tokio::test]
    async fn schema_is_idempotent_and_contains_all_operation_tables() {
        let (_, pool) = repository().await;
        sqlx::raw_sql(TEST_SCHEMA).execute(&pool).await.unwrap();
        for table in ["operations", "operation_approvals", "operation_events"] {
            let exists: i64 = sqlx::query_scalar(
                "SELECT EXISTS(
                    SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = ?1
                 )",
            )
            .bind(table)
            .fetch_one(&pool)
            .await
            .unwrap();
            assert_eq!(exists, 1, "{table}");
        }
    }

    #[tokio::test]
    async fn insertion_derives_hash_and_appends_a_verifiable_initial_event() {
        let (repository, _) = repository().await;
        let runtime_id = Uuid::new_v4();
        let record = repository
            .insert_planned(operation(
                OperationKind::ReadQuery,
                runtime_id,
                "insert-once",
            ))
            .await
            .unwrap();
        assert_eq!(record.state, OperationState::Planned);
        assert_eq!(record.payload_hash.len(), 64);
        assert_eq!(record.runtime_id, runtime_id);
        assert_eq!(record.started_at, None);
        assert_eq!(record.finished_at, None);
        assert!(repository.verify_event_chain(record.id).await.unwrap());
        let events = repository.events(record.id).await.unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].kind, OperationEventKind::Planned);
        assert_eq!(events[0].sequence, 1);
        assert_eq!(events[0].prev_hash, None);
    }

    #[tokio::test]
    async fn idempotency_returns_the_exact_existing_record_and_rejects_key_rebinding() {
        let (repository, _) = repository().await;
        let runtime_id = Uuid::new_v4();
        let first_request = operation(OperationKind::ReadQuery, runtime_id, "same-request");
        let first = repository
            .insert_planned(first_request.clone())
            .await
            .unwrap();
        let mut retry = first_request;
        retry.id = Uuid::new_v4();
        let replay = repository.insert_planned(retry).await.unwrap();
        assert_eq!(replay.id, first.id);
        assert_eq!(repository.events(first.id).await.unwrap().len(), 1);

        let mut conflicting = operation(OperationKind::ReadQuery, runtime_id, "same-request");
        conflicting.connection_id = first.connection_id;
        conflicting.payload = json!({"sql": "SELECT 2", "parameters": []});
        assert!(matches!(
            repository.insert_planned(conflicting).await,
            Err(AppError::Blocked { .. })
        ));
    }

    #[tokio::test]
    async fn immutable_projection_and_append_only_ledgers_reject_direct_updates() {
        let (repository, pool) = repository().await;
        let record = repository
            .insert_planned(operation(
                OperationKind::ReadQuery,
                Uuid::new_v4(),
                "immutable",
            ))
            .await
            .unwrap();
        assert!(
            sqlx::query("UPDATE operations SET payload_json = '{}' WHERE id = ?1")
                .bind(record.id.to_string())
                .execute(&pool)
                .await
                .is_err()
        );
        assert!(
            sqlx::query("UPDATE operations SET connection_revision = 2 WHERE id = ?1")
                .bind(record.id.to_string())
                .execute(&pool)
                .await
                .is_err()
        );
        assert!(sqlx::query("DELETE FROM operations WHERE id = ?1")
            .bind(record.id.to_string())
            .execute(&pool)
            .await
            .is_err());
        assert!(sqlx::query(
            "UPDATE operation_events SET event_json = '{}' WHERE operation_id = ?1"
        )
        .bind(record.id.to_string())
        .execute(&pool)
        .await
        .is_err());

        sqlx::query(
            "INSERT INTO operation_approvals (
                id, operation_id, payload_hash, approver_kind, approver_id,
                decision, policy_revision, created_at
             ) VALUES (?1, ?2, ?3, 'local_user', 'local-owner', 'approved', ?4, ?5)",
        )
        .bind(Uuid::new_v4().to_string())
        .bind(record.id.to_string())
        .bind(&record.payload_hash)
        .bind(&record.policy_revision)
        .bind(timestamp(Utc::now()))
        .execute(&pool)
        .await
        .unwrap();
        assert!(
            sqlx::query("UPDATE operation_approvals SET decision = 'rejected'")
                .execute(&pool)
                .await
                .is_err()
        );
        assert!(sqlx::query("DELETE FROM operation_approvals")
            .execute(&pool)
            .await
            .is_err());
    }

    #[tokio::test]
    async fn connection_deletion_cannot_delete_operation_provenance() {
        let (repository, pool) = repository().await;
        let request = operation(
            OperationKind::ReadQuery,
            Uuid::new_v4(),
            "connection-delete",
        );
        sqlx::query(
            "INSERT INTO connections (
                id, name, engine, host, port, db_name, username, sslmode,
                extra_params, readonly_default, allow_writes, created_at, updated_at
             ) VALUES (?1, 'fixture', 'sqlite', '', 0, ':memory:', '', 'disable',
                       '{}', 1, 0, ?2, ?2)",
        )
        .bind(request.connection_id.to_string())
        .bind(timestamp(Utc::now()))
        .execute(&pool)
        .await
        .unwrap();
        let record = repository.insert_planned(request.clone()).await.unwrap();
        sqlx::query("DELETE FROM connections WHERE id = ?1")
            .bind(request.connection_id.to_string())
            .execute(&pool)
            .await
            .unwrap();
        assert_eq!(repository.get(record.id).await.unwrap().id, record.id);
        assert!(repository.verify_event_chain(record.id).await.unwrap());
    }

    #[tokio::test]
    async fn claim_is_single_use_runtime_scoped_hash_bound_and_expiry_aware() {
        let (repository, _) = repository().await;
        let runtime_id = Uuid::new_v4();
        let planned = repository
            .insert_planned(operation(
                OperationKind::ReadQuery,
                runtime_id,
                "single-claim",
            ))
            .await
            .unwrap();
        let ready = repository
            .transition(planned.id, runtime_id, OperationState::Ready, &json!({}))
            .await
            .unwrap();
        assert!(repository
            .claim_execution(ready.id, Uuid::new_v4(), &ready.payload_hash, Utc::now())
            .await
            .is_err());
        assert!(repository
            .claim_execution(ready.id, runtime_id, &"0".repeat(64), Utc::now())
            .await
            .is_err());

        let first =
            repository.claim_execution(ready.id, runtime_id, &ready.payload_hash, Utc::now());
        let second =
            repository.claim_execution(ready.id, runtime_id, &ready.payload_hash, Utc::now());
        let (first, second) = tokio::join!(first, second);
        assert_eq!(usize::from(first.is_ok()) + usize::from(second.is_ok()), 1);
        assert_eq!(
            repository.get(ready.id).await.unwrap().state,
            OperationState::Executing
        );

        let mut expired_request = operation(OperationKind::ReadQuery, runtime_id, "expired-claim");
        expired_request.expires_at = Some(Utc::now() - Duration::seconds(1));
        let expired = repository.insert_planned(expired_request).await.unwrap();
        let expired = repository
            .transition(expired.id, runtime_id, OperationState::Ready, &json!({}))
            .await
            .unwrap();
        assert!(repository
            .claim_execution(expired.id, runtime_id, &expired.payload_hash, Utc::now())
            .await
            .is_err());
        assert_eq!(
            repository.get(expired.id).await.unwrap().state,
            OperationState::Expired
        );
    }

    #[tokio::test]
    async fn restart_recovery_never_retries_an_uncertain_mutation() {
        let (repository, _) = repository().await;
        let previous_runtime = Uuid::new_v4();
        let current_runtime = Uuid::new_v4();

        let stale_plan = repository
            .insert_planned(operation(
                OperationKind::ReadQuery,
                previous_runtime,
                "stale-plan",
            ))
            .await
            .unwrap();

        let read = repository
            .insert_planned(operation(
                OperationKind::ReadQuery,
                previous_runtime,
                "interrupted-read",
            ))
            .await
            .unwrap();
        let read = repository
            .transition(read.id, previous_runtime, OperationState::Ready, &json!({}))
            .await
            .unwrap();
        repository
            .claim_execution(read.id, previous_runtime, &read.payload_hash, Utc::now())
            .await
            .unwrap();

        let write = repository
            .insert_planned(operation(
                OperationKind::WriteSql,
                previous_runtime,
                "interrupted-write",
            ))
            .await
            .unwrap();
        let write = repository
            .transition(
                write.id,
                previous_runtime,
                OperationState::Ready,
                &json!({}),
            )
            .await
            .unwrap();
        repository
            .claim_execution(write.id, previous_runtime, &write.payload_hash, Utc::now())
            .await
            .unwrap();

        let import = repository
            .insert_planned(operation(
                OperationKind::Import,
                previous_runtime,
                "interrupted-import",
            ))
            .await
            .unwrap();
        let import = repository
            .transition(
                import.id,
                previous_runtime,
                OperationState::Ready,
                &json!({}),
            )
            .await
            .unwrap();
        repository
            .claim_execution(
                import.id,
                previous_runtime,
                &import.payload_hash,
                Utc::now(),
            )
            .await
            .unwrap();

        let report = repository
            .recover_previous_runtimes(current_runtime)
            .await
            .unwrap();
        assert_eq!(report.expired, vec![stale_plan.id]);
        assert_eq!(report.failed, vec![read.id]);
        assert_eq!(report.outcome_unknown, vec![write.id]);
        assert_eq!(report.checkpoint_validation_required, vec![import.id]);
        assert_eq!(
            repository.get(write.id).await.unwrap().state,
            OperationState::OutcomeUnknown
        );
        assert_eq!(
            repository.get(import.id).await.unwrap().state,
            OperationState::Executing
        );
        assert!(repository.verify_event_chain(write.id).await.unwrap());
        assert!(repository
            .claim_execution(write.id, current_runtime, &write.payload_hash, Utc::now())
            .await
            .is_err());
    }

    #[tokio::test]
    async fn hash_chain_and_payload_loader_detect_out_of_band_tampering() {
        let (repository, pool) = repository().await;
        let record = repository
            .insert_planned(operation(
                OperationKind::ReadQuery,
                Uuid::new_v4(),
                "tamper",
            ))
            .await
            .unwrap();
        sqlx::query("DROP TRIGGER operation_events_reject_update")
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query(
            "UPDATE operation_events SET event_json = '{\"tampered\":true}'
             WHERE operation_id = ?1 AND sequence = 1",
        )
        .bind(record.id.to_string())
        .execute(&pool)
        .await
        .unwrap();
        assert!(!repository.verify_event_chain(record.id).await.unwrap());

        sqlx::query("DROP TRIGGER operations_reject_immutable_update")
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query("UPDATE operations SET payload_json = '{\"sql\":\"SELECT 2\"}' WHERE id = ?1")
            .bind(record.id.to_string())
            .execute(&pool)
            .await
            .unwrap();
        assert!(repository.get(record.id).await.is_err());
    }

    #[tokio::test]
    async fn progress_keeps_projection_state_and_extends_the_hash_chain() {
        let (repository, _) = repository().await;
        let runtime_id = Uuid::new_v4();
        let operation = repository
            .insert_planned(operation(OperationKind::Export, runtime_id, "progress"))
            .await
            .unwrap();
        let operation = repository
            .transition(operation.id, runtime_id, OperationState::Ready, &json!({}))
            .await
            .unwrap();
        let operation = repository
            .claim_execution(
                operation.id,
                runtime_id,
                &operation.payload_hash,
                Utc::now(),
            )
            .await
            .unwrap();
        let event = repository
            .append_progress(operation.id, runtime_id, &json!({"completedRows": 100}))
            .await
            .unwrap();
        assert_eq!(event.kind, OperationEventKind::Progress);
        assert_eq!(event.state, OperationState::Executing);
        assert_eq!(
            repository.get(operation.id).await.unwrap().state,
            OperationState::Executing
        );
        assert!(repository.verify_event_chain(operation.id).await.unwrap());
    }
}
