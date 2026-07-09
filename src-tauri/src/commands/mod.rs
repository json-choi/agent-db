//! The `#[tauri::command]` boundary. Thin wiring only — each command validates
//! inputs, routes to the real module functions (store / connection / introspect /
//! agent / safety / executor / audit), and returns an [`AppResult`] that serializes
//! to `{ kind, message }` for the frontend.
//!
//! Safety invariant enforced here: writes/DDL/privilege are blocked unless the
//! connection's `allow_writes` is on AND the call is `approved`. L4 (`decide`) makes
//! the policy call; the executor re-checks both gates as defense in depth; L2 (the
//! DB's own read-only session) remains the authoritative stop.

use chrono::Utc;
use std::time::Duration;
use tauri::State;
use uuid::Uuid;

use crate::audit::{self, RecordArgs};
use crate::connection::{self, DbPool, LiveConnection};
use crate::error::{AppError, AppResult};
use crate::executor;
use crate::introspect;
use crate::model::{
    AuditEntry, Classification, ConnectionProfile, Engine, ExecOutcome, HistoryEntry, PreviewMode,
    PreviewReport, QueryKind, SafetySettings,
};
use crate::safety::{classify, decide, preview, GateDecision, PoolRef};
use crate::state::AppState;

// ── helpers ──────────────────────────────────────────────────────────────────

/// Borrow a `DbPool` as a `safety::PoolRef` (the L2/L3 entry handle).
fn pool_ref(db: &DbPool) -> PoolRef<'_> {
    match db {
        DbPool::Postgres(p) => PoolRef::Postgres(p),
        DbPool::Mysql(p) => PoolRef::Mysql(p),
        DbPool::Sqlite(p) => PoolRef::Sqlite(p),
    }
}

/// Get a live connection for `id`, opening (and caching) one on first use.
///
/// Never holds the connections lock across an `.await`: it either clones an existing
/// handle out under the lock, or opens a fresh one and inserts the clone afterwards.
async fn get_live(state: &AppState, id: Uuid) -> AppResult<LiveConnection> {
    if let Some(existing) = state.connections.lock().unwrap().get(&id) {
        return Ok(existing.clone());
    }
    let profile = state.store.get_connection(id).await?;
    // SQLite has no password; PG/MySQL may authenticate via socket/trust with none.
    let secret = connection::fetch_secret(&id).unwrap_or_default();
    let live = connection::connect(&profile, &secret).await?;
    let handle = live.clone();
    state.connections.lock().unwrap().insert(id, live);
    Ok(handle)
}

/// Best-effort resolve the introspected catalog: schema cache first, live DB otherwise.
async fn load_catalog(state: &AppState, id: Uuid) -> AppResult<introspect::Catalog> {
    if let Some(json) = state.store.get_schema_cache(id).await? {
        if let Ok(cat) = serde_json::from_str::<introspect::Catalog>(&json) {
            return Ok(cat);
        }
    }
    let live = get_live(state, id).await?;
    let catalog = introspect::introspect(&live).await?;
    let _ = state
        .store
        .set_schema_cache(id, &serde_json::to_string(&catalog)?)
        .await;
    Ok(catalog)
}

/// Append an audit record (always) and a query-history row for one run/attempt.
/// Best-effort outcome logging: failures never mask the actual command result, but
/// they are logged (never silently dropped) so a dropped compliance row is visible.
#[allow(clippy::too_many_arguments)]
async fn record_run(
    state: &AppState,
    id: Uuid,
    engine: Engine,
    sql: &str,
    kind: QueryKind,
    action: &str,
    status: &str,
    row_count: Option<i64>,
    duration_ms: Option<i64>,
    error: Option<String>,
    origin: &str,
) {
    if let Err(e) = audit::record(
        &state.store,
        RecordArgs {
            connection_id: id,
            engine,
            agent_prompt: None,
            sql: sql.to_string(),
            kind,
            action: action.to_string(),
            approved_by: None,
            affected_estimate: row_count,
            error: error.clone(),
        },
    )
    .await
    {
        tracing::error!("audit record ({action}) failed for connection {id}: {e}");
    }
    if let Err(e) = state
        .store
        .insert_history(&HistoryEntry {
            id: Uuid::new_v4(),
            connection_id: id,
            sql: sql.to_string(),
            kind,
            status: status.to_string(),
            row_count,
            duration_ms,
            error,
            executed_at: Utc::now(),
            origin: origin.to_string(),
        })
        .await
    {
        tracing::error!("history insert failed for connection {id}: {e}");
    }
}

// ── connection CRUD ──────────────────────────────────────────────────────────

#[tauri::command]
pub async fn list_connections(state: State<'_, AppState>) -> AppResult<Vec<ConnectionProfile>> {
    state.store.list_connections().await
}

