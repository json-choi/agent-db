//! Read path (L2 read-only pool). Executes a `SELECT` against the connection's
//! read-only, L2-enforced pool and maps rows dynamically to JSON.
//!
//! sqlx has no single dynamic-row API across engines (PgRow/MySqlRow/SqliteRow
//! carry different `Column`/`TypeInfo` types), so the per-engine mappers below
//! are unavoidable duplication rather than a missing abstraction. The mappers and
//! [`stream_capped`] are `pub(crate)` and reused by `safety::l2_enforce` (the MCP
//! path) so both paths decode a cell identically.

use std::time::Instant;

use chrono::{DateTime, FixedOffset, NaiveDate, NaiveDateTime, NaiveTime, Utc};
use futures::TryStreamExt;
use serde_json::Value;
use sqlx::mysql::types::{MySqlTime, MySqlTimeSign};
use sqlx::mysql::MySqlRow;
use sqlx::postgres::types::{Oid, PgInterval, PgMoney, PgRange, PgTimeTz};
use sqlx::postgres::{PgRow, PgTypeKind};
use sqlx::sqlite::SqliteRow;
use sqlx::types::Decimal;
use sqlx::{AssertSqlSafe, Column, Executor, Row, SqlSafeStr, TypeInfo, ValueRef};
use uuid::Uuid;

// PG-only decoders enabled via confirmed, TLS-agnostic sqlx features (see Cargo.toml).
use sqlx::types::ipnetwork::IpNetwork;
use sqlx::types::mac_address::MacAddress;
use sqlx::types::BitVec;

use crate::connection::{LiveConnection, Pool};
use crate::error::{AppError, AppResult};
use crate::executor::cancel;
use crate::model::{Engine, QueryResult};

/// Run a read (`SELECT`/`EXPLAIN`) against the read-only pool (L2). Streams rows,
/// caps at `max_rows` (setting `truncated` when more exist), maps values by type.
/// `query_id` (if set) makes the read cancellable via [`cancel::cancel_query`]; the
/// whole read is also bounded by a wall-clock timeout.
pub async fn run_read(
    live: &LiveConnection,
    _engine: Engine, // pool enum is self-describing; kept to honor the executor contract
    sql: &str,
    max_rows: u64,
    query_id: Option<Uuid>,
) -> AppResult<QueryResult> {
    let started = Instant::now();
    let max = max_rows as usize;

    // ponytail: read_pool is the L2-enforced (read-only) pool; reads never touch write_pool.
    let inner = async {
        let (columns, rows, truncated) = match &live.read_pool {
            Pool::Postgres(pool) => {
                let (c, r, t) =
                    stream_capped(sqlx::query(AssertSqlSafe(sql)).fetch(pool), max, pg_value)
                        .await?;
                (with_headers(c, pool, sql).await, r, t)
            }
            Pool::Mysql(pool) => {
                let (c, r, t) =
                    stream_capped(
                        sqlx::query(AssertSqlSafe(sql)).fetch(pool),
                        max,
                        mysql_value,
                    )
                    .await?;
                (with_headers(c, pool, sql).await, r, t)
            }
            Pool::Sqlite(pool) => {
                let (c, r, t) =
                    stream_capped(
                        sqlx::query(AssertSqlSafe(sql)).fetch(pool),
                        max,
                        sqlite_value,
                    )
                    .await?;
                (with_headers(c, pool, sql).await, r, t)
            }
        };
        Ok::<_, AppError>(QueryResult {
            row_count: rows.len(),
            columns,
            rows,
            truncated,
            duration_ms: 0,
        })
    };

    let mut result = cancel::guard(query_id, cancel::QUERY_TIMEOUT, inner).await?;
    result.duration_ms = started.elapsed().as_millis() as u64;
    Ok(result)
}

