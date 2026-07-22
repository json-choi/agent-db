//! MongoDB document-database adapter. Deliberately separate from the sqlx pool
//! stack (`connection::pool`): MongoDB has no server-enforced read-only session
//! equivalent to L2, so safety here is structural — data access happens ONLY
//! through the typed [`crate::model::DocumentQuery`] API in [`query`], which
//! calls the driver's `find`/`aggregate`/`count_documents` and never
//! `run_command`. Users are still advised to grant the DB account the `read`
//! role; the client allowlist is not presented as a substitute for it.

pub mod introspect;
pub mod query;

use mongodb::bson::doc;
use mongodb::options::ClientOptions;
use mongodb::Client;

use crate::connection::providers;
use crate::error::{AppError, AppResult};
use crate::model::ConnectionProfile;

/// A live MongoDB client bound to the profile's database. Cheap to clone —
/// `Client` is an `Arc` handle over its own connection pool.
#[derive(Clone)]
pub struct MongoConnection {
    client: Client,
    db_name: String,
}

impl MongoConnection {
    /// The profile's database handle. All reads and introspection scope to it.
    pub fn database(&self) -> mongodb::Database {
        self.client.database(&self.db_name)
    }

    /// Liveness probe. `ping` is a stateless no-op command — the sole
    /// `run_command` in this module; the query path never uses raw commands.
    pub async fn ping(&self) -> AppResult<()> {
        self.database().run_command(doc! { "ping": 1 }).await?;
        Ok(())
    }
}

/// Open (and verify with a ping) a MongoDB connection for `profile`.
pub(crate) async fn connect(
    profile: &ConnectionProfile,
    secret: &str,
) -> AppResult<MongoConnection> {
    let uri = build_uri(profile, secret)?;
    let mut options = ClientOptions::parse(&uri)
        .await
        .map_err(|e| sanitize(e, secret))?;
    options.app_name = Some("DopeDB".into());
    options.server_selection_timeout = Some(providers::connect_timeout(profile));
    let client = Client::with_options(options).map_err(|e| sanitize(e, secret))?;
    let conn = MongoConnection {
        client,
        db_name: profile.database.clone(),
    };
    // The client is lazy; ping so connect fails eagerly like the sqlx pools do.
    conn.ping().await?;
    Ok(conn)
}

/// Assemble a `mongodb://` / `mongodb+srv://` URI from the decomposed profile.
///
/// Conventions (no store schema change needed):
/// - `extra_params["srv"] == "true"` selects the `mongodb+srv` scheme (port unused).
/// - `host` passes through verbatim, so a comma-separated replica-set list or an
///   explicit `host:port` works; the profile port is appended only to a bare host.
/// - Every other `extra_params` entry becomes a URI option (`authSource`,
///   `replicaSet`, `tls`, `tlsCAFile`, …) — the official driver parses/validates.
fn build_uri(profile: &ConnectionProfile, secret: &str) -> AppResult<String> {
    let srv = profile
        .extra_params
        .get("srv")
        .is_some_and(|v| v.trim().eq_ignore_ascii_case("true"));
    let host = profile.host.trim();
    let host = if host.is_empty() { "localhost" } else { host };

    let database = profile.database.trim();
    if database.is_empty() {
        return Err(AppError::Config(
            "MongoDB connections need a database name".into(),
        ));
    }
    if database.contains(['/', '\\', '?', '#', '@', ' ']) {
        return Err(AppError::Config(format!(
            "invalid MongoDB database name {database:?}"
        )));
    }

    let scheme = if srv { "mongodb+srv" } else { "mongodb" };
    let authority = if srv || host.contains(',') || host.contains(':') {
        host.to_string()
    } else {
        format!("{host}:{}", profile.port)
    };

    let mut uri = format!("{scheme}://");
    if !profile.username.is_empty() {
        uri.push_str(&encode_component(&profile.username));
        if !secret.is_empty() {
            uri.push(':');
            uri.push_str(&encode_component(secret));
        }
        uri.push('@');
    }
    uri.push_str(&authority);
    uri.push('/');
    uri.push_str(database);

    let mut params: Vec<(&String, &String)> = profile
        .extra_params
        .iter()
        .filter(|(k, _)| k.as_str() != "srv")
        .collect();
    params.sort();
    for (i, (k, v)) in params.iter().enumerate() {
        uri.push(if i == 0 { '?' } else { '&' });
        uri.push_str(&encode_component(k));
        uri.push('=');
        uri.push_str(&encode_component(v));
    }
    Ok(uri)
}