#[tauri::command]
pub async fn upsert_connection(
    state: State<'_, AppState>,
    profile: ConnectionProfile,
    password: Option<String>,
) -> AppResult<ConnectionProfile> {
    let mut profile = profile;
    // Stash any supplied secret in the OS credential store and point the profile at it.
    if let Some(pw) = password.filter(|p| !p.is_empty()) {
        connection::store_secret(&profile.id, &pw)?;
        profile.secret_ref = Some(profile.id.to_string());
    }
    // Drop any cached live connection so new credentials/host take effect next use,
    // and invalidate the cached schema (an edit may repoint the connection at a
    // different database — otherwise the stale table list would persist).
    state.connections.lock().unwrap().remove(&profile.id);
    let _ = state.store.clear_schema_cache(profile.id).await;
    state.store.upsert_connection(&profile).await
}

#[tauri::command]
pub async fn set_connection_schema_group(
    state: State<'_, AppState>,
    id: Uuid,
    schema_group: Option<String>,
) -> AppResult<ConnectionProfile> {
    let normalized = schema_group.and_then(|value| {
        let trimmed = value.trim().to_string();
        if trimmed.is_empty() { None } else { Some(trimmed) }
    });
    state.store.set_connection_schema_group(id, normalized).await
}

#[tauri::command]
pub async fn delete_connection(state: State<'_, AppState>, id: Uuid) -> AppResult<()> {
    state.store.delete_connection(id).await?;
    let _ = connection::delete_secret(&id);
    state.connections.lock().unwrap().remove(&id);
    Ok(())
}

#[tauri::command]
pub async fn test_connection(state: State<'_, AppState>, id: Uuid) -> AppResult<()> {
    let profile = state.store.get_connection(id).await?;
    let secret = connection::fetch_secret(&id).unwrap_or_default();
    let live = connection::connect(&profile, &secret).await?;
    live.test().await
}

/// Dial an ad-hoc (possibly unsaved) profile purely to check that it connects.
/// Persists NOTHING — no store row, no credential-store write, no cached pool. This is the
/// connection form's "Test connection" button: a literal reachability check.
#[tauri::command]
pub async fn test_connection_profile(
    profile: ConnectionProfile,
    password: Option<String>,
) -> AppResult<()> {
    let secret = password.unwrap_or_default();
    let live = connection::connect(&profile, &secret).await?;
    live.test().await
}

// ── schema ───────────────────────────────────────────────────────────────────

#[tauri::command]
pub async fn get_schema(state: State<'_, AppState>, id: Uuid) -> AppResult<String> {
    let catalog = load_catalog(&state, id).await?;
    Ok(serde_json::to_string(&catalog)?)
}

/// Force a live re-introspection (bypassing the cache) and update the cache. Use this
/// when the table list is stale — the cache is otherwise written once and never expires.
#[tauri::command]
pub async fn refresh_schema(state: State<'_, AppState>, id: Uuid) -> AppResult<String> {
    let _ = state.store.clear_schema_cache(id).await;
    let live = get_live(&state, id).await?;
    let catalog = introspect::introspect(&live).await?;
    let _ = state
        .store
        .set_schema_cache(id, &serde_json::to_string(&catalog)?)
        .await;
    Ok(serde_json::to_string(&catalog)?)
}

// ── safety pipeline (L1 / L3) ────────────────────────────────────────────────

#[tauri::command]
pub async fn classify_sql(
    state: State<'_, AppState>,
    id: Uuid,
    sql: String,
) -> AppResult<Classification> {
    let profile = state.store.get_connection(id).await?;
    classify(&sql, profile.engine)
}

#[tauri::command]
pub async fn preview_sql(
    state: State<'_, AppState>,
    id: Uuid,
    sql: String,
) -> AppResult<PreviewReport> {
    let profile = state.store.get_connection(id).await?;
    let settings = state.store.get_safety(id).await?;
    let classification = classify(&sql, profile.engine)?;
    let needs_write_pool = !matches!(classification.kind, QueryKind::Read);

    // A write/DDL preview runs a real execute+rollback (takes row locks, fires
    // triggers/NOTIFY). Never do that on a writes-disabled connection — skip it.
    if needs_write_pool && !settings.allow_writes {
        return Ok(PreviewReport {
            mode: PreviewMode::Skipped,
            estimated_rows: None,
            exact_rows: None,
            plan: None,
            note: Some(
                "writes are disabled for this connection — impact preview skipped (no rows locked)"
                    .into(),
            ),
        });
    }

    let live = get_live(&state, id).await?;

    // explain_preview off → EXPLAIN-plan only, never execute+rollback. EXPLAIN (no
    // ANALYZE) plans a write without running it, so route the write through the
    // Read/EXPLAIN branch to honor the toggle.
    if matches!(classification.kind, QueryKind::Write) && !settings.explain_preview {
        let explain_only = Classification {
            kind: QueryKind::Read,
            ..classification
        };
        return preview(pool_ref(&live.write_pool), &sql, &explain_only, &settings).await;
    }

    // Reads preview via EXPLAIN on the read-only pool; write previews execute + roll
    // back, which must run on the read/write pool.
    let db = if needs_write_pool {
        &live.write_pool
    } else {
        &live.read_pool
    };
    preview(pool_ref(db), &sql, &classification, &settings).await
}

