//! Migration change-log: scan a folder of raw `.sql` migrations, parse each with
//! sqlparser, and REPLAY them into a schema model. The replay lets us:
//!   1. show a per-migration change log (+table / +column / −column / index / …),
//!   2. GENERATE the reverse (down) SQL — including for DROP COLUMN / DROP TABLE, whose
//!      original definition we know from the pre-state (the ORM-rollback pain point),
//!   3. diff the replayed schema against the live DB (drift: pending vs out-of-band).
//!
//! Handles the raw-`.sql` tools (Prisma, Drizzle, sqlx, golang-migrate, Flyway). Files
//! whose SQL doesn't parse are salvaged statement-by-statement rather than dropped.
//!
//! The replay keeps the evolving `CREATE TABLE` AST per table (not a frozen string), so
//! renames / alter-column / add-constraint all reflect into later down-SQL correctly.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use serde::Serialize;
use sqlparser::ast::{
    AlterColumnOperation, AlterTableOperation, ColumnDef, CreateTable, Ident, ObjectName,
    ObjectType, Statement,
};
use sqlparser::dialect::GenericDialect;
use sqlparser::parser::Parser;

use crate::introspect::Catalog;

pub mod applied;
pub use applied::Applied;

const MAX_SCAN_DEPTH: usize = 12;
const MAX_SQL_FILES: usize = 3_000;