/// Columns from the first row are empty when zero rows come back; fall back to the
/// prepared-statement metadata (`describe`) so an empty result still has headers.
async fn with_headers<'e, E>(cols: Vec<String>, ex: E, sql: &str) -> Vec<String>
where
    E: Executor<'e>,
{
    if cols.is_empty() {
        ex.describe(AssertSqlSafe(sql).into_sql_str())
            .await
            .ok()
            .map(describe_cols)
            .unwrap_or_default()
    } else {
        cols
    }
}

/// Column names from statement metadata (used for zero-row headers).
pub(crate) fn describe_cols<DB: sqlx::Database>(d: sqlx::Describe<DB>) -> Vec<String> {
    d.columns().iter().map(|c| c.name().to_string()).collect()
}

/// Stream rows to JSON, capping at `max` (fetch stops one past the cap → `truncated`),
/// so memory is bounded regardless of result size. Column names come from the first
/// row; a zero-row stream returns empty columns (caller fills from `describe`).
pub(crate) async fn stream_capped<S, R>(
    mut stream: S,
    max: usize,
    f: impl Fn(&R, usize) -> Value,
) -> Result<(Vec<String>, Vec<Vec<Value>>, bool), sqlx::Error>
where
    S: futures::Stream<Item = Result<R, sqlx::Error>> + Unpin,
    R: Row,
{
    let mut columns: Vec<String> = Vec::new();
    let mut rows: Vec<Vec<Value>> = Vec::new();
    let mut truncated = false;
    while let Some(row) = stream.try_next().await? {
        if columns.is_empty() {
            columns = row.columns().iter().map(|c| c.name().to_string()).collect();
        }
        if rows.len() >= max {
            truncated = true; // one row past the cap exists → more rows remain
            break;
        }
        let n = row.columns().len();
        rows.push((0..n).map(|i| f(&row, i)).collect());
    }
    Ok((columns, rows, truncated))
}

// ── value decoding ────────────────────────────────────────────────────────────

/// JS `Number` loses precision past 2^53; anything larger is emitted as a string.
const JS_MAX_SAFE_INT: u64 = 1 << 53;

/// `Ok` → JSON value; decode error (including SQL NULL on a non-`Option` get) → `Null`.
/// Only used for exact-type arms where a decode error genuinely means NULL.
fn jv<T: Into<Value>>(r: Result<T, sqlx::Error>) -> Value {
    r.map(Into::into).unwrap_or(Value::Null)
}

/// Ints outside JS's safe range become JSON strings to avoid silent corruption.
pub(crate) fn int_json(v: i64) -> Value {
    if v.unsigned_abs() > JS_MAX_SAFE_INT {
        Value::String(v.to_string())
    } else {
        Value::from(v)
    }
}

pub(crate) fn uint_json(v: u64) -> Value {
    if v > JS_MAX_SAFE_INT {
        Value::String(v.to_string())
    } else {
        Value::from(v)
    }
}

fn int_or_null(r: Result<i64, sqlx::Error>) -> Value {
    r.map(int_json).unwrap_or(Value::Null)
}

fn uint_or_null(r: Result<u64, sqlx::Error>) -> Value {
    r.map(uint_json).unwrap_or(Value::Null)
}

fn hex_str(b: Vec<u8>) -> String {
    format!("\\x{}", hex::encode(b))
}

fn iso_dt(t: chrono::NaiveDateTime) -> String {
    // ISO-8601 (T separator, trailing fractional only when nonzero).
    t.format("%Y-%m-%dT%H:%M:%S%.f").to_string()
}

/// A cell we could not decode: a real SQL NULL stays `Null`, anything else becomes
/// a VISIBLE marker naming the column type — so real data (money, arrays, interval,
/// inet, …) never masquerades as NULL in the grid.
fn null_or_marker<R: Row>(row: &R, i: usize, ty: &str) -> Value
where
    usize: sqlx::ColumnIndex<R>,
{
    if row.try_get_raw(i).map(|v| v.is_null()).unwrap_or(false) {
        Value::Null
    } else {
        Value::String(format!("<unsupported: {}>", ty.to_ascii_lowercase()))
    }
}

