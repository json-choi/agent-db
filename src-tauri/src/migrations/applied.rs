//! Applied-state tracking + executable apply/rollback assembly. Turns the passive
//! change-log into something that can actually manage migration state:
//!   1. [`detect`] probes the read-only pool for the ORM's bookkeeping table and reads
//!      the applied identifiers (plain SELECTs — never a write),
//!   2. [`build_scripts`] pairs each migration's up/down SQL with the tracking-table
//!      statement that marks / un-marks it, per convention,
//!   3. [`pick_target`] + [`run_in_tx`] enforce order and execute one script atomically.
//!
//! Most conventions get a real tracking statement (prisma/rails inserts, sqlx's
//! sha384-checksummed row, golang-migrate's single high-water row, flyway's rank-derived
//! row). Only drizzle stays `-- MANUAL:` — its applied-state keys on a journal timestamp
//! we can't derive from the .sql files, and guessing it would make drizzle skip pending
//! migrations. A comment-only statement is skipped by the runner, so the schema change
//! still applies and only that one tracking row is left to the user.

use sha2::{Digest, Sha256, Sha384};
use uuid::Uuid;

use crate::connection::DbPool;
use crate::error::{AppError, AppResult};
use crate::executor::read::{mysql_value, pg_value, sqlite_value};
use crate::model::Engine;

use super::MigrationView;

// ── tracker detection ──────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrackerKind {
    Prisma,
    Sqlx,
    Rails,
    GolangMigrate,
    Flyway,
    Drizzle,
}

impl TrackerKind {
    pub fn as_str(self) -> &'static str {
        match self {
            TrackerKind::Prisma => "prisma",
            TrackerKind::Sqlx => "sqlx",
            TrackerKind::Rails => "rails",
            TrackerKind::GolangMigrate => "golang-migrate",
            TrackerKind::Flyway => "flyway",
            TrackerKind::Drizzle => "drizzle",
        }
    }
}

/// A detected tracker: its convention, the table name, and the applied identifiers
/// read straight from that table (prisma → migration_name, sqlx/rails/flyway →
/// version, golang-migrate → the single high-water version, drizzle → hashes).
pub struct Applied {
    pub kind: TrackerKind,
    pub table: String,
    pub ids: Vec<String>,
    /// Target engine — [`build_scripts`] needs it to emit engine-correct tracking SQL
    /// (e.g. the sqlx checksum BLOB literal differs on Postgres vs MySQL/SQLite).
    pub engine: Engine,
}

fn engine_of(pool: &DbPool) -> Engine {
    match pool {
        DbPool::Postgres(_) => Engine::Postgres,
        DbPool::Mysql(_) => Engine::Mysql,
        DbPool::Sqlite(_) => Engine::Sqlite,
    }
}

/// Probe the read-only pool for a known tracking table, in ORM-prevalence order. A
/// missing table just errors the probe and we fall through; the first one that reads
/// wins. Returns `None` when no convention is found.
// ponytail: probe-by-SELECT (try to read, catch the error) rather than per-engine
// information_schema lookups — one code path across pg/mysql/sqlite. Up to 6 failed
// SELECTs when nothing is found; fine for a one-shot analyze.
pub async fn detect(pool: &DbPool) -> Option<Applied> {
    let engine = engine_of(pool);
    if let Ok(ids) = fetch_col0(pool, "SELECT migration_name FROM _prisma_migrations").await {
        return Some(Applied { kind: TrackerKind::Prisma, table: "_prisma_migrations".into(), ids, engine });
    }
    if let Ok(ids) = fetch_col0(pool, "SELECT version FROM _sqlx_migrations").await {
        return Some(Applied { kind: TrackerKind::Sqlx, table: "_sqlx_migrations".into(), ids, engine });
    }
    // schema_migrations is shared: golang-migrate has a `dirty` column (single
    // high-water row), rails does not (one row per version).
    if fetch_col0(pool, "SELECT dirty FROM schema_migrations LIMIT 1").await.is_ok() {
        let ids = fetch_col0(pool, "SELECT version FROM schema_migrations").await.unwrap_or_default();
        return Some(Applied { kind: TrackerKind::GolangMigrate, table: "schema_migrations".into(), ids, engine });
    }
    if let Ok(ids) = fetch_col0(pool, "SELECT version FROM schema_migrations").await {
        return Some(Applied { kind: TrackerKind::Rails, table: "schema_migrations".into(), ids, engine });
    }
    if let Ok(ids) = fetch_col0(pool, "SELECT version FROM flyway_schema_history").await {
        return Some(Applied { kind: TrackerKind::Flyway, table: "flyway_schema_history".into(), ids, engine });
    }
    if let Ok(ids) = fetch_col0(pool, "SELECT hash FROM __drizzle_migrations").await {
        return Some(Applied { kind: TrackerKind::Drizzle, table: "__drizzle_migrations".into(), ids, engine });
    }
    None
}