// ── execution (L4 gate → executor → audit) ───────────────────────────────────

#[tauri::command]
pub async fn run_sql(
    state: State<'_, AppState>,
    id: Uuid,
    sql: String,
    approved: bool,
    // Optional so existing frontend invokes keep working. `query_id` wires the
    // executor cancel slot; `origin` tags the history row (agent/data-view vs manual).
    query_id: Option<Uuid>,
    origin: Option<String>,
) -> AppResult<ExecOutcome> {
    let profile = state.store.get_connection(id).await?;
    let settings = state.store.get_safety(id).await?;
    let classification = classify(&sql, profile.engine)?;
    let engine = profile.engine;
    let origin = origin.unwrap_or_else(|| "manual".into());
    let is_write = !matches!(classification.kind, QueryKind::Read);

    // L4 policy decision (writes off / multi-statement → hard block).
    let decision = decide(&settings, &classification);
    match &decision {
        GateDecision::Block { reason } => {
            let reason = reason.clone();
            record_run(
                &state, id, engine, &sql, classification.kind, "blocked", "blocked", None, None,
                Some(reason.clone()), &origin,
            )
            .await;
            return Err(AppError::Blocked { reason });
        }
        GateDecision::RequireApproval if !approved => {
            let reason = "this statement modifies data and requires explicit approval".to_string();
            record_run(
                &state, id, engine, &sql, classification.kind, "blocked", "blocked", None, None,
                Some(reason.clone()), &origin,
            )
            .await;
            return Err(AppError::Blocked { reason });
        }
        _ => {}
    }

    // A write the gate auto-approved (require_approval=false + allow_writes=true) must
    // clear the executor's defense-in-depth `approved` check too — otherwise AutoRun
    // writes would be blocked one layer down.
    let authorized = approved || matches!(decision, GateDecision::AutoRun);

    // Compliance: a committed write MUST leave an audit trail. Insert the attempt row
    // BEFORE touching the DB and fail closed if we can't persist it — otherwise a
    // crash mid-write (or a post-run logging failure) could leave zero audit rows.
    if is_write {
        audit::record(
            &state.store,
            RecordArgs {
                connection_id: id,
                engine,
                agent_prompt: None,
                sql: sql.clone(),
                kind: classification.kind,
                action: "execute:attempt".into(),
                approved_by: None,
                affected_estimate: None,
                error: None,
            },
        )
        .await
        .map_err(|e| {
            AppError::Config(format!("audit pre-record failed — refusing to run write: {e}"))
        })?;
    }

    let live = get_live(&state, id).await?;
    match executor::execute(&live, engine, &classification, &sql, &settings, authorized, query_id)
        .await
    {
        Ok(outcome) => {
            // A committed DDL changed the catalog — drop the cached schema so the
            // sidebar/agent don't serve a stale table list.
            if matches!(classification.kind, QueryKind::Ddl) && outcome.committed {
                let _ = state.store.clear_schema_cache(id).await;
            }
            let row_count = outcome
                .result
                .as_ref()
                .map(|r| r.row_count as i64)
                .or_else(|| outcome.affected.map(|a| a as i64));
            let duration_ms = outcome.result.as_ref().map(|r| r.duration_ms as i64);
            let action = if outcome.committed { "execute" } else { "read" };
            record_run(
                &state, id, engine, &sql, classification.kind, action, "ok", row_count,
                duration_ms, None, &origin,
            )
            .await;
            Ok(outcome)
        }
        Err(e) => {
            record_run(
                &state, id, engine, &sql, classification.kind, "error", "error", None, None,
                Some(e.to_string()), &origin,
            )
            .await;
            Err(e)
        }
    }
}

// ── multi-statement script execution ─────────────────────────────────────────

fn stmt_ok(sql: &str, affected: u64) -> crate::model::ScriptStatement {
    crate::model::ScriptStatement {
        sql: sql.to_string(),
        result: None,
        affected: Some(affected as i64),
        error: None,
    }
}

fn stmt_err(sql: &str, msg: String) -> crate::model::ScriptStatement {
    crate::model::ScriptStatement { sql: sql.to_string(), result: None, affected: None, error: Some(msg) }
}

fn stmt_skipped(sql: &str) -> crate::model::ScriptStatement {
    stmt_err(sql, "skipped — transaction rolled back".into())
}

/// True when a script must take the write path (any statement isn't a plain read).
fn script_has_write(kinds: &[QueryKind]) -> bool {
    kinds.iter().any(|k| !matches!(k, QueryKind::Read))
}