/// MONEY has no scale on the wire; the fractional-digit count comes from the DB's
/// `lc_monetary`. 2 is the near-universal default (en_US etc). ponytail: single knob —
/// set per-connection from `SHOW lc_monetary` (0 for KRW/JPY) if a DB ever needs it.
const PG_MONEY_FRAC_DIGITS: u32 = 2;

pub(crate) fn pg_value(row: &PgRow, i: usize) -> Value {
    let ty = row.column(i).type_info().name().to_ascii_uppercase();
    match ty.as_str() {
        "BOOL" => jv(row.try_get::<bool, _>(i)),
        "INT2" => jv(row.try_get::<i16, _>(i).map(|v| v as i64)),
        "INT4" => jv(row.try_get::<i32, _>(i).map(|v| v as i64)),
        "INT8" => int_or_null(row.try_get::<i64, _>(i)),
        "OID" => jv(row.try_get::<Oid, _>(i).map(|o| o.0)),
        "FLOAT4" => jv(row.try_get::<f32, _>(i).map(|v| v as f64)),
        "FLOAT8" => jv(row.try_get::<f64, _>(i)),
        // NUMERIC: exact string; out-of-range for Decimal → marker, never NULL.
        "NUMERIC" => match row.try_get::<Decimal, _>(i) {
            Ok(d) => Value::String(d.to_string()),
            Err(_) => null_or_marker(row, i, &ty),
        },
        // MONEY is an i64 of minor units, NOT a Decimal on the wire (rust_decimal only
        // decodes NUMERIC), so it needs PgMoney; the old NUMERIC|MONEY arm just markered.
        "MONEY" => match row.try_get::<PgMoney, _>(i) {
            Ok(m) => Value::String(m.to_decimal(PG_MONEY_FRAC_DIGITS).to_string()),
            Err(_) => null_or_marker(row, i, &ty),
        },
        "TEXT" | "VARCHAR" | "BPCHAR" | "CHAR" | "NAME" | "CITEXT" => {
            jv(row.try_get::<String, _>(i))
        }
        "UUID" => jv(row.try_get::<uuid::Uuid, _>(i).map(|u| u.to_string())),
        "JSON" | "JSONB" => row.try_get::<Value, _>(i).unwrap_or(Value::Null),
        "TIMESTAMPTZ" => {
            jv(row.try_get::<DateTime<Utc>, _>(i).map(|t| t.to_rfc3339()))
        }
        "TIMESTAMP" => jv(row.try_get::<NaiveDateTime, _>(i).map(iso_dt)),
        "DATE" => jv(row.try_get::<NaiveDate, _>(i).map(|t| t.to_string())),
        "TIME" => jv(row.try_get::<NaiveTime, _>(i).map(|t| t.to_string())),
        "TIMETZ" => match row.try_get::<PgTimeTz<NaiveTime, FixedOffset>, _>(i) {
            Ok(t) => Value::from(fmt_timetz(&t)),
            Err(_) => null_or_marker(row, i, &ty),
        },
        "INTERVAL" => match row.try_get::<PgInterval, _>(i) {
            Ok(iv) => Value::from(fmt_interval(&iv)),
            Err(_) => null_or_marker(row, i, &ty),
        },
        // Ranges render via PgRange's Display ("[1,5)" canonical form).
        "INT4RANGE" => pg_range::<i32>(row, i, &ty),
        "INT8RANGE" => pg_range::<i64>(row, i, &ty),
        "NUMRANGE" => pg_range::<Decimal>(row, i, &ty),
        "DATERANGE" => pg_range::<NaiveDate>(row, i, &ty),
        "TSRANGE" => pg_range::<NaiveDateTime>(row, i, &ty),
        "TSTZRANGE" => pg_range::<DateTime<Utc>>(row, i, &ty),
        "BYTEA" => jv(row.try_get::<Vec<u8>, _>(i).map(hex_str)),
        // inet/cidr, macaddr, bit/varbit via the sqlx feature decoders enabled in Cargo.toml.
        "INET" | "CIDR" => match row.try_get::<IpNetwork, _>(i) {
            Ok(n) => Value::from(n.to_string()),
            Err(_) => null_or_marker(row, i, &ty),
        },
        "MACADDR" => match row.try_get::<MacAddress, _>(i) {
            Ok(m) => Value::from(m.to_string()),
            Err(_) => null_or_marker(row, i, &ty),
        },
        "BIT" | "VARBIT" => match row.try_get::<BitVec, _>(i) {
            Ok(b) => Value::from(fmt_bits(&b)),
            Err(_) => null_or_marker(row, i, &ty),
        },
        // arrays (NAME[]/INT4[]/…) and custom enums land here.
        _ if ty.ends_with("[]") => pg_array(row, i, &ty),
        _ => pg_fallback(row, i, &ty),
    }
}