/// Read column 0 of every row as a string, reusing the executor's per-engine cell
/// decoders so a bigint version and a text migration_name both come back as `String`.
async fn fetch_col0(pool: &DbPool, sql: &str) -> Result<Vec<String>, sqlx::Error> {
    Ok(match pool {
        DbPool::Postgres(p) => sqlx::query(sql).fetch_all(p).await?.iter().filter_map(|r| stringify(pg_value(r, 0))).collect(),
        DbPool::Mysql(p) => sqlx::query(sql).fetch_all(p).await?.iter().filter_map(|r| stringify(mysql_value(r, 0))).collect(),
        DbPool::Sqlite(p) => sqlx::query(sql).fetch_all(p).await?.iter().filter_map(|r| stringify(sqlite_value(r, 0))).collect(),
    })
}

fn stringify(v: serde_json::Value) -> Option<String> {
    match v {
        serde_json::Value::Null => None,
        serde_json::Value::String(s) => Some(s),
        other => Some(other.to_string()),
    }
}

/// Best-effort: is this migration recorded as applied by the detected tracker?
pub fn is_applied(a: &Applied, version: &str, name: &str, index: usize) -> bool {
    match a.kind {
        TrackerKind::Prisma => a.ids.iter().any(|id| id == name),
        TrackerKind::Sqlx => a.ids.iter().any(|id| id == version),
        TrackerKind::Rails | TrackerKind::Flyway => {
            a.ids.iter().any(|id| id == version || id.starts_with(&format!("{version}.")))
        }
        // Single high-water row: everything at or below the tracked version is applied.
        TrackerKind::GolangMigrate => a
            .ids
            .first()
            .and_then(|hw| hw.parse::<i64>().ok())
            .zip(version.parse::<i64>().ok())
            .map(|(hw, v)| v <= hw)
            .unwrap_or_else(|| a.ids.iter().any(|id| id == version)),
        // No stable name→hash mapping; rows are in application order, so the first
        // N scanned migrations correspond to the N recorded rows.
        // ponytail: count-based; exact hash matching would need drizzle's own hasher.
        TrackerKind::Drizzle => index < a.ids.len(),
    }
}

// ── apply / rollback script assembly ─────────────────────────────────────────────

fn sha256_hex(b: &[u8]) -> String {
    hex::encode(Sha256::digest(b))
}
fn sha384_hex(b: &[u8]) -> String {
    hex::encode(Sha384::digest(b))
}

/// Escape a single-quoted SQL string literal.
fn esc(s: &str) -> String {
    s.replace('\'', "''")
}

/// A version safe to interpolate into a NUMERIC tracking column (sqlx/golang store the
/// version unquoted): non-empty, all ASCII digits. Versions come from the migration
/// filename's leading token (mod.rs `ident_of`), which is otherwise unconstrained — so
/// without this a crafted filename like `1 OR 1=1_x.sql` or `1; DROP TABLE t;_x.sql`
/// would inject SQL into the (user-approved) tracking statement.
fn num_version(v: &str) -> Option<&str> {
    (!v.is_empty() && v.bytes().all(|b| b.is_ascii_digit())).then_some(v)
}