/// Run every statement in ONE write-pool transaction, capturing each statement's
/// affected count. First error rolls back the whole transaction; the failing
/// statement gets its error, every statement after it is marked skipped, and nothing
/// commits. Returns `(per-statement outcomes, committed)`.
// ponytail: SELECTs inside a write script report `affected` only (no grid). To see
// query results, run a read-only script — that path streams rows per statement.
// NOTE: mirrors migrations::applied::run_in_tx — MySQL implicit-commits DDL, so a
// mixed DDL script's atomicity is best-effort there.
async fn execute_script_tx(
    pool: &DbPool,
    statements: &[String],
) -> (Vec<crate::model::ScriptStatement>, bool) {
    macro_rules! run_tx {
        ($p:expr) => {{
            let mut out: Vec<crate::model::ScriptStatement> = Vec::with_capacity(statements.len());
            match $p.begin().await {
                Ok(mut tx) => {
                    let mut ok = true;
                    for s in statements {
                        match sqlx::query(s).execute(&mut *tx).await {
                            Ok(r) => out.push(stmt_ok(s, r.rows_affected())),
                            Err(e) => {
                                out.push(stmt_err(s, e.to_string()));
                                ok = false;
                                break;
                            }
                        }
                    }
                    if !ok {
                        let _ = tx.rollback().await;
                        while out.len() < statements.len() {
                            out.push(stmt_skipped(&statements[out.len()]));
                        }
                        (out, false)
                    } else if let Err(e) = tx.commit().await {
                        // Commit itself failed → nothing persisted; flag every statement.
                        let msg = format!("commit failed — nothing was saved: {e}");
                        for st in &mut out {
                            st.error = Some(msg.clone());
                            st.affected = None;
                        }
                        (out, false)
                    } else {
                        (out, true)
                    }
                }
                Err(e) => (
                    statements
                        .iter()
                        .map(|s| stmt_err(s, format!("could not begin transaction: {e}")))
                        .collect(),
                    false,
                ),
            }
        }};
    }
    match pool {
        DbPool::Postgres(p) => run_tx!(p),
        DbPool::Mysql(p) => run_tx!(p),
        DbPool::Sqlite(p) => run_tx!(p),
    }
}