/// Render a range column as text via `PgRange<T>`'s `Display`. Generic over the element
/// so the six range types share one body; each `T` here has an owned `Decode` impl.
fn pg_range<T>(row: &PgRow, i: usize, ty: &str) -> Value
where
    T: std::fmt::Display,
    PgRange<T>: sqlx::Type<sqlx::Postgres>,
    for<'a> PgRange<T>: sqlx::Decode<'a, sqlx::Postgres>,
{
    match row.try_get::<PgRange<T>, _>(i) {
        Ok(r) => Value::from(r.to_string()),
        Err(_) => null_or_marker(row, i, ty),
    }
}

/// Map a decoded `Vec<Option<T>>` to a JSON array, NULL elements → `Value::Null`.
/// Decoding `Option<T>` per element is what lets an array containing a NULL decode at
/// all: sqlx runs `T::decode` on every element, so a bare `Vec<T>` errors on the first
/// NULL and markers the whole cell (a very common shape for real array columns).
fn arr<T>(
    r: Result<Vec<Option<T>>, sqlx::Error>,
    f: impl Fn(T) -> Value,
) -> Result<Vec<Value>, sqlx::Error> {
    r.map(|v| v.into_iter().map(|x| x.map(&f).unwrap_or(Value::Null)).collect())
}

/// Decode a PG array into a JSON array of the element rendering. sqlx names array types
/// `<BASE>[]` (display_name), so the element type is the name minus the `[]` suffix.
fn pg_array(row: &PgRow, i: usize, ty: &str) -> Value {
    let elem = ty.strip_suffix("[]").unwrap_or(ty);
    let decoded: Result<Vec<Value>, sqlx::Error> = match elem {
        "INT2" => arr(row.try_get(i), |x: i16| Value::from(x as i64)),
        "INT4" => arr(row.try_get(i), |x: i32| Value::from(x as i64)),
        "INT8" => arr(row.try_get(i), int_json),
        "FLOAT4" => arr(row.try_get(i), |x: f32| Value::from(x as f64)),
        "FLOAT8" => arr(row.try_get(i), |x: f64| Value::from(x)),
        "BOOL" => arr(row.try_get(i), |x: bool| Value::from(x)),
        "NUMERIC" => arr(row.try_get(i), |d: Decimal| Value::from(d.to_string())),
        "TEXT" | "VARCHAR" | "BPCHAR" | "CHAR" | "NAME" | "CITEXT" => {
            arr(row.try_get(i), |s: String| Value::from(s))
        }
        "UUID" => arr(row.try_get(i), |u: Uuid| Value::from(u.to_string())),
        "TIMESTAMPTZ" => arr(row.try_get(i), |t: DateTime<Utc>| Value::from(t.to_rfc3339())),
        "TIMESTAMP" => arr(row.try_get(i), |t: NaiveDateTime| Value::from(iso_dt(t))),
        "DATE" => arr(row.try_get(i), |t: NaiveDate| Value::from(t.to_string())),
        "TIME" => arr(row.try_get(i), |t: NaiveTime| Value::from(t.to_string())),
        "JSON" | "JSONB" => arr(row.try_get(i), |v: Value| v),
        // enum[] has an arbitrary element type name; detect structurally and read labels.
        _ => return pg_enum_array(row, i, ty),
    };
    match decoded {
        Ok(v) => Value::Array(v),
        Err(_) => null_or_marker(row, i, ty),
    }
}