/// A MANUAL note emitted in place of a tracking statement whose version can't be safely
/// interpolated (non-numeric where the column is numeric).
fn manual_bad_version(kind: &str, version: &str) -> String {
    format!(
        "-- MANUAL: {kind} version {version:?} is not a plain integer; refusing to \
         interpolate it into a numeric tracking column. Record the tracking row by hand."
    )
}

/// sqlx stores its migration checksum as a BLOB; the literal syntax is engine-specific.
fn sqlx_checksum_literal(engine: Engine, hex: &str) -> String {
    match engine {
        Engine::Postgres => format!("decode('{hex}', 'hex')"),
        Engine::Mysql | Engine::Sqlite => format!("X'{hex}'"),
    }
}

/// Flyway's description as it parses it from the filename: the text after `V<ver>__`,
/// with underscores rendered as spaces. Matching it lets `flyway validate` pass.
fn flyway_description(name: &str) -> String {
    match name.split_once("__") {
        Some((_, desc)) => desc.replace('_', " "),
        None => name.to_string(),
    }
}

/// Build `(apply_script, rollback_script)` for one migration. The apply script is the
/// up SQL followed by the tracking INSERT; the rollback is the down SQL followed by the
/// tracking un-mark. `tracker` `None` (no bookkeeping table) leaves only the SQL plus a
/// note. `prev_version` is the version immediately before this one (golang-migrate needs
/// it to reset its single high-water row).
pub fn build_scripts(
    tracker: Option<&Applied>,
    version: &str,
    name: &str,
    up_sql: &str,
    down_sql: &str,
    prev_version: Option<&str>,
) -> (String, String) {
    let engine = tracker.map(|t| t.engine);
    let apply_track = match tracker.map(|t| t.kind) {
        None => "-- No migration tracking table found; up SQL applied without recording a tracking row.".to_string(),
        Some(TrackerKind::Prisma) => format!(
            "INSERT INTO _prisma_migrations (id, checksum, migration_name, started_at, finished_at, applied_steps_count)\n\
             VALUES ('{id}', '{ck}', '{name}', CURRENT_TIMESTAMP, CURRENT_TIMESTAMP, 1);",
            id = Uuid::new_v4().simple(),
            // ASSUMPTION: prisma's checksum = hex(sha256(migration.sql bytes)), stored as text.
            ck = sha256_hex(up_sql.as_bytes()),
            name = esc(name),
        ),
        // sqlx: real INSERT. version is a BIGINT PK (numeric-guarded); checksum is the
        // sha384 of the up-migration bytes — exactly what `sqlx migrate run` recomputes,
        // so the real CLI accepts this row later. BLOB literal is engine-specific.
        Some(TrackerKind::Sqlx) => match (num_version(version), engine) {
            (Some(ver), Some(eng)) => format!(
                "INSERT INTO _sqlx_migrations (version, description, success, checksum, execution_time)\n\
                 VALUES ({ver}, '{desc}', TRUE, {ck}, 0);",
                desc = esc(name),
                ck = sqlx_checksum_literal(eng, &sha384_hex(up_sql.as_bytes())),
            ),
            _ => manual_bad_version("sqlx", version),
        },
        Some(TrackerKind::Rails) => {
            format!("INSERT INTO schema_migrations (version) VALUES ('{}');", esc(version))
        }
        // golang-migrate keeps ONE high-water row. DELETE + INSERT works whether the
        // table is empty (fresh DB, or after this tool rolled back #1) or already has a
        // row — the earlier UPDATE-only form recorded NOTHING on an empty table.
        Some(TrackerKind::GolangMigrate) => match num_version(version) {
            Some(ver) => format!(
                "-- golang-migrate keeps ONE high-water row; reset it to this version.\n\
                 DELETE FROM schema_migrations;\n\
                 INSERT INTO schema_migrations (version, dirty) VALUES ({ver}, FALSE);"
            ),
            None => manual_bad_version("golang-migrate", version),
        },
        // flyway: real INSERT. installed_rank is derived atomically via MAX()+1; the
        // proprietary CRC checksum is left NULL (flyway treats a NULL applied-checksum as
        // "not recorded" and skips checksum validation, as it does for baseline rows).
        // version/description/script are matched to flyway's own filename parse so
        // `flyway validate` still passes.
        Some(TrackerKind::Flyway) => format!(
            "INSERT INTO flyway_schema_history\n\
             (installed_rank, version, description, type, script, checksum, installed_by, installed_on, execution_time, success)\n\
             SELECT COALESCE(MAX(installed_rank), 0) + 1, '{ver}', '{desc}', 'SQL', '{script}', NULL, 'agent-db', CURRENT_TIMESTAMP, 0, TRUE\n\
             FROM flyway_schema_history;",
            ver = esc(version),
            desc = esc(&flyway_description(name)),
            script = esc(&format!("{name}.sql")),
        ),
        // drizzle stays MANUAL by necessity: applied-detection keys on `created_at`, which
        // is the journal's folderMillis timestamp — not derivable from the .sql files
        // alone. A wrong created_at makes drizzle SKIP still-pending migrations, so we
        // never fabricate it (that would be worse than leaving the row to the user).
        Some(TrackerKind::Drizzle) => {
            "-- MANUAL: drizzle records a content hash + created_at (the journal's folderMillis)\n\
             -- in __drizzle_migrations. created_at is not derivable from the .sql files, and a\n\
             -- wrong value makes drizzle skip pending migrations — so insert this row by hand."
                .to_string()
        }
    };

    let rollback_untrack = match tracker.map(|t| t.kind) {
        None => "-- No migration tracking table found; only the down SQL is emitted (nothing to un-mark).".to_string(),
        Some(TrackerKind::Prisma) => {
            format!("DELETE FROM _prisma_migrations WHERE migration_name = '{}';", esc(name))
        }
        // version is numeric here (BIGINT), so guard it before interpolating unquoted.
        Some(TrackerKind::Sqlx) => match num_version(version) {
            Some(ver) => format!("DELETE FROM _sqlx_migrations WHERE version = {ver};"),
            None => manual_bad_version("sqlx", version),
        },
        Some(TrackerKind::Rails) => {
            format!("DELETE FROM schema_migrations WHERE version = '{}';", esc(version))
        }
        Some(TrackerKind::Flyway) => {
            format!("DELETE FROM flyway_schema_history WHERE version = '{}';", esc(version))
        }
        // Reset the single high-water row to the previous version. DELETE + INSERT (not a
        // bare UPDATE) so it still records a row if the table was somehow emptied; prev is
        // numeric-guarded before interpolating unquoted.
        Some(TrackerKind::GolangMigrate) => match prev_version.and_then(num_version) {
            Some(pv) => format!(
                "-- golang-migrate: reset the single high-water row to the previous version.\n\
                 DELETE FROM schema_migrations;\n\
                 INSERT INTO schema_migrations (version, dirty) VALUES ({pv}, FALSE);"
            ),
            None => "-- golang-migrate: rolling back the first migration → NilVersion; remove the high-water row.\n\
                     DELETE FROM schema_migrations;"
                .to_string(),
        },
        Some(TrackerKind::Drizzle) => {
            "-- MANUAL: remove the matching __drizzle_migrations row (hash semantics — no stable name→hash mapping)."
                .to_string()
        }
    };

    let apply = format!("{}\n\n{apply_track}", up_sql.trim_end());
    let rollback = format!("{}\n\n{rollback_untrack}", down_sql.trim_end());
    (apply, rollback)
}