/// Run a pasted multi-statement script. Splits into statements (comment-only skipped),
/// classifies EACH via L1, then:
/// - all reads → run sequentially on the read-only pool (honoring `auto_run_reads`,
///   `max_rows` per statement), stopping at the first error;
/// - any write/DDL → require `approved` AND `allow_writes`, then run ALL statements in
///   ONE write-pool transaction (rollback on the first error).
///
/// This is the escape hatch from `run_sql`'s single-statement L4 hard block: a seed or
/// migration-style file that `run_sql` refuses can run here, still gated + audited.
#[tauri::command]
pub async fn run_script(
    state: State<'_, AppState>,
    id: Uuid,
    sql: String,
    approved: bool,
    query_id: Option<Uuid>,
    origin: Option<String>,
) -> AppResult<crate::model::ScriptOutcome> {
    use crate::model::{ScriptOutcome, ScriptStatement};

    let profile = state.store.get_connection(id).await?;
    let settings = state.store.get_safety(id).await?;
    let engine = profile.engine;
    let origin = origin.unwrap_or_else(|| "manual".into());

    // Split + drop comment-only statements (reusing the migration scanner), classify each.
    let statements: Vec<String> = crate::migrations::split_statements_for(&sql, engine)
        .into_iter()
        .filter(|s| crate::migrations::applied::is_effective_sql(s))
        .collect();
    if statements.is_empty() {
        return Err(AppError::Config("no executable statements in the script".into()));
    }
    let kinds: Vec<QueryKind> = statements
        .iter()
        .map(|s| classify(s, engine).map(|c| c.kind))
        .collect::<AppResult<_>>()?;

    // ── all-reads path: sequential on the read-only pool ──────────────────────
    if !script_has_write(&kinds) {
        // Reads honor `auto_run_reads`; when off they need the same explicit approval
        // a single read would (mirrors run_sql's RequireApproval for reads).
        if !settings.auto_run_reads && !approved {
            let reason = "reads require approval for this connection".to_string();
            record_run(
                &state, id, engine, &sql, QueryKind::Read, "blocked", "blocked", None, None,
                Some(reason.clone()), &origin,
            )
            .await;
            return Err(AppError::Blocked { reason });
        }

        let live = get_live(&state, id).await?;
        let mut out: Vec<ScriptStatement> = Vec::with_capacity(statements.len());
        let mut failure: Option<String> = None;
        for stmt in &statements {
            if failure.is_some() {
                out.push(stmt_skipped(stmt));
                continue;
            }
            // query_id threaded in so a long read script is cancellable per statement.
            match executor::run_read(&live, engine, stmt, settings.max_rows, query_id).await {
                Ok(result) => out.push(ScriptStatement {
                    sql: stmt.clone(),
                    result: Some(result),
                    affected: None,
                    error: None,
                }),
                Err(e) => {
                    let msg = e.to_string();
                    out.push(stmt_err(stmt, msg.clone()));
                    failure = Some(msg);
                }
            }
        }
        let total: i64 = out
            .iter()
            .filter_map(|s| s.result.as_ref())
            .map(|r| r.row_count as i64)
            .sum();
        let (status, err) = match &failure {
            Some(e) => ("error", Some(e.clone())),
            None => ("ok", None),
        };
        record_run(
            &state, id, engine, &sql, QueryKind::Read, "script:execute", status, Some(total), None,
            err, &origin,
        )
        .await;
        return Ok(ScriptOutcome { statements: out, committed: false, all_reads: true });
    }

    // ── write/DDL path: one transaction, all-or-nothing ───────────────────────
    // Same gates as run_sql: writes must be enabled AND the run explicitly approved.
    if !settings.allow_writes {
        let reason = "writing is disabled for this connection (writes are off by default). \
                      Enable writes in the connection's safety settings to run this script."
            .to_string();
        record_run(
            &state, id, engine, &sql, QueryKind::Write, "blocked", "blocked", None, None,
            Some(reason.clone()), &origin,
        )
        .await;
        return Err(AppError::Blocked { reason });
    }
    if !approved {
        let reason = "this script modifies data and requires explicit approval".to_string();
        record_run(
            &state, id, engine, &sql, QueryKind::Write, "blocked", "blocked", None, None,
            Some(reason.clone()), &origin,
        )
        .await;
        return Err(AppError::Blocked { reason });
    }

    let has_ddl = kinds.iter().any(|k| matches!(k, QueryKind::Ddl));
    let script_kind = if has_ddl {
        QueryKind::Ddl
    } else if kinds.iter().any(|k| matches!(k, QueryKind::Privilege)) {
        QueryKind::Privilege
    } else {
        QueryKind::Write
    };

    // Compliance: persist the attempt BEFORE touching the DB; fail closed if we can't.
    audit::record(
        &state.store,
        RecordArgs {
            connection_id: id,
            engine,
            agent_prompt: None,
            sql: sql.clone(),
            kind: script_kind,
            action: "script:execute:attempt".into(),
            approved_by: None,
            affected_estimate: None,
            error: None,
        },
    )
    .await
    .map_err(|e| {
        AppError::Config(format!("audit pre-record failed — refusing to run script: {e}"))
    })?;

    let live = get_live(&state, id).await?;
    // Wrap the whole transaction in the cancel/timeout guard; a cancel drops the tx
    // future mid-flight (uncommitted → rolled back), same as the single-write path.
    let tx_fut = async { Ok::<_, AppError>(execute_script_tx(&live.write_pool, &statements).await) };
    let (rows, committed) = match executor::cancel::guard(query_id, executor::cancel::QUERY_TIMEOUT, tx_fut).await {
        Ok(v) => v,
        Err(e) => {
            record_run(
                &state, id, engine, &sql, script_kind, "script:execute", "error", None, None,
                Some(e.to_string()), &origin,
            )
            .await;
            return Err(e);
        }
    };

    // A committed DDL changed the catalog — drop the cached schema.
    if committed && has_ddl {
        let _ = state.store.clear_schema_cache(id).await;
    }
    let total: i64 = rows.iter().filter_map(|s| s.affected).sum();
    let first_err = rows.iter().find_map(|s| s.error.clone());
    record_run(
        &state, id, engine, &sql, script_kind, "script:execute",
        if committed { "ok" } else { "error" }, Some(total), None, first_err, &origin,
    )
    .await;

    Ok(ScriptOutcome { statements: rows, committed, all_reads: false })
}

// ── safety settings ──────────────────────────────────────────────────────────

#[tauri::command]
pub async fn get_safety(state: State<'_, AppState>, id: Uuid) -> AppResult<SafetySettings> {
    state.store.get_safety(id).await
}

#[tauri::command]
pub async fn set_safety(
    state: State<'_, AppState>,
    id: Uuid,
    settings: SafetySettings,
) -> AppResult<()> {
    // Clamp the row caps before persisting (defense-in-depth alongside the frontend
    // clamp). max_rows is u64 so a negative UI value already wraps astronomically
    // large; exec_preview_row_limit is i64 and a negative wraps to an infinite read
    // cap once cast to usize downstream. Bound both to sane ranges. Mirrors the
    // .min(...) the MCP read path applies.
    let mut settings = settings;
    settings.max_rows = settings.max_rows.clamp(1, 100_000);
    settings.exec_preview_row_limit = settings.exec_preview_row_limit.clamp(0, 1_000_000);
    state.store.set_safety(id, &settings).await
}