/// Percent-encode everything outside RFC 3986 unreserved characters.
fn encode_component(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for byte in s.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                out.push(byte as char)
            }
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

/// Driver config errors can echo parts of the connection string. The secret must
/// never reach logs, the UI, or the append-only audit chain — scrub both its raw
/// and percent-encoded spellings before the message leaves this module.
fn sanitize(e: mongodb::error::Error, secret: &str) -> AppError {
    AppError::Config(format!(
        "MongoDB connection failed: {}",
        scrub(e.to_string(), secret)
    ))
}

fn scrub(mut msg: String, secret: &str) -> String {
    if !secret.is_empty() {
        msg = msg.replace(secret, "***");
        msg = msg.replace(&encode_component(secret), "***");
    }
    msg
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use uuid::Uuid;

    use crate::model::{Engine, Provider};

    use super::*;

    fn profile() -> ConnectionProfile {
        ConnectionProfile {
            id: Uuid::new_v4(),
            name: "mongo".into(),
            engine: Engine::Mongodb,
            provider: Provider::Generic,
            driver_id: None,
            host: "localhost".into(),
            port: 27017,
            database: "app".into(),
            username: "reader".into(),
            sslmode: "prefer".into(),
            extra_params: HashMap::new(),
            readonly_default: true,
            allow_writes: false,
            secret_ref: None,
            env: None,
            schema_group: None,
            workspace_access: crate::model::WorkspaceConnectionAccess::Local,
        }
    }

    #[test]
    fn builds_a_plain_uri_with_encoded_credentials() {
        // Fixture password is assembled from parts so secret scanners don't
        // mistake the fake credential URI for a real one.
        let fake_password = ["not", "a", "real", "p@ss/w:rd"].join("-");
        let uri = build_uri(&profile(), &fake_password).unwrap();
        let expected = format!(
            "mongodb://reader:{}@localhost:27017/app",
            encode_component(&fake_password)
        );
        assert_eq!(uri, expected);
        assert!(
            expected.contains("p%40ss%2Fw%3Ard"),
            "special chars must be percent-encoded"
        );
    }

    #[test]
    fn srv_scheme_drops_the_port_and_keeps_options() {
        let mut p = profile();
        p.host = "cluster0.example.mongodb.net".into();
        p.extra_params.insert("srv".into(), "true".into());
        p.extra_params.insert("authSource".into(), "admin".into());
        p.extra_params.insert("replicaSet".into(), "rs0".into());
        let uri = build_uri(&p, "").unwrap();
        assert_eq!(
            uri,
            "mongodb+srv://reader@cluster0.example.mongodb.net/app?authSource=admin&replicaSet=rs0"
        );
    }

    #[test]
    fn replica_set_host_list_passes_through_without_the_port() {
        let mut p = profile();
        p.host = "db1.example.com:27017,db2.example.com:27018".into();
        p.username = String::new();
        let uri = build_uri(&p, "").unwrap();
        assert_eq!(
            uri,
            "mongodb://db1.example.com:27017,db2.example.com:27018/app"
        );
    }

    #[test]
    fn rejects_a_missing_or_malformed_database_name() {
        let mut empty = profile();
        empty.database = "  ".into();
        assert!(build_uri(&empty, "").is_err());

        let mut bad = profile();
        bad.database = "a/b".into();
        assert!(build_uri(&bad, "").is_err());
    }

    #[test]
    fn scrub_removes_raw_and_percent_encoded_secrets() {
        // Same scanner-hygiene rule: the fake secret never appears verbatim
        // inside a credential-shaped URI literal.
        let fake_secret = ["s3cr", "t"].join("@");
        let msg = scrub(
            format!(
                "bad uri mongodb://u:{}@h/db (password: {fake_secret})",
                encode_component(&fake_secret)
            ),
            &fake_secret,
        );
        assert!(
            !msg.contains("s3cr"),
            "scrubbed message leaked the secret: {msg}"
        );
        assert!(msg.contains("***"));
    }
}