/// An array whose element type name matched nothing above: if it is structurally an
/// array-of-enum, decode the labels (via `try_get_unchecked`, which skips the element
/// compat check that would otherwise reject the enum). Anything else → marker.
fn pg_enum_array(row: &PgRow, i: usize, ty: &str) -> Value {
    if let PgTypeKind::Array(inner) = row.column(i).type_info().kind() {
        if matches!(inner.kind(), PgTypeKind::Enum(_)) {
            if let Ok(v) = row.try_get_unchecked::<Vec<Option<String>>, _>(i) {
                return Value::Array(
                    v.into_iter().map(|x| x.map(Value::from).unwrap_or(Value::Null)).collect(),
                );
            }
        }
    }
    null_or_marker(row, i, ty)
}

fn pg_fallback(row: &PgRow, i: usize, ty: &str) -> Value {
    if let Ok(s) = row.try_get::<String, _>(i) {
        return Value::from(s);
    }
    if let Ok(v) = row.try_get::<i64, _>(i) {
        return int_json(v);
    }
    if let Ok(v) = row.try_get::<f64, _>(i) {
        return Value::from(v);
    }
    if let Ok(v) = row.try_get::<bool, _>(i) {
        return Value::from(v);
    }
    if let Ok(d) = row.try_get::<Decimal, _>(i) {
        return Value::String(d.to_string());
    }
    // Custom enum: on the prepared path (the only path dopedb uses) kind() is resolved
    // to Enum and never panics; the enum's wire bytes ARE its label, so a valid-UTF-8
    // decode yields it. A genuinely binary type fails from_utf8 and falls to the marker.
    if matches!(row.column(i).type_info().kind(), PgTypeKind::Enum(_)) {
        if let Ok(raw) = row.try_get_raw(i) {
            if let Ok(b) = raw.as_bytes() {
                if let Some(label) = bytes_as_label(b) {
                    return Value::from(label);
                }
            }
        }
    }
    null_or_marker(row, i, ty)
}

/// PG enum wire bytes are the label text (identical in Text and Binary format), so this
/// is the enum decoder; invalid UTF-8 (a real binary type) returns None → marker.
fn bytes_as_label(bytes: &[u8]) -> Option<String> {
    std::str::from_utf8(bytes).ok().map(str::to_owned)
}

/// psql-style interval, e.g. "1 year 2 mons 5 days 02:03:04.5". ponytail: PgInterval only
/// carries months/days/µs, so per-component sign nuance (rare) collapses into the time part.
fn fmt_interval(iv: &PgInterval) -> String {
    let mut out: Vec<String> = Vec::new();
    let (years, mons) = (iv.months / 12, iv.months % 12);
    if years != 0 {
        out.push(format!("{years} year{}", if years.abs() == 1 { "" } else { "s" }));
    }
    if mons != 0 {
        out.push(format!("{mons} mon{}", if mons.abs() == 1 { "" } else { "s" }));
    }
    if iv.days != 0 {
        out.push(format!("{} day{}", iv.days, if iv.days.abs() == 1 { "" } else { "s" }));
    }
    if iv.microseconds != 0 || out.is_empty() {
        let sign = if iv.microseconds < 0 { "-" } else { "" };
        let total = iv.microseconds.unsigned_abs();
        let (secs, us) = (total / 1_000_000, total % 1_000_000);
        let (h, m, s) = (secs / 3600, (secs % 3600) / 60, secs % 60);
        if us == 0 {
            out.push(format!("{sign}{h:02}:{m:02}:{s:02}"));
        } else {
            let frac = format!("{us:06}");
            out.push(format!("{sign}{h:02}:{m:02}:{s:02}.{}", frac.trim_end_matches('0')));
        }
    }
    out.join(" ")
}

