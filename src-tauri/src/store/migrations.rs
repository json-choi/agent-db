//! DDL for the local app.db. Idempotent
//! `CREATE TABLE IF NOT EXISTS` so `Store::open` can run it on every start.
//! Secrets never live here — connections hold only a `secret_ref` (credential-store id).

/// Stable id for the offline-first Personal Workspace created during migration.
/// A deterministic value lets fresh installs, upgrades, and restored backups converge
/// without changing any pre-existing resource UUIDs.
pub const PERSONAL_WORKSPACE_ID: &str = "00000000-0000-0000-0000-000000000001";

/// All migrations as one script; executed via `sqlx::raw_sql` (multi-statement).
pub const SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS workspaces (
    id              TEXT PRIMARY KEY,
    name            TEXT NOT NULL,
    kind            TEXT NOT NULL,       -- personal|team
    lifecycle_state TEXT NOT NULL,       -- active|archived|deleted
    created_at      TEXT NOT NULL,
    updated_at      TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS workspace_members (
    id           TEXT PRIMARY KEY,
    workspace_id TEXT NOT NULL REFERENCES workspaces(id),
    user_id      TEXT,                   -- NULL for the offline local owner
    display_name TEXT NOT NULL,
    role         TEXT NOT NULL,
    status       TEXT NOT NULL,
    joined_at    TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_workspace_members_workspace
    ON workspace_members(workspace_id, status);
CREATE INDEX IF NOT EXISTS idx_workspace_members_user_status
    ON workspace_members(user_id, status, workspace_id);
CREATE UNIQUE INDEX IF NOT EXISTS idx_workspace_members_remote_identity
    ON workspace_members(workspace_id, user_id)
    WHERE user_id IS NOT NULL;

-- Non-secret account index for the unified account/workspace switcher. Better Auth
-- Bearer tokens stay in per-account OS credential-store entries and never enter SQLite.
CREATE TABLE IF NOT EXISTS workspace_accounts (
    user_id           TEXT PRIMARY KEY,
    email             TEXT NOT NULL,
    display_name      TEXT NOT NULL,
    last_workspace_id TEXT,
    created_at        TEXT NOT NULL,
    updated_at        TEXT NOT NULL,
    last_used_at      TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_workspace_accounts_last_used
    ON workspace_accounts(last_used_at DESC);

CREATE TABLE IF NOT EXISTS app_settings (
    key   TEXT PRIMARY KEY,
    value TEXT NOT NULL
);

INSERT OR IGNORE INTO workspaces
    (id, name, kind, lifecycle_state, created_at, updated_at)
VALUES
    ('00000000-0000-0000-0000-000000000001', 'Personal Workspace', 'personal', 'active',
     CURRENT_TIMESTAMP, CURRENT_TIMESTAMP);
INSERT OR IGNORE INTO workspace_members
    (id, workspace_id, user_id, display_name, role, status, joined_at)
VALUES
    ('00000000-0000-0000-0000-000000000002',
     '00000000-0000-0000-0000-000000000001', NULL, 'Local owner', 'owner', 'active',
     CURRENT_TIMESTAMP);
INSERT OR IGNORE INTO app_settings (key, value)
VALUES ('active_workspace_id', '00000000-0000-0000-0000-000000000001');

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
    workspace_id      TEXT NOT NULL DEFAULT '00000000-0000-0000-0000-000000000001'
                      REFERENCES workspaces(id),
    account_user_id   TEXT,                          -- owner of a team-local resource
    remote_id         TEXT,
    revision          INTEGER NOT NULL DEFAULT 1,
    sync_status       TEXT NOT NULL DEFAULT 'local', -- local|dirty|synced|conflict
    workspace_access  TEXT NOT NULL DEFAULT 'local', -- view|read|write|manage|local
    deleted_at        TEXT,
    created_at        TEXT NOT NULL,
    updated_at        TEXT NOT NULL
);

-- Per-account local overlay for a redacted shared connection template. The secret
-- value itself stays in the OS credential store; this table stores only its opaque
-- credential-item id, member-local fields, and the last server-verified RBAC view.
CREATE TABLE IF NOT EXISTS workspace_connection_bindings (
    connection_id  TEXT NOT NULL REFERENCES connections(id) ON DELETE CASCADE,
    account_user_id TEXT NOT NULL,
    username       TEXT NOT NULL DEFAULT '',
    extra_params   TEXT NOT NULL DEFAULT '{}',
    secret_ref     TEXT,
    workspace_access TEXT NOT NULL DEFAULT 'view',
    allow_writes   INTEGER NOT NULL DEFAULT 0,
    updated_at     TEXT NOT NULL,
    PRIMARY KEY (connection_id, account_user_id)
);
CREATE INDEX IF NOT EXISTS idx_workspace_connection_bindings_account
    ON workspace_connection_bindings(account_user_id, connection_id);

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
    account_scope TEXT NOT NULL DEFAULT 'personal', -- personal or authenticated account id
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
    workspace_id  TEXT NOT NULL DEFAULT '00000000-0000-0000-0000-000000000001'
                  REFERENCES workspaces(id),
    remote_id     TEXT,
    revision      INTEGER NOT NULL DEFAULT 1,
    sync_status   TEXT NOT NULL DEFAULT 'local',
    deleted_at    TEXT,
    updated_at    TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS dashboards (
    id                 TEXT PRIMARY KEY,
    connection_id      TEXT NOT NULL REFERENCES connections(id) ON DELETE CASCADE,
    title              TEXT NOT NULL,
    description        TEXT NOT NULL DEFAULT '',
    sql                TEXT NOT NULL,
    visualization_json TEXT NOT NULL,
    workspace_id       TEXT NOT NULL DEFAULT '00000000-0000-0000-0000-000000000001'
                       REFERENCES workspaces(id),
    remote_id          TEXT,
    revision           INTEGER NOT NULL DEFAULT 1,
    sync_status        TEXT NOT NULL DEFAULT 'local',
    deleted_at         TEXT,
    created_at         TEXT NOT NULL,
    updated_at         TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_dashboards_conn_updated
    ON dashboards(connection_id, updated_at DESC);

CREATE TABLE IF NOT EXISTS sync_outbox (
    id            TEXT PRIMARY KEY,
    workspace_id  TEXT NOT NULL REFERENCES workspaces(id),
    resource_type TEXT NOT NULL,
    resource_id   TEXT NOT NULL,
    operation     TEXT NOT NULL,       -- upsert|delete
    revision      INTEGER NOT NULL,
    payload_json  TEXT,                -- intentionally NULL until hosted sync exists
    created_at    TEXT NOT NULL,
    attempts      INTEGER NOT NULL DEFAULT 0,
    last_error    TEXT
);
CREATE INDEX IF NOT EXISTS idx_sync_outbox_workspace_created
    ON sync_outbox(workspace_id, created_at);

CREATE TABLE IF NOT EXISTS sync_state (
    workspace_id TEXT PRIMARY KEY REFERENCES workspaces(id),
    pull_cursor  TEXT,
    last_pulled_at TEXT,
    last_pushed_at TEXT
);
INSERT OR IGNORE INTO sync_state (workspace_id)
VALUES ('00000000-0000-0000-0000-000000000001');

CREATE TABLE IF NOT EXISTS schema_cache (
    connection_id   TEXT NOT NULL REFERENCES connections(id) ON DELETE CASCADE,
    account_scope   TEXT NOT NULL DEFAULT 'personal',
    introspected_at TEXT NOT NULL,
    catalog_json    TEXT NOT NULL,
    PRIMARY KEY (connection_id, account_scope)
);

-- In-app agent chat: one row per conversation thread. `cli_session_id` is the
-- underlying CLI's own resume token (Claude Code `--resume` / Codex `resume <id>`),
-- persisted here so a conversation survives across app restarts. `model`/`effort`
-- hold the values used by the most recent turn, seeding the picker on thread switch.
-- `connection_id` binds every new thread to one DopeDB connection for context
-- injection. Upgraded databases may retain NULL in historical rows; the UI excludes
-- those legacy threads. Deliberately no FK so deleting a connection does not erase
-- its conversation record.
CREATE TABLE IF NOT EXISTS agent_chat_threads (
    id             TEXT PRIMARY KEY,
    provider       TEXT NOT NULL,
    connection_id  TEXT,
    workspace_id   TEXT NOT NULL DEFAULT '00000000-0000-0000-0000-000000000001',
    account_scope  TEXT NOT NULL DEFAULT 'personal',
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