// ── order enforcement + transactional execution ──────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    Apply,
    Rollback,
}

/// Locate the migration `version` targets and confirm it is the only one order allows:
/// apply → the earliest not-yet-applied migration; rollback → the latest applied one.
/// `migrations` is assumed sorted ascending (as [`super::analyze`] returns it).
pub fn pick_target<'a>(
    migrations: &'a [MigrationView],
    version: &str,
    dir: Direction,
) -> AppResult<&'a MigrationView> {
    let target = migrations
        .iter()
        .find(|m| m.version == version)
        .ok_or_else(|| AppError::NotFound(format!("migration version {version} not found")))?;
    match dir {
        Direction::Apply => {
            let earliest = migrations
                .iter()
                .find(|m| m.applied != Some(true))
                .ok_or_else(|| AppError::Blocked {
                    reason: "all migrations are already applied — nothing to apply".into(),
                })?;
            if earliest.version != version {
                return Err(AppError::Blocked {
                    reason: format!(
                        "out of order: apply the earliest pending migration ({}) before {version}",
                        earliest.version
                    ),
                });
            }
        }
        Direction::Rollback => {
            let latest = migrations
                .iter()
                .rev()
                .find(|m| m.applied == Some(true))
                .ok_or_else(|| AppError::Blocked {
                    reason: "no applied migration to roll back".into(),
                })?;
            if latest.version != version {
                return Err(AppError::Blocked {
                    reason: format!(
                        "out of order: only the latest applied migration ({}) can be rolled back, not {version}",
                        latest.version
                    ),
                });
            }
        }
    }
    Ok(target)
}

