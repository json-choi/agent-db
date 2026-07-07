//! Per-provider connection-string normalization. We do NOT bundle any external CA files — a custom CA can be
//! supplied per connection via `extra_params["sslrootcert"]` (documented, not shipped).

use std::time::Duration;

use sqlx::mysql::{MySqlConnectOptions, MySqlSslMode};
use sqlx::postgres::PgConnectOptions;

use crate::model::ConnectionProfile;

/// Managed cloud providers we apply specific tuning for. `Generic` covers
/// self-hosted / unknown hosts (no special handling beyond the profile fields).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Provider {
    /// Supabase Supavisor pooler (`*.pooler.supabase.com`), txn mode on 6543.
    SupabasePooler,
    /// Neon serverless Postgres (`*.neon.tech`), scale-to-zero cold starts.
    Neon,
    /// PlanetScale MySQL over Vitess (`*.psdb.cloud`) — FK metadata unreliable.
    PlanetScaleMysql,
    /// AWS RDS (`*.rds.amazonaws.com`) — needs the RDS CA for verify-full.
    Rds,
    Generic,
}

/// Classify a profile by host. Cheap substring match — hosts are provider-fixed.
pub fn detect(p: &ConnectionProfile) -> Provider {
    let h = p.host.to_ascii_lowercase();
    if h.contains("pooler.supabase.com") {
        Provider::SupabasePooler
    } else if h.contains("neon.tech") {
        Provider::Neon
    } else if h.contains("psdb.cloud") {
        Provider::PlanetScaleMysql
    } else if h.contains("rds.amazonaws.com") {
        Provider::Rds
    } else {
        Provider::Generic
    }
}

/// PlanetScale/Vitess is sharded — its FK metadata in `information_schema` is
/// unreliable, so introspection skips it.
pub fn skip_fk_metadata(p: &ConnectionProfile) -> bool {
    matches!(detect(p), Provider::PlanetScaleMysql)
}

/// Pool acquire timeout. Neon scales to zero, so cold connects need slack.
pub fn connect_timeout(p: &ConnectionProfile) -> Duration {
    match detect(p) {
        Provider::Neon => Duration::from_secs(30),
        _ => Duration::from_secs(15),
    }
}

/// Apply Postgres per-provider tuning to freshly-built connect options.
pub fn apply_pg_tuning(p: &ConnectionProfile, mut opts: PgConnectOptions) -> PgConnectOptions {
    if detect(p) == Provider::SupabasePooler {
        // Supavisor transaction mode multiplexes server-side prepared statements;
        // client-side statement caching breaks connections → disable it.
        opts = opts.statement_cache_capacity(0);
    }
    // Neon negotiates channel_binding via SCRAM automatically; its cold-start
    // penalty is handled by connect_timeout(), not an option here.

    // Custom CA (e.g. RDS global CA, ISRG roots) — user-supplied, never bundled.
    if let Some(ca) = p.extra_params.get("sslrootcert") {
        opts = opts.ssl_root_cert(ca);
    }
    opts
}

/// Apply MySQL per-provider tuning.
pub fn apply_mysql_tuning(p: &ConnectionProfile, mut opts: MySqlConnectOptions) -> MySqlConnectOptions {
    if detect(p) == Provider::PlanetScaleMysql {
        // PlanetScale requires TLS with identity verification.
        opts = opts.ssl_mode(MySqlSslMode::VerifyIdentity);
    }
    if let Some(ca) = p.extra_params.get("sslrootcert") {
        opts = opts.ssl_ca(ca);
    }
    opts
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Engine;
    use std::collections::HashMap;
    use uuid::Uuid;

    fn profile(host: &str) -> ConnectionProfile {
        ConnectionProfile {
            id: Uuid::new_v4(),
            name: "t".into(),
            engine: Engine::Postgres,
            host: host.into(),
            port: 5432,
            database: "db".into(),
            username: "u".into(),
            sslmode: "require".into(),
            extra_params: HashMap::new(),
            readonly_default: true,
            allow_writes: false,
            secret_ref: None,
            project_dir: None,
            env: None,
        }
    }

    #[test]
    fn detects_providers() {
        assert_eq!(detect(&profile("aws-0-us.pooler.supabase.com")), Provider::SupabasePooler);
        assert_eq!(detect(&profile("ep-x-pooler.us-east-2.aws.neon.tech")), Provider::Neon);
        assert_eq!(detect(&profile("xyz.connect.psdb.cloud")), Provider::PlanetScaleMysql);
        assert_eq!(detect(&profile("db.abc.rds.amazonaws.com")), Provider::Rds);
        assert_eq!(detect(&profile("localhost")), Provider::Generic);
        assert!(skip_fk_metadata(&profile("xyz.connect.psdb.cloud")));
        assert!(!skip_fk_metadata(&profile("localhost")));
    }
}