// ── logs ─────────────────────────────────────────────────────────────────────

#[tauri::command]
pub async fn list_audit(state: State<'_, AppState>, id: Uuid) -> AppResult<Vec<AuditEntry>> {
    audit::list(&state.store, id).await
}

/// Verify the hash-chain for a connection's audit log. Returns `{ ok, firstBadIndex }`
/// where `firstBadIndex` is the insertion-order position of the first tampered row.
#[tauri::command]
pub async fn audit_verify(
    state: State<'_, AppState>,
    connection_id: Uuid,
) -> AppResult<serde_json::Value> {
    let (ok, first_bad_index) = audit::verify_chain(&state.store, connection_id).await?;
    Ok(serde_json::json!({ "ok": ok, "firstBadIndex": first_bad_index }))
}

#[tauri::command]
pub async fn list_history(state: State<'_, AppState>, id: Uuid) -> AppResult<Vec<HistoryEntry>> {
    state.store.list_history(id).await
}

// ── MCP server status ─────────────────────────────────────────────────────────

/// Port / URL / bearer token for the local MCP server, so the UI can render the
/// per-platform connection snippets.
#[tauri::command]
pub fn mcp_status(state: State<'_, AppState>) -> serde_json::Value {
    serde_json::json!({
        "port": crate::mcp::MCP_PORT,
        "url": crate::mcp::mcp_url(),
        "token": state.mcp_token,
        "bridgePort": crate::mcp::MCP_BRIDGE_PORT,
        "bridgePath": crate::mcp::bridge_binary_path(),
    })
}

/// Detect which AI platforms are installed (for one-click connect buttons).
#[tauri::command]
pub fn mcp_platforms() -> Vec<crate::mcp::connect::PlatformInfo> {
    crate::mcp::connect::detect()
}

/// One-click connect: write/merge the MCP config for the given platform so the user
/// doesn't hand-edit JSON/TOML. Their local token is filled in automatically.
#[tauri::command]
pub fn connect_platform(state: State<'_, AppState>, platform: String) -> AppResult<String> {
    crate::mcp::connect::connect(
        &platform,
        &state.mcp_token,
        &crate::mcp::mcp_url(),
        &crate::mcp::bridge_binary_path(),
    )
    .map_err(AppError::Config)
}

/// One-click disconnect: remove the dopedb entry from the platform's MCP config.
#[tauri::command]
pub fn disconnect_platform(platform: String) -> AppResult<String> {
    crate::mcp::connect::disconnect(&platform).map_err(AppError::Config)
}

/// Open a supported local AI app after the frontend has copied a SQL prompt.
#[tauri::command]
pub fn open_agent_app(platform: String) -> AppResult<String> {
    crate::mcp::connect::open_app(&platform).map_err(AppError::Config)
}

// ── native pickers ────────────────────────────────────────────────────────────
// Rust-side dialog plugin (no webview capability wiring needed). The blocking_*
// calls dispatch to the main thread; async commands keep them off the IPC pool.

/// Native folder picker (project folder / migrations dir). None = user cancelled.
#[tauri::command]
pub async fn pick_folder(app: tauri::AppHandle) -> Option<String> {
    use tauri_plugin_dialog::DialogExt;
    app.dialog()
        .file()
        .blocking_pick_folder()
        .and_then(|p| p.into_path().ok())
        .map(|p| p.to_string_lossy().into_owned())
}

/// Native file picker (SQLite database path). None = user cancelled.
#[tauri::command]
pub async fn pick_file(app: tauri::AppHandle) -> Option<String> {
    use tauri_plugin_dialog::DialogExt;
    app.dialog()
        .file()
        .blocking_pick_file()
        .and_then(|p| p.into_path().ok())
        .map(|p| p.to_string_lossy().into_owned())
}

// ── migration change-log ──────────────────────────────────────────────────────