/// Strip `--` line and `/* */` block comments so a comment-only (e.g. `-- MANUAL:`)
/// statement can be recognised and skipped.
// ponytail: byte-walk, ignores string literals containing `--`; only used to test
// emptiness, and a real statement always has non-comment chars.
fn strip_comments(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = String::new();
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'-' && i + 1 < b.len() && b[i + 1] == b'-' {
            while i < b.len() && b[i] != b'\n' {
                i += 1;
            }
        } else if b[i] == b'/' && i + 1 < b.len() && b[i + 1] == b'*' {
            i += 2;
            while i + 1 < b.len() && !(b[i] == b'*' && b[i + 1] == b'/') {
                i += 1;
            }
            i = (i + 2).min(b.len());
        } else {
            out.push(b[i] as char);
            i += 1;
        }
    }
    out
}

/// True when a split statement has real SQL (not just comments / whitespace).
pub fn is_effective_sql(stmt: &str) -> bool {
    !strip_comments(stmt).trim().is_empty()
}

/// Run every statement in ONE transaction on the write pool, committing only if all
/// succeed. On any error the transaction rolls back and the returned error names which
/// statement failed.
// NOTE: MySQL auto-commits DDL (implicit commit) — atomicity is best-effort there; a
// multi-statement DDL migration can partially apply on MySQL despite the BEGIN/ROLLBACK.
pub async fn run_in_tx(pool: &DbPool, statements: &[String]) -> AppResult<u64> {
    let total = statements.len();
    let mut affected = 0u64;
    match pool {
        DbPool::Postgres(p) => {
            let mut tx = p.begin().await?;
            for (i, s) in statements.iter().enumerate() {
                match sqlx::query(s).execute(&mut *tx).await {
                    Ok(r) => affected += r.rows_affected(),
                    Err(e) => {
                        let _ = tx.rollback().await;
                        return Err(stmt_err(i, total, s, e));
                    }
                }
            }
            tx.commit().await?;
        }
        DbPool::Mysql(p) => {
            let mut tx = p.begin().await?;
            for (i, s) in statements.iter().enumerate() {
                match sqlx::query(s).execute(&mut *tx).await {
                    Ok(r) => affected += r.rows_affected(),
                    Err(e) => {
                        let _ = tx.rollback().await;
                        return Err(stmt_err(i, total, s, e));
                    }
                }
            }
            tx.commit().await?;
        }
        DbPool::Sqlite(p) => {
            let mut tx = p.begin().await?;
            for (i, s) in statements.iter().enumerate() {
                match sqlx::query(s).execute(&mut *tx).await {
                    Ok(r) => affected += r.rows_affected(),
                    Err(e) => {
                        let _ = tx.rollback().await;
                        return Err(stmt_err(i, total, s, e));
                    }
                }
            }
            tx.commit().await?;
        }
    }
    Ok(affected)
}

