//! Live connection pools. Each [`LiveConnection`] holds TWO pools: a normal
//! read/write pool and a SEPARATE read-only pool. The read-only pool is the first
//! line of L2 enforcement at the connection level — but the authoritative boundary
//! remains the per-request read-only transaction the executor opens:
//!   - Postgres: `after_connect` sets `default_transaction_read_only = on`.
//!   - MySQL:    `after_connect` sets `SESSION transaction_read_only = 1`.
//!   - SQLite:   a second handle opened `read_only(true)` (file-level, unforgeable).

use sqlx::mysql::{MySqlConnectOptions, MySqlPool, MySqlPoolOptions, MySqlSslMode};
use sqlx::postgres::{PgConnectOptions, PgPool, PgPoolOptions, PgSslMode};
use sqlx::sqlite::{SqliteConnectOptions, SqlitePool, SqlitePoolOptions};
use sqlx::Executor;

use crate::error::{AppError, AppResult};
use crate::model::{ConnectionProfile, Engine};

use super::providers;

const MAX_CONNS: u32 = 5;

/// A live sqlx pool for one of the three supported engines. Cheap to clone — each
/// inner sqlx pool is an `Arc` handle.
#[derive(Clone)]
pub enum DbPool {
    Postgres(PgPool),
    Mysql(MySqlPool),
    Sqlite(SqlitePool),
}

impl DbPool {
    /// `SELECT 1` liveness probe.
    pub async fn ping(&self) -> AppResult<()> {
        match self {
            DbPool::Postgres(p) => {
                sqlx::query("SELECT 1").execute(p).await?;
            }
            DbPool::Mysql(p) => {
                sqlx::query("SELECT 1").execute(p).await?;
            }
            DbPool::Sqlite(p) => {
                sqlx::query("SELECT 1").execute(p).await?;
            }
        }
        Ok(())
    }
}

/// An open connection: read/write and read-only pools (each `DbPool` variant is
/// self-describing about its engine). Cheap to clone (each pool is an `Arc` handle)
/// so commands can pull a handle out of the shared map without holding the lock
/// across an `.await`.
///
/// Field names (`read_pool` = the L2-enforced read-only pool, `write_pool` = the
/// read/write pool) are the executor's contract; `DbPool` is also re-exported as
/// `connection::Pool` for that module.
#[derive(Clone)]
pub struct LiveConnection {
    /// L2-enforced read-only pool. Reads and read previews route through this.
    pub read_pool: DbPool,
    /// Read/write pool. Approved writes and exec-rollback previews use this.
    pub write_pool: DbPool,
    /// True for PlanetScale/Vitess — introspection must skip FK metadata.
    pub skip_fk_metadata: bool,
}

impl LiveConnection {
    /// The read-only pool. Reads and all read previews route through this.
    pub fn ro(&self) -> &DbPool {
        &self.read_pool
    }

    /// `SELECT 1` against the live server.
    pub async fn test(&self) -> AppResult<()> {
        self.write_pool.ping().await
    }
}