/// Scan a folder of `.sql` migrations, replay them into a schema model (change log +
/// generated down SQL), and — if a connection is given — diff the result against the
/// live DB schema.
#[tauri::command]
pub async fn analyze_migrations(
    state: State<'_, AppState>,
    dir: String,
    connection_id: Option<Uuid>,
) -> AppResult<crate::migrations::MigrationReport> {
    const LIVE_COMPARE_TIMEOUT: Duration = Duration::from_secs(2);
    const LOCAL_ANALYZE_TIMEOUT: Duration = Duration::from_secs(6);

    let (catalog, tracker) = match connection_id {
        Some(id) => {
            // Keep the local migration report responsive even when the live DB is
            // slow or unreachable. Drift/tracker data is useful, but best-effort.
            let catalog = match tokio::time::timeout(LIVE_COMPARE_TIMEOUT, load_catalog(&state, id)).await {
                Ok(Ok(catalog)) => Some(catalog),
                Ok(Err(_)) | Err(_) => None,
            };
            // Probe the read-only pool for the ORM's applied-state table (best-effort).
            let tracker = match tokio::time::timeout(LIVE_COMPARE_TIMEOUT, get_live(&state, id)).await {
                Ok(Ok(live)) => tokio::time::timeout(
                    LIVE_COMPARE_TIMEOUT,
                    crate::migrations::applied::detect(live.ro()),
                )
                .await
                .ok()
                .flatten(),
                Ok(Err(_)) | Err(_) => None,
            };
            (catalog, tracker)
        }
        None => (None, None),
    };
    let analysis_dir = dir.clone();
    match tokio::time::timeout(
        LOCAL_ANALYZE_TIMEOUT,
        tokio::task::spawn_blocking(move || {
            crate::migrations::analyze(&analysis_dir, catalog.as_ref(), tracker.as_ref())
        }),
    )
    .await
    {
        Ok(Ok(report)) => Ok(report),
        Ok(Err(e)) => Err(AppError::Config(format!("migration analysis failed: {e}"))),
        Err(_) => Ok(crate::migrations::MigrationReport {
            dir,
            migrations: vec![],
            drift: None,
            error: Some(
                "Migration analysis timed out. Choose the actual migrations folder instead of a broad project folder."
                    .into(),
            ),
            tracker: None,
            tracker_table: None,
        }),
    }
}

/// Actually run a migration: re-analyze the folder fresh against the live tracker, pick
/// the apply/rollback script for `version`, and execute it in ONE transaction on the
/// write pool. Requires `approved` AND the connection's `allow_writes` (same gate as
/// `run_sql`), and enforces order — rollback only the latest applied migration, apply
/// only the earliest pending one. Audits the attempt + outcome and clears the schema
/// cache on success.
#[tauri::command]
pub async fn run_migration_script(
    state: State<'_, AppState>,
    connection_id: Uuid,
    dir: String,
    version: String,
    direction: String,
    approved: bool,
    // Optional so existing invokes keep working; wires the executor cancel slot and
    // (even when None) the wall-clock timeout, matching run_sql/run_script.
    query_id: Option<Uuid>,
) -> AppResult<ExecOutcome> {
    use crate::migrations::applied::{self, Direction};

    let dir_kind = match direction.as_str() {
        "apply" => Direction::Apply,
        "rollback" => Direction::Rollback,
        other => {
            return Err(AppError::Config(format!(
                "unknown direction {other:?} — use \"apply\" or \"rollback\""
            )))
        }
    };

    let profile = state.store.get_connection(connection_id).await?;
    let settings = state.store.get_safety(connection_id).await?;
    let engine = profile.engine;

    // Gate (mirrors run_sql): explicit approval AND writes enabled, or it never runs.
    if !approved {
        return Err(AppError::Blocked {
            reason: "this migration modifies the database and requires explicit approval".into(),
        });
    }
    if !settings.allow_writes {
        return Err(AppError::Blocked {
            reason: "writes are disabled for this connection (allow_writes = 0)".into(),
        });
    }

    // Re-analyze FRESH against the live tracker — never trust a stale client copy.
    let live = get_live(&state, connection_id).await?;
    let tracker = applied::detect(live.ro()).await;
    let report = crate::migrations::analyze(&dir, None, tracker.as_ref());
    if let Some(err) = report.error {
        return Err(AppError::NotFound(err));
    }

    let target = applied::pick_target(&report.migrations, &version, dir_kind)?;
    let script = match dir_kind {
        Direction::Apply => target.apply_script.clone(),
        Direction::Rollback => target.rollback_script.clone(),
    }
    .unwrap_or_default();

    // Split into executable statements, skipping pure-comment (`-- MANUAL:`) ones.
    let statements: Vec<String> = crate::migrations::split_statements_for(&script, engine)
        .into_iter()
        .filter(|s| applied::is_effective_sql(s))
        .collect();

    let (action_attempt, action_done) = match dir_kind {
        Direction::Apply => ("migration:apply:attempt", "migration:apply"),
        Direction::Rollback => ("migration:rollback:attempt", "migration:rollback"),
    };

    // Compliance: persist the attempt BEFORE touching the DB; fail closed if we can't.
    audit::record(
        &state.store,
        RecordArgs {
            connection_id,
            engine,
            agent_prompt: None,
            sql: script.clone(),
            kind: QueryKind::Ddl,
            action: action_attempt.into(),
            approved_by: None,
            affected_estimate: None,
            error: None,
        },
    )
    .await
    .map_err(|e| {
        AppError::Config(format!("audit pre-record failed — refusing to run migration: {e}"))
    })?;

    // Wrap in the same cancel/timeout guard as run_sql/run_script: a slow backfill or
    // CREATE INDEX must not pin the write-pool connection forever with no way to abort.
    // A cancel/timeout drops the tx future mid-flight (uncommitted → rolled back).
    let tx_fut = applied::run_in_tx(&live.write_pool, &statements);
    match executor::cancel::guard(query_id, executor::cancel::QUERY_TIMEOUT, tx_fut).await {
        Ok(affected) => {
            // Structure changed — drop the cached catalog so the sidebar/agent refresh.
            let _ = state.store.clear_schema_cache(connection_id).await;
            record_run(
                &state, connection_id, engine, &script, QueryKind::Ddl, action_done, "ok",
                Some(affected as i64), None, None, "migration",
            )
            .await;
            Ok(ExecOutcome { result: None, affected: Some(affected), committed: true })
        }
        Err(e) => {
            record_run(
                &state, connection_id, engine, &script, QueryKind::Ddl, action_done, "error",
                None, None, Some(e.to_string()), "migration",
            )
            .await;
            Err(e)
        }
    }
}

