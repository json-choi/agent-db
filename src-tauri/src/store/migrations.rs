//! DDL for the local app.db. Idempotent
//! `CREATE TABLE IF NOT EXISTS` so `Store::open` can run it on every start.
//! Secrets never live here — connections hold only a `secret_ref` (credential-store id).

/// All migrations as one script; executed via `sqlx::raw_sql` (multi-statement).
pub const SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS connections (
    id                TEXT PRIMARY KEY,
    name              TEXT NOT NULL,
    engine            TEXT NOT NULL,
    provider          TEXT NOT NULL DEFAULT 'auto', -- control-plane overlay
    driver_id         TEXT,                          -- NULL = registry recommendation
    host              TEXT NOT NULL,
    port              INTEGER NOT NULL,
    db_name           TEXT NOT NULL,
    username          TEXT NOT NULL,
    sslmode           TEXT NOT NULL,
    extra_params      TEXT NOT NULL DEFAULT '{}',   -- JSON map
    secret_ref        TEXT,                          -- credential-store item id, NOT the password
    readonly_default  INTEGER NOT NULL DEFAULT 1,
    allow_writes      INTEGER NOT NULL DEFAULT 0,
    env               TEXT,                          -- dev|staging|prod label (optional)
    schema_group      TEXT,                          -- groups dev|staging|prod siblings for schema diff
    created_at        TEXT NOT NULL,
    updated_at        TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS connection_safety (
    connection_id         TEXT PRIMARY KEY REFERENCES connections(id) ON DELETE CASCADE,
    require_approval      INTEGER NOT NULL DEFAULT 1,
    allow_writes          INTEGER NOT NULL DEFAULT 0,
    wrap_writes_in_tx     INTEGER NOT NULL DEFAULT 1,
    explain_preview       INTEGER NOT NULL DEFAULT 1,
    auto_run_reads        INTEGER NOT NULL DEFAULT 1,
    max_rows              INTEGER NOT NULL DEFAULT 1000,
    exec_preview_row_limit INTEGER NOT NULL DEFAULT 50000
);

CREATE TABLE IF NOT EXISTS query_history (
    id            TEXT PRIMARY KEY,
    connection_id TEXT NOT NULL REFERENCES connections(id) ON DELETE CASCADE,
    sql           TEXT NOT NULL,
    kind          TEXT NOT NULL,
    status        TEXT NOT NULL,           -- ok|error|blocked
    row_count     INTEGER,
    duration_ms   INTEGER,
    error         TEXT,
    executed_at   TEXT NOT NULL,
    origin        TEXT NOT NULL            -- agent|manual|dashboard|migration
);
CREATE INDEX IF NOT EXISTS idx_history_conn ON query_history(connection_id, executed_at);

-- Append-only, hash-chained compliance log. Rows are never updated or deleted;
-- `verify_chain` recomputes hashes to make post-hoc edits evident (tamper-EVIDENT,
-- not tamper-proof — anyone with write access to this file could rebuild the chain).
-- Deliberately NO foreign key: audit rows must SURVIVE connection deletion (a deleted
-- connection must not erase its compliance history). See `migrate_audit_no_cascade`.
CREATE TABLE IF NOT EXISTS audit_log (
    id                TEXT PRIMARY KEY,
    connection_id     TEXT NOT NULL,
    ts                TEXT NOT NULL,
    engine            TEXT NOT NULL,
    agent_prompt      TEXT,
    sql               TEXT NOT NULL,
    kind              TEXT NOT NULL,
    action            TEXT NOT NULL,       -- propose|approve|reject|execute|blocked
    approved_by       TEXT,
    affected_estimate INTEGER,
    error             TEXT,
    prev_hash         TEXT,
    hash              TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_audit_conn ON audit_log(connection_id, ts);

CREATE TABLE IF NOT EXISTS snippets (
    id            TEXT PRIMARY KEY,
    connection_id TEXT REFERENCES connections(id) ON DELETE CASCADE,
    title         TEXT NOT NULL,
    sql           TEXT NOT NULL,
    tags          TEXT NOT NULL DEFAULT '[]',   -- JSON array
    updated_at    TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS dashboards (
    id                 TEXT PRIMARY KEY,
    connection_id      TEXT NOT NULL REFERENCES connections(id) ON DELETE CASCADE,
    title              TEXT NOT NULL,
    description        TEXT NOT NULL DEFAULT '',
    sql                TEXT NOT NULL,
    visualization_json TEXT NOT NULL,
    created_at         TEXT NOT NULL,
    updated_at         TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_dashboards_conn_updated
    ON dashboards(connection_id, updated_at DESC);

CREATE TABLE IF NOT EXISTS schema_cache (
    connection_id   TEXT PRIMARY KEY REFERENCES connections(id) ON DELETE CASCADE,
    introspected_at TEXT NOT NULL,
    catalog_json    TEXT NOT NULL
);

-- In-app agent chat: one row per conversation thread. `cli_session_id` is the
-- underlying CLI's own resume token (Claude Code `--resume` / Codex `resume <id>`),
-- persisted here so a conversation survives across app restarts. `model`/`effort`
-- hold the values used by the most recent turn, seeding the picker on thread switch.
CREATE TABLE IF NOT EXISTS agent_chat_threads (
    id             TEXT PRIMARY KEY,
    provider       TEXT NOT NULL,
    title          TEXT NOT NULL DEFAULT '',
    cli_session_id TEXT,
    model          TEXT,
    effort         TEXT,
    created_at     TEXT NOT NULL,
    updated_at     TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_agent_chat_threads_updated ON agent_chat_threads(updated_at DESC);

CREATE TABLE IF NOT EXISTS agent_chat_messages (
    id         TEXT PRIMARY KEY,
    thread_id  TEXT NOT NULL REFERENCES agent_chat_threads(id) ON DELETE CASCADE,
    role       TEXT NOT NULL,      -- user|assistant
    text       TEXT NOT NULL,
    error      TEXT,
    created_at TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_agent_chat_messages_thread ON agent_chat_messages(thread_id, created_at);
"#;