/// Wrap a statement failure as an `AppError::Db` (kind `"db"`, consistent with
/// `run_sql`) whose message names the failing statement.
fn stmt_err(i: usize, total: usize, sql: &str, e: sqlx::Error) -> AppError {
    let snippet: String = sql
        .lines()
        .find(|l| !l.trim().is_empty())
        .unwrap_or("")
        .trim()
        .chars()
        .take(80)
        .collect();
    AppError::Db(sqlx::Error::Protocol(format!(
        "migration aborted at statement {}/{} [{snippet}]: {e}",
        i + 1,
        total
    )))
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqlx::sqlite::{SqliteConnectOptions, SqlitePool, SqlitePoolOptions};

    async fn sqlite_file(tag: &str) -> SqlitePool {
        let path = std::env::temp_dir().join(format!("agentdb-applied-{tag}-{}.db", std::process::id()));
        let _ = std::fs::remove_file(&path);
        let opts = SqliteConnectOptions::new().filename(&path).create_if_missing(true);
        SqlitePoolOptions::new().max_connections(1).connect_with(opts).await.unwrap()
    }

    fn mv(version: &str, applied: Option<bool>) -> MigrationView {
        MigrationView {
            version: version.into(),
            name: format!("{version}_x"),
            up_file: format!("{version}.sql"),
            has_down_file: false,
            changes: vec![],
            generated_down: String::new(),
            parse_error: None,
            partial_parse: false,
            applied,
            apply_script: None,
            rollback_script: None,
        }
    }

    // Applied detection against a sqlite file with a _sqlx_migrations table.
    #[tokio::test]
    async fn detects_sqlx_tracker() {
        let pool = sqlite_file("sqlx").await;
        sqlx::raw_sql(
            "CREATE TABLE _sqlx_migrations (version BIGINT PRIMARY KEY, description TEXT, \
             installed_on TEXT, success BOOLEAN, checksum BLOB, execution_time BIGINT);\
             INSERT INTO _sqlx_migrations (version, description, success) VALUES (20230101120000, 'init', 1);",
        )
        .execute(&pool)
        .await
        .unwrap();

        let db = DbPool::Sqlite(pool);
        let a = detect(&db).await.expect("should detect a tracker");
        assert_eq!(a.kind, TrackerKind::Sqlx);
        assert_eq!(a.table, "_sqlx_migrations");
        assert!(a.ids.iter().any(|v| v == "20230101120000"), "reads the version: {:?}", a.ids);
    }

    // schema_migrations WITH a `dirty` column is golang-migrate, not rails.
    #[tokio::test]
    async fn schema_migrations_with_dirty_is_golang() {
        let pool = sqlite_file("golang").await;
        sqlx::raw_sql(
            "CREATE TABLE schema_migrations (version BIGINT PRIMARY KEY, dirty BOOLEAN);\
             INSERT INTO schema_migrations (version, dirty) VALUES (3, 0);",
        )
        .execute(&pool)
        .await
        .unwrap();
        let db = DbPool::Sqlite(pool);
        let a = detect(&db).await.unwrap();
        assert_eq!(a.kind, TrackerKind::GolangMigrate);
        // high-water 3 → versions 1,2,3 applied, 4 pending.
        assert!(is_applied(&a, "2", "2_x", 1));
        assert!(!is_applied(&a, "4", "4_x", 3));
    }

    // Rollback script must carry BOTH the down SQL and the tracking DELETE.
    #[test]
    fn rollback_script_has_down_and_untrack() {
        let a = Applied { kind: TrackerKind::Sqlx, table: "_sqlx_migrations".into(), ids: vec![], engine: Engine::Sqlite };
        let (_apply, rollback) = build_scripts(
            Some(&a),
            "20230101120000",
            "20230101120000_init",
            "CREATE TABLE users (id INT);",
            "DROP TABLE users;",
            None,
        );
        assert!(rollback.contains("DROP TABLE users"), "down SQL present: {rollback}");
        assert!(
            rollback.to_uppercase().contains("DELETE FROM _SQLX_MIGRATIONS"),
            "un-mark present: {rollback}"
        );
        assert!(rollback.contains("20230101120000"), "targets the version: {rollback}");
    }

    // Order enforcement: only the latest applied migration may be rolled back.
    #[test]
    fn rollback_rejects_non_latest() {
        let migs = vec![mv("001", Some(true)), mv("002", Some(true))];
        assert!(pick_target(&migs, "001", Direction::Rollback).is_err(), "001 is not the latest applied");
        assert!(pick_target(&migs, "002", Direction::Rollback).is_ok(), "002 is the latest applied");
    }

    // Order enforcement: only the earliest pending migration may be applied.
    #[test]
    fn apply_rejects_non_earliest() {
        let migs = vec![mv("001", Some(true)), mv("002", Some(false)), mv("003", Some(false))];
        assert!(pick_target(&migs, "003", Direction::Apply).is_err(), "003 is not the earliest pending");
        assert!(pick_target(&migs, "002", Direction::Apply).is_ok(), "002 is the earliest pending");
    }

    // End-to-end on a sqlite fixture: all statements commit together; any failure rolls
    // back the whole transaction (the command wrapper is a thin gate over this).
    #[tokio::test]
    async fn run_in_tx_commits_all_or_nothing() {
        let pool = sqlite_file("tx").await;
        sqlx::raw_sql("CREATE TABLE schema_migrations (version TEXT);")
            .execute(&pool)
            .await
            .unwrap();
        let db = DbPool::Sqlite(pool.clone());

        // Success: schema change + tracking insert commit together.
        run_in_tx(
            &db,
            &[
                "CREATE TABLE users (id INTEGER)".into(),
                "INSERT INTO schema_migrations (version) VALUES ('001')".into(),
            ],
        )
        .await
        .unwrap();
        let marked: i64 = sqlx::query_scalar("SELECT count(*) FROM schema_migrations")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(marked, 1, "tracking row committed");

        // Failure: the bad second statement rolls back the earlier CREATE.
        let r = run_in_tx(&db, &["CREATE TABLE more (id INTEGER)".into(), "THIS IS NOT SQL".into()]).await;
        assert!(r.is_err(), "invalid statement must abort");
        let exists: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM sqlite_master WHERE type='table' AND name='more'",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(exists, 0, "failed transaction must roll back the earlier CREATE");
    }

    // Comment-only statements (MANUAL notes) are skipped; real SQL runs.
    #[test]
    fn effective_sql_skips_comment_only() {
        assert!(!is_effective_sql("-- MANUAL: do a thing"));
        assert!(!is_effective_sql("/* block */\n  -- line"));
        assert!(is_effective_sql("-- lead\nINSERT INTO t VALUES (1)"));
    }

    // Split a script the way the runner does (comment-only statements dropped).
    fn effective(script: &str) -> Vec<String> {
        crate::migrations::split_statements(script)
            .into_iter()
            .filter(|s| is_effective_sql(s))
            .collect()
    }

    // ── finding 4: golang-migrate records a row even when schema_migrations is EMPTY ──
    // The old apply emitted only `UPDATE`, which affects 0 rows on an empty table (a fresh
    // DB, or after this tool rolled back #1 via `DELETE FROM schema_migrations`) — so the
    // migration stayed "pending" forever and could never advance.
    #[tokio::test]
    async fn golang_apply_records_row_on_empty_table() {
        let pool = sqlite_file("golang-apply").await;
        sqlx::raw_sql("CREATE TABLE schema_migrations (version BIGINT PRIMARY KEY, dirty BOOLEAN);")
            .execute(&pool)
            .await
            .unwrap();
        let db = DbPool::Sqlite(pool.clone());

        let tracker = detect(&db).await.expect("golang detected on empty table");
        assert_eq!(tracker.kind, TrackerKind::GolangMigrate);
        assert!(tracker.ids.is_empty(), "table starts empty");

        let (apply, _rollback) = build_scripts(
            Some(&tracker),
            "1",
            "1_init",
            "CREATE TABLE g_users (id INT);",
            "DROP TABLE g_users;",
            None,
        );
        assert!(
            apply.to_uppercase().contains("INSERT INTO SCHEMA_MIGRATIONS"),
            "apply must INSERT the high-water row, not only UPDATE: {apply}"
        );

        run_in_tx(&db, &effective(&apply)).await.unwrap();

        let after = detect(&db).await.unwrap();
        assert_eq!(after.ids, vec!["1".to_string()], "high-water row now recorded");
        assert!(is_applied(&after, "1", "1_init", 0), "migration reports applied after apply");
    }

    // ── finding 5: sqlx apply writes a real tracking row (was comment-only → dead-end) ──
    #[tokio::test]
    async fn sqlx_apply_records_tracking_row() {
        let pool = sqlite_file("sqlx-apply").await;
        sqlx::raw_sql(
            "CREATE TABLE _sqlx_migrations (version BIGINT PRIMARY KEY, description TEXT NOT NULL, \
             installed_on TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP, success BOOLEAN NOT NULL, \
             checksum BLOB NOT NULL, execution_time BIGINT NOT NULL);",
        )
        .execute(&pool)
        .await
        .unwrap();
        let db = DbPool::Sqlite(pool.clone());

        let tracker = detect(&db).await.expect("sqlx detected");
        assert!(tracker.ids.is_empty());
        let up = "CREATE TABLE s_users (id INT);";
        let (apply, _rb) = build_scripts(
            Some(&tracker),
            "20230101120000",
            "20230101120000_init",
            up,
            "DROP TABLE s_users;",
            None,
        );
        assert!(
            apply.to_uppercase().contains("INSERT INTO _SQLX_MIGRATIONS"),
            "sqlx apply must emit a real INSERT, not a MANUAL note: {apply}"
        );

        run_in_tx(&db, &effective(&apply)).await.unwrap();

        let after = detect(&db).await.unwrap();
        assert!(
            after.ids.iter().any(|v| v == "20230101120000"),
            "version recorded so the flow can advance past #1: {:?}",
            after.ids
        );
        assert!(is_applied(&after, "20230101120000", "20230101120000_init", 0));
        // The stored BLOB is sha384(up bytes) — exactly what `sqlx migrate run` recomputes,
        // so the real sqlx CLI accepts this row rather than flagging a checksum mismatch.
        let ck: Vec<u8> = sqlx::query_scalar("SELECT checksum FROM _sqlx_migrations")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(hex::encode(&ck), sha384_hex(up.as_bytes()), "checksum = sqlx's sha384(up)");
    }

    // ── finding 8: a crafted (non-numeric) version can't inject SQL into the numeric ──
    // sqlx/golang tracking statements. The guard degrades them to MANUAL comments, which
    // the runner drops — so no injected predicate or extra statement ever executes.
    #[test]
    fn crafted_version_is_not_injected_into_numeric_trackers() {
        let evil = "1 OR 1=1; DROP TABLE pwned; --";
        for kind in [TrackerKind::Sqlx, TrackerKind::GolangMigrate] {
            let t = Applied { kind, table: "t".into(), ids: vec![], engine: Engine::Sqlite };
            let (apply, rollback) = build_scripts(
                Some(&t),
                evil,
                "x",
                "CREATE TABLE keep (id INT);",
                "DROP TABLE keep;",
                Some(evil),
            );
            for script in [&apply, &rollback] {
                for s in effective(script) {
                    let up = s.to_uppercase();
                    assert!(!up.contains("OR 1=1"), "injected predicate leaked into SQL: {s}");
                    assert!(!up.contains("PWNED"), "injected second statement leaked: {s}");
                }
            }
        }
    }
}