/// Auto-detect the migrations folder inside a project root (Prisma/Drizzle/Rails/… layouts).
#[tauri::command]
pub fn detect_migrations_dir(project_dir: String) -> Option<String> {
    crate::migrations::detect_dir(&project_dir)
}

/// Watch the migrations folder; emit `migrations:changed` on any change so the UI can
/// re-analyze live. Replaces any previous watcher.
#[tauri::command]
pub fn start_migration_watch(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    dir: String,
) -> AppResult<()> {
    use notify::{RecursiveMode, Watcher};
    use tauri::Emitter;

    let handle = app.clone();
    let mut watcher = notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
        if res.is_ok() {
            let _ = handle.emit("migrations:changed", ());
        }
    })
    .map_err(|e| AppError::Config(format!("migration watch init failed: {e}")))?;
    watcher
        .watch(std::path::Path::new(&dir), RecursiveMode::Recursive)
        .map_err(|e| AppError::Config(format!("migration watch failed: {e}")))?;
    *state.mig_watcher.lock().unwrap() = Some(watcher);
    Ok(())
}

#[cfg(test)]
mod script_tests {
    use super::*;
    use crate::model::QueryKind;
    use sqlx::sqlite::{SqliteConnectOptions, SqlitePool, SqlitePoolOptions};

    async fn sqlite(tag: &str) -> SqlitePool {
        let path =
            std::env::temp_dir().join(format!("dopedb-script-{tag}-{}.db", std::process::id()));
        let _ = std::fs::remove_file(&path);
        let opts = SqliteConnectOptions::new().filename(&path).create_if_missing(true);
        SqlitePoolOptions::new().max_connections(1).connect_with(opts).await.unwrap()
    }

    // Gating decision: pure reads take the read path; any write/DDL/privilege forces
    // the (approval + allow_writes) write path.
    #[test]
    fn write_path_only_when_a_statement_writes() {
        assert!(!script_has_write(&[QueryKind::Read, QueryKind::Read]));
        assert!(script_has_write(&[QueryKind::Read, QueryKind::Write]));
        assert!(script_has_write(&[QueryKind::Ddl]));
        assert!(script_has_write(&[QueryKind::Privilege]));
    }

    // One transaction: a mid-script failure rolls back everything before it, marks the
    // failing + trailing statements, and commits nothing.
    #[tokio::test]
    async fn tx_rolls_back_the_whole_script_on_error() {
        let pool = sqlite("rollback").await;
        sqlx::raw_sql("CREATE TABLE t (id INTEGER);").execute(&pool).await.unwrap();
        let db = DbPool::Sqlite(pool.clone());

        let (rows, committed) = execute_script_tx(
            &db,
            &[
                "INSERT INTO t VALUES (1)".into(),
                "INSERT INTO t VALUES (2)".into(),
                "THIS IS NOT SQL".into(),
                "INSERT INTO t VALUES (3)".into(),
            ],
        )
        .await;

        assert!(!committed, "a failed statement must not commit");
        assert!(rows[0].error.is_none() && rows[1].error.is_none());
        assert!(rows[2].error.is_some(), "the bad statement carries the error");
        assert!(rows[3].error.is_some(), "statements after the failure are skipped");

        let n: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM t").fetch_one(&pool).await.unwrap();
        assert_eq!(n, 0, "rollback leaves the table empty");
    }

    // All statements succeed → one commit, every row persisted.
    #[tokio::test]
    async fn tx_commits_all_on_success() {
        let pool = sqlite("commit").await;
        let db = DbPool::Sqlite(pool.clone());

        let (rows, committed) = execute_script_tx(
            &db,
            &[
                "CREATE TABLE t (id INTEGER)".into(),
                "INSERT INTO t VALUES (1)".into(),
                "INSERT INTO t VALUES (2)".into(),
            ],
        )
        .await;

        assert!(committed);
        assert!(rows.iter().all(|r| r.error.is_none()));
        let n: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM t").fetch_one(&pool).await.unwrap();
        assert_eq!(n, 2);
    }
}
