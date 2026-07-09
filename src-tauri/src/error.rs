//! Typed error spine. Every fallible path in the crate returns [`AppError`], which
//! serializes to a `{ kind, message }` object so `#[tauri::command]` can hand a
//! structured error straight to the frontend.

use thiserror::Error;

pub type AppResult<T> = Result<T, AppError>;

#[derive(Debug, Error)]
pub enum AppError {
    /// Errors from the target-database drivers (sqlx).
    #[error("database error: {0}")]
    Db(#[from] sqlx::Error),

    /// An agent-facing operation failed or returned unusable output.
    #[error("agent error: {0}")]
    Agent(String),

    /// A safety-layer violation the DB or classifier rejected before execution.
    #[error("safety violation: {0}")]
    Safety(String),

    /// SQL parse/classification failure (L1). Treated as fail-safe (→ write).
    #[error("parse error: {0}")]
    Parse(#[from] sqlparser::parser::ParserError),

    /// OS credential-store access failure (macOS Keychain / Windows Credential Manager).
    #[error("credential store error: {0}")]
    Keychain(#[from] keyring::Error),

    #[error("io error: {0}")]
    Io(#[from] std::io::Error),

    #[error("serialization error: {0}")]
    Serialization(#[from] serde_json::Error),

    /// Malformed or missing configuration (connection profile, agent config, etc.).
    #[error("config error: {0}")]
    Config(String),

    #[error("not found: {0}")]
    NotFound(String),

    /// The safety gate blocked an action; `reason` is shown verbatim in the UI.
    #[error("blocked: {reason}")]
    Blocked { reason: String },
}

impl AppError {
    /// Stable machine-readable discriminant for the frontend to switch on.
    pub fn kind(&self) -> &'static str {
        match self {
            AppError::Db(_) => "db",
            AppError::Agent(_) => "agent",
            AppError::Safety(_) => "safety",
            AppError::Parse(_) => "parse",
            AppError::Keychain(_) => "keychain",
            AppError::Io(_) => "io",
            AppError::Serialization(_) => "serialization",
            AppError::Config(_) => "config",
            AppError::NotFound(_) => "notFound",
            AppError::Blocked { .. } => "blocked",
        }
    }
}

// Serialize to `{ kind, message }` so JS gets a typed, switchable error object.
impl serde::Serialize for AppError {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        use serde::ser::SerializeStruct;
        let mut st = serializer.serialize_struct("AppError", 2)?;
        st.serialize_field("kind", self.kind())?;
        st.serialize_field("message", &self.to_string())?;
        st.end()
    }
}