/// TIMETZ as "13:14:15+02:00" (NaiveTime + FixedOffset both Display to those forms).
fn fmt_timetz(t: &PgTimeTz<NaiveTime, FixedOffset>) -> String {
    format!("{}{}", t.time, t.offset)
}

/// BIT/VARBIT as a string of 0/1, e.g. "1011".
fn fmt_bits(b: &BitVec) -> String {
    b.iter().map(|bit| if bit { '1' } else { '0' }).collect()
}

/// MySQL `TIME` is a signed duration with a much wider range than a time of day.
/// Keep MySQL's familiar zero-padded rendering while preserving the full
/// +/-838-hour range and fractional seconds.
fn fmt_mysql_time(t: &MySqlTime) -> String {
    let sign = if matches!(t.sign(), MySqlTimeSign::Negative) {
        "-"
    } else {
        ""
    };
    let mut out = format!(
        "{sign}{:02}:{:02}:{:02}",
        t.hours(),
        t.minutes(),
        t.seconds()
    );
    if t.microseconds() != 0 {
        let fraction = format!("{:06}", t.microseconds());
        out.push('.');
        out.push_str(fraction.trim_end_matches('0'));
    }
    out
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MySqlDecodeRoute {
    UnsignedInteger,
    Binary,
    SignedInteger,
    Float32,
    Float64,
    Decimal,
    Text,
    Set,
    DateTime,
    Date,
    Time,
    Json,
    Fallback,
}

fn mysql_decode_route(ty: &str) -> MySqlDecodeRoute {
    if ty.contains("UNSIGNED") || ty == "YEAR" {
        return MySqlDecodeRoute::UnsignedInteger;
    }
    if ty.contains("BLOB") || ty.contains("BINARY") {
        return MySqlDecodeRoute::Binary;
    }
    match ty {
        "TINYINT" | "SMALLINT" | "MEDIUMINT" | "INT" | "BIGINT" => {
            MySqlDecodeRoute::SignedInteger
        }
        "FLOAT" => MySqlDecodeRoute::Float32,
        "DOUBLE" => MySqlDecodeRoute::Float64,
        "DECIMAL" | "NEWDECIMAL" => MySqlDecodeRoute::Decimal,
        "VARCHAR" | "CHAR" | "TEXT" | "TINYTEXT" | "MEDIUMTEXT" | "LONGTEXT" | "ENUM" => {
            MySqlDecodeRoute::Text
        }
        "SET" => MySqlDecodeRoute::Set,
        "DATETIME" | "TIMESTAMP" => MySqlDecodeRoute::DateTime,
        "DATE" => MySqlDecodeRoute::Date,
        "TIME" => MySqlDecodeRoute::Time,
        "JSON" => MySqlDecodeRoute::Json,
        _ => MySqlDecodeRoute::Fallback,
    }
}

pub(crate) fn mysql_value(row: &MySqlRow, i: usize) -> Value {
    let ty = row.column(i).type_info().name().to_ascii_uppercase();
    match mysql_decode_route(&ty) {
        // SQLx models YEAR as an unsigned integer even though its type name does
        // not carry the `UNSIGNED` suffix used by ordinary integer columns.
        MySqlDecodeRoute::UnsignedInteger => uint_or_null(row.try_get::<u64, _>(i)),
        MySqlDecodeRoute::Binary => jv(row.try_get::<Vec<u8>, _>(i).map(hex_str)),
        MySqlDecodeRoute::SignedInteger => int_or_null(row.try_get::<i64, _>(i)),
        MySqlDecodeRoute::Float32 => jv(row.try_get::<f32, _>(i).map(|v| v as f64)),
        MySqlDecodeRoute::Float64 => jv(row.try_get::<f64, _>(i)),
        MySqlDecodeRoute::Decimal => match row.try_get::<Decimal, _>(i) {
            Ok(d) => Value::String(d.to_string()),
            Err(_) => null_or_marker(row, i, &ty),
        },
        MySqlDecodeRoute::Text => {
            jv(row.try_get::<String, _>(i))
        }
        // SET is textual on the wire, but SQLx 0.8 omits ColumnType::Set from
        // String::compatible. The unchecked get skips only that type guard while
        // retaining SQLx's normal UTF-8 decoder.
        MySqlDecodeRoute::Set => match row.try_get_unchecked::<String, _>(i) {
            Ok(value) => Value::from(value),
            Err(_) => null_or_marker(row, i, &ty),
        },
        MySqlDecodeRoute::DateTime => {
            jv(row.try_get::<chrono::NaiveDateTime, _>(i).map(iso_dt))
        }
        MySqlDecodeRoute::Date => {
            jv(row.try_get::<chrono::NaiveDate, _>(i).map(|t| t.to_string()))
        }
        MySqlDecodeRoute::Time => match row.try_get::<MySqlTime, _>(i) {
            Ok(t) => Value::from(fmt_mysql_time(&t)),
            Err(_) => mysql_fallback(row, i, &ty),
        },
        MySqlDecodeRoute::Json => row.try_get::<Value, _>(i).unwrap_or(Value::Null),
        // BIT and anything unlisted fall through.
        MySqlDecodeRoute::Fallback => mysql_fallback(row, i, &ty),
    }
}

fn mysql_fallback(row: &MySqlRow, i: usize, ty: &str) -> Value {
    if let Ok(s) = row.try_get::<String, _>(i) {
        return Value::from(s);
    }
    if let Ok(v) = row.try_get::<i64, _>(i) {
        return int_json(v);
    }
    if let Ok(v) = row.try_get::<u64, _>(i) {
        return uint_json(v);
    }
    if let Ok(v) = row.try_get::<f64, _>(i) {
        return Value::from(v);
    }
    if let Ok(d) = row.try_get::<Decimal, _>(i) {
        return Value::String(d.to_string());
    }
    if let Ok(b) = row.try_get::<Vec<u8>, _>(i) {
        return Value::from(hex_str(b)); // BIT etc.
    }
    null_or_marker(row, i, ty)
}

pub(crate) fn sqlite_value(row: &SqliteRow, i: usize) -> Value {
    // ponytail: SQLite is dynamically typed (declared type != stored class), so probe
    // storage classes in order. The five classes are covered, so all-fail == real NULL.
    if let Ok(v) = row.try_get::<i64, _>(i) {
        return int_json(v);
    }
    if let Ok(v) = row.try_get::<f64, _>(i) {
        return Value::from(v);
    }
    if let Ok(s) = row.try_get::<String, _>(i) {
        return Value::from(s);
    }
    if let Ok(b) = row.try_get::<Vec<u8>, _>(i) {
        return Value::from(hex_str(b));
    }
    Value::Null
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mysql_time_preserves_duration_range_sign_and_fraction() {
        let negative = MySqlTime::new(MySqlTimeSign::Negative, 25, 1, 2, 123_400).unwrap();
        let long = MySqlTime::new(MySqlTimeSign::Positive, 838, 59, 59, 0).unwrap();
        let short = MySqlTime::new(MySqlTimeSign::Positive, 1, 2, 3, 0).unwrap();

        assert_eq!(fmt_mysql_time(&negative), "-25:01:02.1234");
        assert_eq!(fmt_mysql_time(&long), "838:59:59");
        assert_eq!(fmt_mysql_time(&short), "01:02:03");
    }

    #[test]
    fn mysql_year_and_set_use_their_required_decoder_routes() {
        assert_eq!(mysql_decode_route("YEAR"), MySqlDecodeRoute::UnsignedInteger);
        assert_eq!(mysql_decode_route("BIGINT UNSIGNED"), MySqlDecodeRoute::UnsignedInteger);
        assert_eq!(mysql_decode_route("BIGINT"), MySqlDecodeRoute::SignedInteger);
        assert_eq!(mysql_decode_route("SET"), MySqlDecodeRoute::Set);
    }

    #[test]
    fn big_ints_become_strings() {
        assert_eq!(int_json(2), Value::from(2));
        assert_eq!(int_json(1 << 53), Value::from(9_007_199_254_740_992_i64)); // exactly 2^53 stays a number
        assert_eq!(
            int_json(9_007_199_254_740_993),
            Value::String("9007199254740993".into())
        );
        assert_eq!(
            int_json(-9_007_199_254_740_993),
            Value::String("-9007199254740993".into())
        );
        assert_eq!(
            uint_json(u64::MAX),
            Value::String(u64::MAX.to_string())
        );
        assert_eq!(uint_json(10), Value::from(10u64));
    }

    #[test]
    fn interval_formats_psql_style() {
        let iv = |months, days, microseconds| PgInterval { months, days, microseconds };
        assert_eq!(fmt_interval(&iv(0, 0, 0)), "00:00:00");
        assert_eq!(fmt_interval(&iv(0, 1, 7_380_000_000)), "1 day 02:03:00");
        assert_eq!(fmt_interval(&iv(14, 5, 0)), "1 year 2 mons 5 days");
        assert_eq!(fmt_interval(&iv(1, 0, 0)), "1 mon");
        assert_eq!(fmt_interval(&iv(0, 2, 0)), "2 days");
        // fractional seconds keep only significant digits
        assert_eq!(fmt_interval(&iv(0, 0, 4_500_000)), "00:00:04.5");
        // negative time part
        assert_eq!(fmt_interval(&iv(0, 0, -3_600_000_000)), "-01:00:00");
    }

    #[test]
    fn range_display_is_canonical() {
        // pg_range delegates to PgRange's Display; lock the "[start,end)" rendering.
        use std::ops::Bound;
        let r = PgRange { start: Bound::Included(1_i32), end: Bound::Excluded(5_i32) };
        assert_eq!(r.to_string(), "[1,5)");
    }

    #[test]
    fn enum_bytes_decode_to_label_utf8_only() {
        // enum wire bytes are the label; valid UTF-8 -> label, binary garbage -> None (marker).
        assert_eq!(bytes_as_label(b"active"), Some("active".to_string()));
        assert_eq!(bytes_as_label(&[0xff, 0xfe, 0x00]), None);
    }

    #[test]
    fn array_element_name_is_base_minus_suffix() {
        // pg_array strips the "[]" sqlx appends to array display names.
        assert_eq!("INT4[]".strip_suffix("[]"), Some("INT4"));
        assert_eq!("CALLS_STATUS_ENUM[]".strip_suffix("[]"), Some("CALLS_STATUS_ENUM"));
        // scalar rendering used by array arms: big int8 elements stay strings, ints stay numbers
        let elems: Vec<Value> = vec![1_i64, 9_007_199_254_740_993].into_iter().map(int_json).collect();
        assert_eq!(Value::Array(elems), serde_json::json!([1, "9007199254740993"]));
    }

    #[test]
    fn array_null_elements_become_json_null() {
        // A NULL element must map to Value::Null, not collapse the whole cell to a marker.
        let ok: Result<Vec<Option<i32>>, sqlx::Error> = Ok(vec![Some(1), None, Some(3)]);
        assert_eq!(
            Value::Array(arr(ok, |x: i32| Value::from(x as i64)).unwrap()),
            serde_json::json!([1, null, 3])
        );
    }
}