// ── serde output types (camelCase for the frontend) ──────────────────────────────
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ChangeView {
    pub kind: String,
    pub summary: String,
    pub down: Option<String>,
    pub reversible: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MigrationView {
    pub version: String,
    pub name: String,
    pub up_file: String,
    pub has_down_file: bool,
    pub changes: Vec<ChangeView>,
    /// Reverse SQL we generated (independent of any hand-written down file).
    pub generated_down: String,
    pub parse_error: Option<String>,
    /// True when the file didn't parse whole and we fell back to per-statement parsing;
    /// downstream replay (and any drift derived from it) is then approximate.
    pub partial_parse: bool,
    /// Recorded as applied by the live tracker? `None` when no connection/tracker.
    pub applied: Option<bool>,
    /// up SQL + the tracking INSERT that marks this migration applied (per convention).
    pub apply_script: Option<String>,
    /// down SQL + the tracking statement that un-marks it (per convention).
    pub rollback_script: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Drift {
    /// In the migrations but not in the live DB (likely not applied).
    pub pending_tables: Vec<String>,
    /// In the live DB but not in the migrations (out-of-band / manual changes).
    pub extra_tables: Vec<String>,
    pub column_diffs: Vec<ColumnDiff>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ColumnDiff {
    pub table: String,
    pub missing_in_db: Vec<String>,
    pub extra_in_db: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MigrationReport {
    pub dir: String,
    pub migrations: Vec<MigrationView>,
    pub drift: Option<Drift>,
    pub error: Option<String>,
    /// Detected tracker convention (e.g. "prisma", "sqlx"); `None` if none found.
    pub tracker: Option<String>,
    /// The bookkeeping table name backing the tracker (e.g. "_prisma_migrations").
    pub tracker_table: Option<String>,
}

// ── replay model ─────────────────────────────────────────────────────────────────
/// key = (schema-or-"" , table-name-key). "" means the connection default schema.
type TableKey = (String, String);

/// Keeps the live `CREATE TABLE` AST so create SQL is rendered on demand — a rename /
/// alter-column / add-constraint mutates the AST and later down-SQL sees the new state.
struct TableState {
    ct: CreateTable,
}
impl TableState {
    fn create_sql(&self) -> String {
        semi(&Statement::CreateTable(self.ct.clone()).to_string())
    }
    fn column(&self, key: &str) -> Option<&ColumnDef> {
        self.ct.columns.iter().find(|c| ident_key(&c.name) == key)
    }
    fn column_keys(&self) -> BTreeSet<String> {
        self.ct.columns.iter().map(|c| ident_key(&c.name)).collect()
    }
}

struct IndexState {
    create_sql: String,
    table: TableKey,
}

#[derive(Default)]
struct Model {
    tables: BTreeMap<TableKey, TableState>,
    indexes: BTreeMap<String, IndexState>, // key = index name key
    /// The connection default schema (public for PG, None for sqlite/single-schema MySQL).
    default_schema: Option<String>,
}

/// Identifier key: quoted idents keep exact case, unquoted fold to lowercase (PG/ANSI).
fn ident_key(id: &Ident) -> String {
    if id.quote_style.is_some() {
        id.value.clone()
    } else {
        id.value.to_lowercase()
    }
}

/// (schema, name) key for a table, defaulting unqualified names to the connection schema.
fn table_key(name: &ObjectName, default_schema: &Option<String>) -> TableKey {
    let parts = &name.0;
    let name_key = parts.last().map(ident_key).unwrap_or_default();
    let schema = if parts.len() >= 2 {
        ident_key(&parts[parts.len() - 2])
    } else {
        default_schema.clone().unwrap_or_default()
    };
    (schema, name_key)
}

fn index_key(name: &ObjectName) -> String {
    name.0.last().map(ident_key).unwrap_or_default()
}

fn display_key(k: &TableKey) -> String {
    if k.0.is_empty() {
        k.1.clone()
    } else {
        format!("{}.{}", k.0, k.1)
    }
}

fn semi(s: &str) -> String {
    let t = s.trim().trim_end_matches(';');
    format!("{t};")
}

fn apply(model: &mut Model, stmt: &Statement) -> Vec<ChangeView> {
    let ds = model.default_schema.clone();
    match stmt {
        Statement::CreateTable(ct) => {
            let disp = ct.name.to_string();
            let ncols = ct.columns.len();
            model
                .tables
                .insert(table_key(&ct.name, &ds), TableState { ct: ct.clone() });
            vec![ChangeView {
                kind: "createTable".into(),
                summary: format!("＋ table {disp}  ({ncols} cols)"),
                down: Some(format!("DROP TABLE {disp};")),
                reversible: true,
            }]
        }
        Statement::Drop { object_type: ObjectType::Table, names, .. } => {
            let mut out = Vec::new();
            for n in names {
                let disp = n.to_string();
                let tk = table_key(n, &ds);
                let removed = model.tables.remove(&tk);
                let down = removed.map(|ts| {
                    let mut sql = ts.create_sql();
                    // Include (and purge) this table's indexes so the down recreates them.
                    let idx_keys: Vec<String> = model
                        .indexes
                        .iter()
                        .filter(|(_, v)| v.table == tk)
                        .map(|(k, _)| k.clone())
                        .collect();
                    for k in idx_keys {
                        if let Some(iv) = model.indexes.remove(&k) {
                            sql.push('\n');
                            sql.push_str(&iv.create_sql);
                        }
                    }
                    sql
                });
                let reversible = down.is_some();
                out.push(ChangeView {
                    kind: "dropTable".into(),
                    summary: format!("－ table {disp}"),
                    down,
                    reversible,
                });
            }
            out
        }
        Statement::Drop { object_type: ObjectType::Index, names, .. } => names
            .iter()
            .map(|n| {
                let disp = n.to_string();
                let down = model.indexes.remove(&index_key(n));
                let reversible = down.is_some();
                ChangeView {
                    kind: "dropIndex".into(),
                    summary: format!("－ index {disp}"),
                    down: down.map(|s| semi(&s.create_sql)),
                    reversible,
                }
            })
            .collect(),
        Statement::CreateIndex(ci) => {
            let name = ci.name.as_ref().map(|n| n.to_string()).unwrap_or_default();
            let k = ci.name.as_ref().map(index_key).unwrap_or_default();
            let tk = table_key(&ci.table_name, &ds);
            if !k.is_empty() {
                model
                    .indexes
                    .insert(k, IndexState { create_sql: semi(&stmt.to_string()), table: tk });
            }
            // ponytail: DROP INDEX is portable-ish; MySQL needs `ON <table>`, so trail it as a comment.
            let down = (!name.is_empty()).then(|| {
                format!("DROP INDEX {name}; -- MySQL: DROP INDEX {name} ON {};", ci.table_name)
            });
            let reversible = down.is_some();
            vec![ChangeView {
                kind: "createIndex".into(),
                summary: format!("＋ index {}", if name.is_empty() { "(unnamed)".into() } else { name }),
                down,
                reversible,
            }]
        }
        Statement::AlterTable { name, operations, .. } => operations
            .iter()
            .map(|op| apply_alter(model, name, op, &ds))
            .collect(),
        other => vec![ChangeView {
            kind: "other".into(),
            summary: one_line(&other.to_string()),
            down: None,
            reversible: false,
        }],
    }
}

fn apply_alter(
    model: &mut Model,
    name: &ObjectName,
    op: &AlterTableOperation,
    ds: &Option<String>,
) -> ChangeView {
    let t = name.to_string();
    let tk = table_key(name, ds);
    match op {
        AlterTableOperation::AddColumn { column_def, .. } => {
            let col = column_def.name.value.clone();
            if let Some(ts) = model.tables.get_mut(&tk) {
                ts.ct.columns.push(column_def.clone());
            }
            ChangeView {
                kind: "addColumn".into(),
                summary: format!("＋ column {t}.{col}  ({})", column_def.data_type),
                down: Some(format!("ALTER TABLE {t} DROP COLUMN {col};")),
                reversible: true,
            }
        }
        AlterTableOperation::DropColumn { column_name, .. } => {
            let col = column_name.value.clone();
            let ck = ident_key(column_name);
            let def = model.tables.get(&tk).and_then(|ts| ts.column(&ck).cloned());
            if let Some(ts) = model.tables.get_mut(&tk) {
                ts.ct.columns.retain(|c| ident_key(&c.name) != ck);
            }
            // Reverse with the CURRENT (post-alter) def, so a prior ALTER COLUMN is honored.
            let down = def.map(|d| format!("ALTER TABLE {t} ADD COLUMN {d};"));
            let reversible = down.is_some();
            ChangeView {
                kind: "dropColumn".into(),
                summary: format!("－ column {t}.{col}"),
                down,
                reversible,
            }
        }
        AlterTableOperation::RenameColumn { old_column_name, new_column_name } => {
            let (o, n) = (old_column_name.value.clone(), new_column_name.value.clone());
            let ok = ident_key(old_column_name);
            if let Some(ts) = model.tables.get_mut(&tk) {
                for c in ts.ct.columns.iter_mut() {
                    if ident_key(&c.name) == ok {
                        c.name = new_column_name.clone();
                    }
                }
            }
            ChangeView {
                kind: "renameColumn".into(),
                summary: format!("↻ column {t}.{o} → {n}"),
                down: Some(format!("ALTER TABLE {t} RENAME COLUMN {n} TO {o};")),
                reversible: true,
            }
        }
        AlterTableOperation::RenameTable { table_name } => {
            let nn = table_name.to_string();
            let new_tk = table_key(table_name, ds);
            if let Some(mut ts) = model.tables.remove(&tk) {
                ts.ct.name = table_name.clone(); // so a later DROP TABLE recreates the NEW name
                model.tables.insert(new_tk.clone(), ts);
            }
            for iv in model.indexes.values_mut() {
                if iv.table == tk {
                    iv.table = new_tk.clone();
                }
            }
            ChangeView {
                kind: "renameTable".into(),
                summary: format!("↻ table {t} → {nn}"),
                down: Some(format!("ALTER TABLE {nn} RENAME TO {t};")),
                reversible: true,
            }
        }
        AlterTableOperation::AddConstraint(c) => {
            // Fold into the AST so a later DROP TABLE reconstruction keeps the constraint.
            if let Some(ts) = model.tables.get_mut(&tk) {
                ts.ct.constraints.push(c.clone());
            }
            ChangeView {
                kind: "addConstraint".into(),
                summary: format!("＋ constraint on {t}: {}", one_line(&c.to_string())),
                down: None, // dropping a constraint needs its (dialect-specific) name; manual
                reversible: false,
            }
        }
        AlterTableOperation::AlterColumn { column_name, op } => {
            // Update the stored def so a later DROP COLUMN / DROP TABLE regenerates the ALTERED
            // type. Type change is the one that matters for reconstruction; other alter-column
            // ops (SET/DROP NOT NULL/DEFAULT) don't change the ADD COLUMN type we'd re-emit.
            // ponytail: fold only SetDataType; the rest don't affect regenerated type.
            if let AlterColumnOperation::SetDataType { data_type, .. } = op {
                let ck = ident_key(column_name);
                if let Some(ts) = model.tables.get_mut(&tk) {
                    for c in ts.ct.columns.iter_mut() {
                        if ident_key(&c.name) == ck {
                            c.data_type = data_type.clone();
                        }
                    }
                }
            }
            ChangeView {
                kind: "alterColumn".into(),
                summary: format!("~ column {t}.{}  ({})", column_name.value, one_line(&op.to_string())),
                down: None, // reverting a type/default change needs the prior state; manual
                reversible: false,
            }
        }
        other => ChangeView {
            kind: "alterOther".into(),
            summary: format!("{t}: {}", one_line(&other.to_string())),
            down: None,
            reversible: false,
        },
    }
}

fn one_line(s: &str) -> String {
    let flat = s.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.chars().count() > 120 {
        format!("{}…", flat.chars().take(120).collect::<String>())
    } else {
        flat
    }
}

/// Split a SQL file into top-level statements, respecting single/double quotes,
/// dollar-quoted strings ($$…$$ / $tag$…$tag$), and line/block comments. Postgres/SQLite
/// dialect (doubled-quote escapes only). Used both as a parse-salvage path and by the
/// script runners.
pub(crate) fn split_statements(sql: &str) -> Vec<String> {
    split_statements_impl(sql, false)
}

/// Engine-aware split. MySQL honors backslash escapes inside string literals by default
/// (`'a\'b'`), which the doubled-quote-only scan would mis-read as a closed string and
/// then split the statement mid-literal on the following `;`.
// ponytail: assumes MySQL's default sql_mode. A session with NO_BACKSLASH_ESCAPES wants
// the plain scan — thread the live sql_mode only if that ever surfaces.
pub(crate) fn split_statements_for(sql: &str, engine: crate::model::Engine) -> Vec<String> {
    split_statements_impl(sql, matches!(engine, crate::model::Engine::Mysql))
}

fn split_statements_impl(sql: &str, backslash_escapes: bool) -> Vec<String> {
    let b = sql.as_bytes();
    let n = b.len();
    let mut out = Vec::new();
    let mut start = 0usize;
    let mut i = 0usize;
    while i < n {
        match b[i] {
            q @ (b'\'' | b'"') => {
                i += 1;
                while i < n {
                    // MySQL: a backslash escapes the next byte (including a quote), so it
                    // can never terminate the literal.
                    if backslash_escapes && b[i] == b'\\' && i + 1 < n {
                        i += 2;
                        continue;
                    }
                    if b[i] == q {
                        if i + 1 < n && b[i + 1] == q {
                            i += 2; // doubled-quote escape
                            continue;
                        }
                        i += 1;
                        break;
                    }
                    i += 1;
                }
            }
            b'-' if i + 1 < n && b[i + 1] == b'-' => {
                while i < n && b[i] != b'\n' {
                    i += 1;
                }
            }
            b'/' if i + 1 < n && b[i + 1] == b'*' => {
                i += 2;
                while i + 1 < n && !(b[i] == b'*' && b[i + 1] == b'/') {
                    i += 1;
                }
                i = (i + 2).min(n);
            }
            b'$' => {
                if let Some(len) = dollar_tag_len(b, i) {
                    let tag = &b[i..i + len];
                    i += len;
                    while i < n {
                        if b[i..].starts_with(tag) {
                            i += len;
                            break;
                        }
                        i += 1;
                    }
                } else {
                    i += 1;
                }
            }
            b';' => {
                let stmt = sql[start..i].trim();
                if !stmt.is_empty() {
                    out.push(stmt.to_string());
                }
                i += 1;
                start = i;
            }
            _ => i += 1,
        }
    }
    let tail = sql[start..].trim();
    if !tail.is_empty() {
        out.push(tail.to_string());
    }
    out
}

/// If `b[i]` opens a dollar-quote tag (`$$` or `$ident$`), return its total byte length.
fn dollar_tag_len(b: &[u8], i: usize) -> Option<usize> {
    let mut j = i + 1;
    while j < b.len() && (b[j].is_ascii_alphanumeric() || b[j] == b'_') {
        j += 1;
    }
    (j < b.len() && b[j] == b'$').then_some(j - i + 1)
}

// ── scanning ─────────────────────────────────────────────────────────────────────
struct MigFile {
    version: String,
    name: String,
    up: PathBuf,
    down: Option<PathBuf>,
}

#[derive(Default)]
struct ScanState {
    truncated: bool,
}

fn is_sql_file(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .map(|e| e.eq_ignore_ascii_case("sql"))
        .unwrap_or(false)
}

fn should_skip_dir(path: &Path) -> bool {
    let Some(name) = path.file_name().and_then(|s| s.to_str()) else {
        return false;
    };
    matches!(
        name.to_ascii_lowercase().as_str(),
        ".git"
            | ".hg"
            | ".svn"
            | ".cache"
            | ".next"
            | ".nuxt"
            | ".turbo"
            | "node_modules"
            | "target"
            | "dist"
            | "build"
            | "coverage"
            | "vendor"
    )
}

fn collect_sql(dir: &Path, out: &mut Vec<PathBuf>) -> bool {
    let mut state = ScanState::default();
    collect_sql_inner(dir, out, 0, &mut state);
    state.truncated
}

fn collect_sql_inner(dir: &Path, out: &mut Vec<PathBuf>, depth: usize, state: &mut ScanState) {
    if depth > MAX_SCAN_DEPTH || out.len() >= MAX_SQL_FILES {
        state.truncated = true;
        return;
    }
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    for entry in rd.flatten() {
        if out.len() >= MAX_SQL_FILES {
            state.truncated = true;
            return;
        }
        let Ok(ft) = entry.file_type() else { continue };
        let p = entry.path();
        if ft.is_symlink() {
            continue;
        }
        if ft.is_dir() {
            if should_skip_dir(&p) {
                continue;
            }
            collect_sql_inner(&p, out, depth + 1, state);
        } else if ft.is_file() && is_sql_file(&p) {
            out.push(p);
        }
    }
}

fn has_direct_sql(dir: &Path) -> bool {
    let Ok(rd) = std::fs::read_dir(dir) else { return false };
    rd.flatten().any(|entry| {
        entry.file_type().map(|ft| ft.is_file()).unwrap_or(false) && is_sql_file(&entry.path())
    })
}

/// version + name for ordering/labelling; `is_down` marks reverse files.
fn ident_of(path: &Path) -> (String, String, bool) {
    let fname = path.file_name().and_then(|s| s.to_str()).unwrap_or("").to_string();
    let lower = fname.to_lowercase();
    // Prisma folder layout: <ts>_<name>/{migration.sql, down.sql} — identity from the folder,
    // never the file, so keys are unique per folder and bare down.sql is a down sibling only.
    if lower == "migration.sql" || lower == "down.sql" {
        let parent = path
            .parent()
            .and_then(|p| p.file_name())
            .and_then(|s| s.to_str())
            .unwrap_or("")
            .to_string();
        let version = parent.split(['_', '-']).next().unwrap_or(&parent).to_string();
        return (version, parent, lower == "down.sql");
    }
    let is_down = lower.ends_with(".down.sql");
    // Otherwise: strip .sql then a trailing .up/.down; version = leading token.
    let stem = fname.strip_suffix(".sql").unwrap_or(&fname);
    let base = stem
        .strip_suffix(".down")
        .or_else(|| stem.strip_suffix(".up"))
        .unwrap_or(stem)
        .to_string();
    let version = match base.split_once("__") {
        // Flyway "V<ver>__<desc>": the version itself may have '_'/'.' sub-parts
        // (V1_1 = 1.1). Take everything before the double underscore, strip the V prefix,
        // and normalize sub-part separators to '.' so V1_1 and V1_2 stay DISTINCT (a plain
        // first-token split collapsed both to "1").
        Some((ver, _desc)) => ver.trim_start_matches(['V', 'v']).replace('_', "."),
        None => base
            .split(|c: char| c == '_' || c == '-')
            .next()
            .unwrap_or(&base)
            .trim_start_matches(['V', 'v'])
            .to_string(),
    };
    (version, base, is_down)
}

/// Natural (numeric-aware) ordering: split into digit / non-digit runs, compare digit runs
/// numerically (by trimmed length then value, overflow-safe) so V2 < V10 and 2_ < 10_.
fn natural_cmp(a: &str, b: &str) -> std::cmp::Ordering {
    use std::cmp::Ordering::*;
    let (ta, tb) = (tokens(a), tokens(b));
    for (x, y) in ta.iter().zip(tb.iter()) {
        let ord = match (x, y) {
            (Tok::Num(x), Tok::Num(y)) => {
                let (x, y) = (x.trim_start_matches('0'), y.trim_start_matches('0'));
                x.len().cmp(&y.len()).then_with(|| x.cmp(y))
            }
            (Tok::Txt(x), Tok::Txt(y)) => x.cmp(y),
            (Tok::Num(_), Tok::Txt(_)) => Less, // numbers sort before text
            (Tok::Txt(_), Tok::Num(_)) => Greater,
        };
        if ord != Equal {
            return ord;
        }
    }
    ta.len().cmp(&tb.len())
}

enum Tok<'a> {
    Num(&'a str),
    Txt(&'a str),
}
fn tokens(s: &str) -> Vec<Tok<'_>> {
    let mut out = Vec::new();
    let mut i = 0;
    let b = s.as_bytes();
    while i < b.len() {
        let digit = b[i].is_ascii_digit();
        let start = i;
        while i < b.len() && b[i].is_ascii_digit() == digit {
            i += 1;
        }
        let run = &s[start..i];
        out.push(if digit { Tok::Num(run) } else { Tok::Txt(run) });
    }
    out
}

fn scan(dir: &Path) -> (Vec<MigFile>, bool) {
    let mut files = Vec::new();
    let truncated = collect_sql(dir, &mut files);

    // Split ups from downs, key downs by their base identity for pairing.
    let mut downs: BTreeMap<String, PathBuf> = BTreeMap::new();
    let mut ups: Vec<(String, String, PathBuf)> = Vec::new();
    for p in files {
        let (version, name, is_down) = ident_of(&p);
        if is_down {
            downs.insert(name.to_lowercase(), p);
        } else {
            ups.push((version, name, p));
        }
    }
    ups.sort_by(|a, b| natural_cmp(&a.0, &b.0).then_with(|| natural_cmp(&a.1, &b.1)));
    let migrations = ups
        .into_iter()
        .map(|(version, name, up)| {
            let down = downs.get(&name.to_lowercase()).cloned().or_else(|| {
                // Prisma "down.sql" sibling in the same folder.
                up.parent().map(|d| d.join("down.sql")).filter(|p| p.exists())
            });
            MigFile { version, name, up, down }
        })
        .collect();
    (migrations, truncated)
}

// ── public entrypoint ────────────────────────────────────────────────────────────
/// Scan + replay a migrations folder. With a `catalog` it also diffs against the live
/// schema (drift); with a `tracker` (detected via [`applied::detect`]) each view gains
/// its applied flag and the report names the tracker. Apply/rollback scripts are always
/// built — they degrade to SQL-plus-a-note when no tracker is present.
pub fn analyze(
    dir: &str,
    catalog: Option<&Catalog>,
    tracker: Option<&Applied>,
) -> MigrationReport {
    let path = Path::new(dir);
    if !path.is_dir() {
        return MigrationReport {
            dir: dir.to_string(),
            migrations: vec![],
            drift: None,
            error: Some(format!("not a folder: {dir}")),
            tracker: tracker.map(|a| a.kind.as_str().to_string()),
            tracker_table: tracker.map(|a| a.table.clone()),
        };
    }

    let dialect = GenericDialect {};
    let mut model = Model::default();
    // Default schema for unqualified migration tables: public for PG (catalog carries a schema),
    // None for sqlite / single-schema MySQL. Only affects keying when we have a catalog to diff.
    model.default_schema = catalog.and_then(|c| {
        c.tables.iter().any(|t| t.schema.is_some()).then(|| "public".to_string())
    });
    let mut migrations = Vec::new();
    let mut prev_version: Option<String> = None;

    let (scanned, truncated) = scan(path);
    if truncated {
        return MigrationReport {
            dir: dir.to_string(),
            migrations: vec![],
            drift: None,
            error: Some(format!(
                "migration scan was too broad. Choose a narrower folder (max {MAX_SQL_FILES} SQL files, depth {MAX_SCAN_DEPTH})."
            )),
            tracker: tracker.map(|a| a.kind.as_str().to_string()),
            tracker_table: tracker.map(|a| a.table.clone()),
        };
    }

    for (index, mf) in scanned.into_iter().enumerate() {
        let sql = std::fs::read_to_string(&mf.up).unwrap_or_default();
        let mut changes = Vec::new();
        let mut parse_error = None;
        let mut partial_parse = false;
        match Parser::parse_sql(&dialect, &sql) {
            Ok(stmts) => {
                for st in &stmts {
                    changes.extend(apply(&mut model, st));
                }
            }
            Err(e) => {
                // Whole-file parse failed: salvage what we can, statement by statement, so a
                // single unparseable statement (DO $$ blocks, triggers, …) doesn't drop the file.
                partial_parse = true;
                parse_error = Some(e.to_string());
                for stmt_sql in split_statements(&sql) {
                    match Parser::parse_sql(&dialect, &stmt_sql) {
                        Ok(stmts) => {
                            for st in &stmts {
                                changes.extend(apply(&mut model, st));
                            }
                        }
                        Err(_) => {
                            let first = stmt_sql.lines().find(|l| !l.trim().is_empty()).unwrap_or("");
                            changes.push(ChangeView {
                                kind: "unknown".into(),
                                summary: one_line(first),
                                down: None,
                                reversible: false,
                            });
                        }
                    }
                }
            }
        }
        // Generated down = reverse of each change, in reverse order; irreversible steps become a
        // manual placeholder so the copied script never silently omits them.
        let generated_down = changes
            .iter()
            .rev()
            .map(|c| match &c.down {
                Some(d) => d.clone(),
                None => format!("-- MANUAL: cannot auto-reverse: {}", c.summary),
            })
            .collect::<Vec<_>>()
            .join("\n");
        // Executable scripts: prefer a hand-written down file, else the generated reverse.
        let down_sql = mf
            .down
            .as_ref()
            .and_then(|p| std::fs::read_to_string(p).ok())
            .unwrap_or_else(|| generated_down.clone());
        let applied = tracker.map(|a| applied::is_applied(a, &mf.version, &mf.name, index));
        let (apply_script, rollback_script) = applied::build_scripts(
            tracker,
            &mf.version,
            &mf.name,
            &sql,
            &down_sql,
            prev_version.as_deref(),
        );
        prev_version = Some(mf.version.clone());
        migrations.push(MigrationView {
            version: mf.version,
            name: mf.name,
            up_file: mf.up.file_name().and_then(|s| s.to_str()).unwrap_or("").to_string(),
            has_down_file: mf.down.is_some(),
            changes,
            generated_down,
            parse_error,
            partial_parse,
            applied,
            apply_script: Some(apply_script),
            rollback_script: Some(rollback_script),
        });
    }

    let drift = catalog.map(|c| diff(&model, c));
    MigrationReport {
        dir: dir.to_string(),
        migrations,
        drift,
        error: None,
        tracker: tracker.map(|a| a.kind.as_str().to_string()),
        tracker_table: tracker.map(|a| a.table.clone()),
    }
}

/// Given a project root, find the migrations folder by probing common ORM layouts.
/// Returns the first candidate that actually contains `.sql` files.
pub fn detect_dir(project_dir: &str) -> Option<String> {
    let root = Path::new(project_dir);
    const CANDIDATES: &[&str] = &[
        "prisma/migrations",
        "drizzle",
        "migrations",
        "db/migrate",
        "db/migrations",
        "supabase/migrations",
        "database/migrations",
        "sql/migrations",
    ];
    let has_sql = |p: &Path| {
        let mut v = Vec::new();
        let _ = collect_sql(p, &mut v);
        !v.is_empty()
    };
    for c in CANDIDATES {
        let p = root.join(c);
        if p.is_dir() && has_sql(&p) {
            return Some(p.to_string_lossy().into_owned());
        }
    }
    // Fallback: only a flat SQL-at-root layout. Recursing through the entire project
    // root can walk node_modules/build outputs and make the migration screen feel hung.
    if has_direct_sql(root) {
        return Some(project_dir.to_string());
    }
    None
}

/// ORM/migration bookkeeping tables — never real drift.
fn is_tracking_table(name: &str) -> bool {
    let n = name.to_lowercase();
    matches!(
        n.as_str(),
        "_prisma_migrations"
            | "schema_migrations"
            | "__drizzle_migrations"
            | "_drizzle_migrations"
            | "_sqlx_migrations"
            | "flyway_schema_history"
            | "ar_internal_metadata"
            | "atlas_schema_revisions"
            | "goose_db_version"
    ) || n.contains("schema_migrations")
        || n.ends_with("_migrations")
        || n.contains("migration_lock")
}

fn diff(model: &Model, catalog: &Catalog) -> Drift {
    // Schemas the migrations dir actually touches — we only judge drift within these, so a
    // Supabase/PG DB's auth/storage/etc schemas don't all show up as bogus "extra in DB".
    let touched: BTreeSet<String> = model.tables.keys().map(|(s, _)| s.clone()).collect();

    // DB tables keyed by (schema-or-"", name); DB names are already canonical (no re-folding).
    let db_tables: BTreeMap<TableKey, BTreeSet<String>> = catalog
        .tables
        .iter()
        .filter(|t| !is_tracking_table(&t.name))
        .map(|t| {
            (
                (t.schema.clone().unwrap_or_default(), t.name.clone()),
                t.columns.iter().map(|c| c.name.clone()).collect(),
            )
        })
        .collect();

    let mut pending_tables = Vec::new();
    let mut column_diffs = Vec::new();
    for (k, ts) in &model.tables {
        match db_tables.get(k) {
            None => pending_tables.push(display_key(k)),
            Some(db_cols) => {
                let mig_cols = ts.column_keys();
                let missing_in_db: Vec<String> = mig_cols.difference(db_cols).cloned().collect();
                let extra_in_db: Vec<String> = db_cols.difference(&mig_cols).cloned().collect();
                if !missing_in_db.is_empty() || !extra_in_db.is_empty() {
                    column_diffs.push(ColumnDiff { table: display_key(k), missing_in_db, extra_in_db });
                }
            }
        }
    }
    let extra_tables: Vec<String> = db_tables
        .keys()
        .filter(|k| !model.tables.contains_key(*k) && touched.contains(&k.0))
        .map(display_key)
        .collect();

    Drift { pending_tables, extra_tables, column_diffs }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::introspect::{Column, Table};

    fn parse1(sql: &str) -> Statement {
        Parser::parse_sql(&GenericDialect {}, sql).unwrap().remove(0)
    }

    fn tbl(schema: Option<&str>, name: &str, cols: &[&str]) -> Table {
        Table {
            schema: schema.map(str::to_string),
            name: name.to_string(),
            kind: "table".into(),
            columns: cols
                .iter()
                .map(|c| Column { name: c.to_string(), data_type: "text".into(), nullable: true, pk: false })
                .collect(),
            foreign_keys: vec![],
            indexes: vec![],
            row_estimate: None,
        }
    }

    #[test]
    fn generates_reverse_from_replayed_state() {
        let mut m = Model::default();

        let c = apply(&mut m, &parse1("CREATE TABLE users (id INT, name TEXT);"));
        assert_eq!(c[0].kind, "createTable");
        assert!(c[0].down.as_ref().unwrap().to_uppercase().contains("DROP TABLE"));

        let c = apply(&mut m, &parse1("ALTER TABLE users ADD COLUMN email TEXT;"));
        assert_eq!(c[0].kind, "addColumn");
        assert!(c[0].down.as_ref().unwrap().to_uppercase().contains("DROP COLUMN"));

        // DROP COLUMN — reverse must re-ADD with the type we learned from the replay.
        let c = apply(&mut m, &parse1("ALTER TABLE users DROP COLUMN email;"));
        assert!(c[0].reversible);
        let down = c[0].down.as_ref().unwrap().to_uppercase();
        assert!(down.contains("ADD COLUMN") && down.contains("EMAIL"));

        // DROP TABLE — reverse must reconstruct the original CREATE.
        let c = apply(&mut m, &parse1("DROP TABLE users;"));
        assert!(c[0].reversible);
        assert!(c[0].down.as_ref().unwrap().to_uppercase().contains("CREATE TABLE"));
    }

    #[test]
    fn scans_and_analyzes_a_folder() {
        let base = std::env::temp_dir().join(format!("dopedb-migtest-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).unwrap();
        // sqlx / golang-migrate style: <ver>_<name>.up.sql + .down.sql
        std::fs::write(base.join("001_init.up.sql"), "CREATE TABLE users (id INT, name TEXT);").unwrap();
        std::fs::write(base.join("001_init.down.sql"), "DROP TABLE users;").unwrap();
        std::fs::write(base.join("002_add_email.up.sql"), "ALTER TABLE users ADD COLUMN email TEXT;").unwrap();

        let r = analyze(base.to_str().unwrap(), None, None);
        assert_eq!(r.migrations.len(), 2, "should find 2 up migrations");
        assert_eq!(r.migrations[0].version, "001");
        assert!(r.migrations[0].has_down_file, "001 should pair its .down.sql");
        assert!(!r.migrations[1].has_down_file, "002 has no down file");
        assert!(
            r.migrations[1].generated_down.to_uppercase().contains("DROP COLUMN"),
            "generated down for add-column should drop it"
        );
        let _ = std::fs::remove_dir_all(&base);
    }

    // ── defect 1: numeric-aware version ordering ──
    #[test]
    fn natural_sort_v2_before_v10() {
        assert_eq!(natural_cmp("2", "10"), std::cmp::Ordering::Less);
        assert_eq!(natural_cmp("10", "2"), std::cmp::Ordering::Greater);
        // zero-padded and timestamps keep working
        assert_eq!(natural_cmp("001", "002"), std::cmp::Ordering::Less);
        assert_eq!(natural_cmp("20230101120000", "20230101130000"), std::cmp::Ordering::Less);

        let base = std::env::temp_dir().join(format!("dopedb-migsort-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).unwrap();
        // Flyway V-style: V2 must replay before V10.
        std::fs::write(base.join("V2__a.sql"), "CREATE TABLE a (id INT);").unwrap();
        std::fs::write(base.join("V10__b.sql"), "CREATE TABLE b (id INT);").unwrap();
        let r = analyze(base.to_str().unwrap(), None, None);
        let vers: Vec<_> = r.migrations.iter().map(|m| m.version.clone()).collect();
        assert_eq!(vers, vec!["2".to_string(), "10".to_string()], "V2 before V10");
        let _ = std::fs::remove_dir_all(&base);
    }

    // ── defect 2: Prisma down.sql not double-ingested + unique folder identity ──
    #[test]
    fn prisma_down_sql_not_double_ingested() {
        let base = std::env::temp_dir().join(format!("dopedb-migprisma-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let f1 = base.join("20230101_init");
        let f2 = base.join("20230102_more");
        std::fs::create_dir_all(&f1).unwrap();
        std::fs::create_dir_all(&f2).unwrap();
        std::fs::write(f1.join("migration.sql"), "CREATE TABLE a (id INT);").unwrap();
        std::fs::write(f1.join("down.sql"), "DROP TABLE a;").unwrap();
        std::fs::write(f2.join("migration.sql"), "CREATE TABLE b (id INT);").unwrap();

        let r = analyze(base.to_str().unwrap(), None, None);
        assert_eq!(r.migrations.len(), 2, "down.sql must NOT be counted as an up migration");
        // No migration keyed as version/name "down".
        assert!(r.migrations.iter().all(|m| m.name != "down" && m.version != "down"));
        // Folder identity is unique: version+"/"+name never collides.
        let keys: BTreeSet<String> = r.migrations.iter().map(|m| format!("{}/{}", m.version, m.name)).collect();
        assert_eq!(keys.len(), 2, "each prisma folder gets a unique version/name key");
        // The folder with a down.sql sibling is paired as having a down file.
        let init = r.migrations.iter().find(|m| m.name == "20230101_init").unwrap();
        assert!(init.has_down_file, "prisma down.sql sibling should pair");
        let _ = std::fs::remove_dir_all(&base);
    }

    // ── defect 3: partial-parse resilience ──
    #[test]
    fn partial_parse_applies_valid_statements() {
        let base = std::env::temp_dir().join(format!("dopedb-migpartial-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).unwrap();
        // A DO $$ block sqlparser can't parse, wrapped by two valid CREATE TABLEs.
        let sql = "CREATE TABLE a (id INT);\n\
                   DO $$ BEGIN RAISE NOTICE 'hi ; not a delimiter'; END $$;\n\
                   CREATE TABLE b (id INT);";
        std::fs::write(base.join("001_mixed.sql"), sql).unwrap();

        let r = analyze(base.to_str().unwrap(), None, None);
        let m = &r.migrations[0];
        assert!(m.partial_parse, "should flag partial parse");
        assert!(m.parse_error.is_some(), "should keep the parse error");
        let kinds: Vec<&str> = m.changes.iter().map(|c| c.kind.as_str()).collect();
        assert_eq!(kinds.iter().filter(|k| **k == "createTable").count(), 2, "both valid CREATEs apply");
        assert!(kinds.contains(&"unknown"), "the DO block becomes an opaque unknown change");
        // The manual placeholder for the irreversible unknown must appear in the down.
        assert!(m.generated_down.contains("-- MANUAL: cannot auto-reverse"));
        let _ = std::fs::remove_dir_all(&base);
    }

    // ── defect 5b: rename-then-drop uses the NEW name ──
    #[test]
    fn rename_then_drop_uses_new_name() {
        let mut m = Model::default();
        apply(&mut m, &parse1("CREATE TABLE users (id INT);"));
        apply(&mut m, &parse1("ALTER TABLE users RENAME TO members;"));
        let c = apply(&mut m, &parse1("DROP TABLE members;"));
        let down = c[0].down.as_ref().unwrap();
        assert!(down.to_uppercase().contains("CREATE TABLE"));
        assert!(down.to_lowercase().contains("members"), "recreates the NEW name: {down}");
        assert!(!down.to_lowercase().contains("users"), "must not recreate the OLD name: {down}");
    }

    // ── defect 5a: alter-column-then-drop-column regenerates the ALTERED type ──
    #[test]
    fn alter_column_then_drop_column_regenerates_altered_type() {
        let mut m = Model::default();
        apply(&mut m, &parse1("CREATE TABLE t (id INT, amount INT);"));
        apply(&mut m, &parse1("ALTER TABLE t ALTER COLUMN amount SET DATA TYPE BIGINT;"));
        let c = apply(&mut m, &parse1("ALTER TABLE t DROP COLUMN amount;"));
        let down = c[0].down.as_ref().unwrap().to_uppercase();
        // Pre-alter type was INT; only the ALTERED BIGINT proves the def was updated in place.
        assert!(down.contains("ADD COLUMN") && down.contains("BIGINT"), "altered type must survive: {down}");
    }

    // ── defect 5c: add-constraint folds into DROP TABLE reconstruction ──
    #[test]
    fn add_constraint_survives_drop_table() {
        let mut m = Model::default();
        apply(&mut m, &parse1("CREATE TABLE t (id INT, email TEXT);"));
        apply(&mut m, &parse1("ALTER TABLE t ADD CONSTRAINT uq_email UNIQUE (email);"));
        let c = apply(&mut m, &parse1("DROP TABLE t;"));
        let down = c[0].down.as_ref().unwrap().to_uppercase();
        assert!(down.contains("UQ_EMAIL"), "reconstructed CREATE must keep the constraint: {down}");
    }

    // ── defect 6: DROP TABLE down includes the table's indexes ──
    #[test]
    fn drop_table_down_includes_indexes() {
        let mut m = Model::default();
        apply(&mut m, &parse1("CREATE TABLE books (id INT, author_id INT);"));
        apply(&mut m, &parse1("CREATE INDEX idx_books_author ON books (author_id);"));
        let c = apply(&mut m, &parse1("DROP TABLE books;"));
        let down = c[0].down.as_ref().unwrap();
        assert!(down.to_uppercase().contains("CREATE TABLE"));
        assert!(down.contains("idx_books_author"), "down must recreate the dropped table's index: {down}");
        // Stale index entry purged.
        assert!(m.indexes.is_empty(), "dropped table's indexes removed from the model");
    }

    // ── flyway sub-versions (V1_1, V1_2) must stay distinct, not collapse to "1" ──
    #[test]
    fn flyway_subversions_stay_distinct() {
        let base = std::env::temp_dir().join(format!("dopedb-migflyway-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).unwrap();
        std::fs::write(base.join("V1__init.sql"), "CREATE TABLE a (id INT);").unwrap();
        std::fs::write(base.join("V1_1__more.sql"), "CREATE TABLE b (id INT);").unwrap();
        std::fs::write(base.join("V1_2__yet.sql"), "CREATE TABLE c (id INT);").unwrap();

        let r = analyze(base.to_str().unwrap(), None, None);
        let vers: Vec<_> = r.migrations.iter().map(|m| m.version.clone()).collect();
        assert_eq!(
            vers,
            vec!["1".to_string(), "1.1".to_string(), "1.2".to_string()],
            "V1_1 and V1_2 must resolve to 1.1 and 1.2, not both to \"1\""
        );
        let _ = std::fs::remove_dir_all(&base);
    }

    // ── MySQL backslash-escaped quotes must not split a statement mid-literal ──
    #[test]
    fn mysql_backslash_escapes_do_not_split_mid_literal() {
        use crate::model::Engine;
        let sql = r"INSERT INTO t VALUES ('a\';b'); INSERT INTO t VALUES ('c');";
        // MySQL honors `\'`, so the inner ';' is data, not a terminator → exactly 2 stmts.
        let mysql = split_statements_for(sql, Engine::Mysql);
        assert_eq!(mysql.len(), 2, "escaped-quote literal kept whole: {mysql:?}");
        assert!(mysql[0].contains(r"'a\';b'"), "literal preserved: {:?}", mysql[0]);
        // Default (PG/SQLite) scan treats backslash literally, so it mis-reads the closing
        // quote and truncates the first statement mid-literal (the bug this guards for MySQL).
        let plain = split_statements(sql);
        assert!(
            !plain[0].contains(r"'a\';b'"),
            "default scan truncates the escaped-quote literal: {:?}",
            plain[0]
        );
    }

    // ── defect 4 + 8: drift is schema-qualified and ignores tracking tables ──
    #[test]
    fn drift_is_schema_qualified_and_skips_tracking_tables() {
        let base = std::env::temp_dir().join(format!("dopedb-migdrift-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).unwrap();
        std::fs::write(base.join("001.sql"), "CREATE TABLE users (id INT, name TEXT);").unwrap();

        // PG-style catalog: public.users matches; auth.users is a DIFFERENT schema (not touched);
        // _prisma_migrations is a tracking table; auth.* must not be bogus extra drift.
        let catalog = Catalog {
            tables: vec![
                tbl(Some("public"), "users", &["id", "name"]),
                tbl(Some("auth"), "users", &["id"]),
                tbl(Some("public"), "_prisma_migrations", &["id"]),
            ],
        };
        let r = analyze(base.to_str().unwrap(), Some(&catalog), None);
        let d = r.drift.unwrap();
        assert!(d.pending_tables.is_empty(), "public.users is present: {:?}", d.pending_tables);
        assert!(d.column_diffs.is_empty(), "columns match: {:?}", d.column_diffs);
        // auth.users lives in an untouched schema; _prisma_migrations is excluded → no extra drift.
        assert!(d.extra_tables.is_empty(), "no bogus extra tables: {:?}", d.extra_tables);
    }
}