/// Build both pools for a profile. `secret` is the password (or, for a
/// connection-string secret, the password component); it is never stored here.
pub async fn connect(profile: &ConnectionProfile, secret: &str) -> AppResult<LiveConnection> {
    let skip_fk_metadata = providers::skip_fk_metadata(profile);
    let acquire = providers::connect_timeout(profile);

    let (write_pool, read_pool) = match profile.engine {
        Engine::Postgres => {
            let base = PgConnectOptions::new()
                .host(&profile.host)
                .port(profile.port)
                .database(&profile.database)
                .username(&profile.username)
                .password(secret)
                .ssl_mode(pg_ssl_mode(&profile.sslmode)?);
            let base = providers::apply_pg_tuning(profile, base);

            let rw = PgPoolOptions::new()
                .max_connections(MAX_CONNS)
                .acquire_timeout(acquire)
                .connect_with(base.clone())
                .await?;

            let ro = PgPoolOptions::new()
                .max_connections(MAX_CONNS)
                .acquire_timeout(acquire)
                .after_connect(|conn, _meta| {
                    Box::pin(async move {
                        conn.execute("SET default_transaction_read_only = on").await?;
                        Ok(())
                    })
                })
                .connect_with(base)
                .await?;

            (DbPool::Postgres(rw), DbPool::Postgres(ro))
        }
        Engine::Mysql => {
            let base = MySqlConnectOptions::new()
                .host(&profile.host)
                .port(profile.port)
                .database(&profile.database)
                .username(&profile.username)
                .password(secret)
                .ssl_mode(mysql_ssl_mode(&profile.sslmode)?);
            let base = providers::apply_mysql_tuning(profile, base);

            let rw = MySqlPoolOptions::new()
                .max_connections(MAX_CONNS)
                .acquire_timeout(acquire)
                .connect_with(base.clone())
                .await?;

            let ro = MySqlPoolOptions::new()
                .max_connections(MAX_CONNS)
                .acquire_timeout(acquire)
                .after_connect(|conn, _meta| {
                    Box::pin(async move {
                        // Fail CLOSED: the read pool must be genuinely read-only. Try the
                        // modern variable, then the legacy MariaDB name; if neither exists,
                        // reject the connection rather than hand back a writable read pool.
                        if conn
                            .execute("SET SESSION transaction_read_only = 1")
                            .await
                            .is_err()
                            && conn
                                .execute("SET SESSION tx_read_only = 1")
                                .await
                                .is_err()
                        {
                            return Err(sqlx::Error::Configuration(
                                "read-only pool: server accepts neither `transaction_read_only` \
                                 nor `tx_read_only` — refusing a silently writable read pool"
                                    .into(),
                            ));
                        }
                        Ok(())
                    })
                })
                .connect_with(base)
                .await?;

            (DbPool::Mysql(rw), DbPool::Mysql(ro))
        }
        Engine::Sqlite => {
            // For SQLite the file path lives in `database`; host/port/user unused.
            let path = &profile.database;
            let rw_opts = SqliteConnectOptions::new()
                .filename(path)
                .create_if_missing(false);
            let rw = SqlitePoolOptions::new()
                .max_connections(MAX_CONNS)
                .connect_with(rw_opts)
                .await?;

            // Unforgeable file-level read-only handle.
            let ro_opts = SqliteConnectOptions::new().filename(path).read_only(true);
            let ro = SqlitePoolOptions::new()
                .max_connections(MAX_CONNS)
                .connect_with(ro_opts)
                .await?;

            (DbPool::Sqlite(rw), DbPool::Sqlite(ro))
        }
    };

    Ok(LiveConnection {
        read_pool,
        write_pool,
        skip_fk_metadata,
    })
}

// Fail CLOSED on unknown sslmode: a typo like "verrify-full" must NOT silently
// downgrade to a non-verifying mode. Trim + lowercase; empty means "unspecified"
// and keeps the platform default; anything else unknown is a config error.
fn pg_ssl_mode(mode: &str) -> AppResult<PgSslMode> {
    Ok(match mode.trim().to_ascii_lowercase().as_str() {
        "" => PgSslMode::Prefer, // ponytail: empty = unspecified, not a typo
        "disable" => PgSslMode::Disable,
        "allow" => PgSslMode::Allow,
        "prefer" => PgSslMode::Prefer,
        "require" => PgSslMode::Require,
        "verify-ca" | "verify_ca" => PgSslMode::VerifyCa,
        "verify-full" | "verify_full" => PgSslMode::VerifyFull,
        other => {
            return Err(AppError::Config(format!(
                "unknown Postgres sslmode {other:?} — use disable/allow/prefer/require/verify-ca/verify-full"
            )))
        }
    })
}

fn mysql_ssl_mode(mode: &str) -> AppResult<MySqlSslMode> {
    Ok(match mode.trim().to_ascii_lowercase().as_str() {
        "" => MySqlSslMode::Preferred, // ponytail: empty = unspecified, not a typo
        "disable" | "disabled" => MySqlSslMode::Disabled,
        "prefer" | "preferred" => MySqlSslMode::Preferred,
        "require" | "required" => MySqlSslMode::Required,
        "verify-ca" | "verify_ca" => MySqlSslMode::VerifyCa,
        "verify-identity" | "verify_identity" | "verify-full" => MySqlSslMode::VerifyIdentity,
        other => {
            return Err(AppError::Config(format!(
                "unknown MySQL sslmode {other:?} — use disabled/preferred/required/verify-ca/verify-identity"
            )))
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pg_sslmode_documented_values_ok() {
        assert!(matches!(pg_ssl_mode("verify-full"), Ok(PgSslMode::VerifyFull)));
        // trailing space / mixed case still resolve, not error
        assert!(matches!(pg_ssl_mode("  Require "), Ok(PgSslMode::Require)));
    }

    #[test]
    fn pg_sslmode_unknown_errors() {
        // typo must fail closed, never silently downgrade to Prefer
        assert!(pg_ssl_mode("verrify-full").is_err());
    }

    #[test]
    fn mysql_sslmode_documented_values_ok() {
        assert!(matches!(mysql_ssl_mode("VERIFY-IDENTITY"), Ok(MySqlSslMode::VerifyIdentity)));
    }

    #[test]
    fn mysql_sslmode_unknown_errors() {
        assert!(mysql_ssl_mode("prefered").is_err());
    }
}
