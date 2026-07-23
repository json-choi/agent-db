//! Catalog V2 DTOs shared by introspection, CLI, ERD, DDL, and table editing.

use chrono::{DateTime, Utc};
use serde::de::Error as _;
use serde::{Deserialize, Deserializer, Serialize};
use thiserror::Error;
use uuid::Uuid;

pub const CATALOG_SCHEMA_VERSION: u32 = 2;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DatabaseEngine {
    Postgres,
    Mysql,
    Sqlite,
    Mongodb,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CatalogSnapshot {
    schema_version: u32,
    connection_id: Uuid,
    engine: DatabaseEngine,
    database: String,
    captured_at: DateTime<Utc>,
    fingerprint: String,
    #[serde(default)]
    namespaces: Vec<Namespace>,
    #[serde(default)]
    relations: Vec<Relation>,
    #[serde(default)]
    routines: Vec<Routine>,
    #[serde(default)]
    other_objects: Vec<DatabaseObject>,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct CatalogContents {
    pub namespaces: Vec<Namespace>,
    pub relations: Vec<Relation>,
    pub routines: Vec<Routine>,
    pub other_objects: Vec<DatabaseObject>,
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum CatalogValidationError {
    #[error("unsupported catalog schema version {actual}; expected {expected}")]
    SchemaVersion { actual: u32, expected: u32 },
    #[error("catalog fingerprint must be exactly 64 lowercase hexadecimal characters")]
    Fingerprint,
    #[error("catalog database name cannot be empty")]
    EmptyDatabase,
}

impl CatalogSnapshot {
    pub fn new(
        connection_id: Uuid,
        engine: DatabaseEngine,
        database: impl Into<String>,
        captured_at: DateTime<Utc>,
        fingerprint: impl Into<String>,
        contents: CatalogContents,
    ) -> Result<Self, CatalogValidationError> {
        let snapshot = Self {
            schema_version: CATALOG_SCHEMA_VERSION,
            connection_id,
            engine,
            database: database.into(),
            captured_at,
            fingerprint: fingerprint.into(),
            namespaces: contents.namespaces,
            relations: contents.relations,
            routines: contents.routines,
            other_objects: contents.other_objects,
        };
        snapshot.validate()?;
        Ok(snapshot)
    }

    pub fn validate(&self) -> Result<(), CatalogValidationError> {
        if self.schema_version != CATALOG_SCHEMA_VERSION {
            return Err(CatalogValidationError::SchemaVersion {
                actual: self.schema_version,
                expected: CATALOG_SCHEMA_VERSION,
            });
        }
        if self.database.trim().is_empty() {
            return Err(CatalogValidationError::EmptyDatabase);
        }
        if !valid_fingerprint(&self.fingerprint) {
            return Err(CatalogValidationError::Fingerprint);
        }
        Ok(())
    }

    pub const fn schema_version(&self) -> u32 {
        self.schema_version
    }

    pub const fn connection_id(&self) -> Uuid {
        self.connection_id
    }

    pub const fn engine(&self) -> DatabaseEngine {
        self.engine
    }

    pub fn database(&self) -> &str {
        &self.database
    }

    pub const fn captured_at(&self) -> DateTime<Utc> {
        self.captured_at
    }

    pub fn fingerprint(&self) -> &str {
        &self.fingerprint
    }

    pub fn namespaces(&self) -> &[Namespace] {
        &self.namespaces
    }

    pub fn relations(&self) -> &[Relation] {
        &self.relations
    }

    pub fn routines(&self) -> &[Routine] {
        &self.routines
    }

    pub fn other_objects(&self) -> &[DatabaseObject] {
        &self.other_objects
    }
}

fn valid_fingerprint(fingerprint: &str) -> bool {
    fingerprint.len() == 64
        && fingerprint
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct WireCatalogSnapshot {
    schema_version: u32,
    connection_id: Uuid,
    engine: DatabaseEngine,
    database: String,
    captured_at: DateTime<Utc>,
    fingerprint: String,
    #[serde(default)]
    namespaces: Vec<Namespace>,
    #[serde(default)]
    relations: Vec<Relation>,
    #[serde(default)]
    routines: Vec<Routine>,
    #[serde(default)]
    other_objects: Vec<DatabaseObject>,
}

impl<'de> Deserialize<'de> for CatalogSnapshot {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let wire = WireCatalogSnapshot::deserialize(deserializer)?;
        let snapshot = Self {
            schema_version: wire.schema_version,
            connection_id: wire.connection_id,
            engine: wire.engine,
            database: wire.database,
            captured_at: wire.captured_at,
            fingerprint: wire.fingerprint,
            namespaces: wire.namespaces,
            relations: wire.relations,
            routines: wire.routines,
            other_objects: wire.other_objects,
        };
        snapshot.validate().map_err(D::Error::custom)?;
        Ok(snapshot)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Namespace {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub comment: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ObjectKind {
    Table,
    View,
    MaterializedView,
    Routine,
    Sequence,
    Type,
    Trigger,
    Other,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ObjectRef {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub catalog: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub namespace: Option<String>,
    pub name: String,
    pub kind: ObjectKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub native_id: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NormalizedTypeFamily {
    Boolean,
    Integer,
    Decimal,
    Float,
    Text,
    Binary,
    Json,
    Date,
    Time,
    Timestamp,
    Uuid,
    Array,
    Document,
    Other,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Column {
    pub name: String,
    pub ordinal: u32,
    pub native_type: String,
    pub type_family: NormalizedTypeFamily,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub length: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub precision: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scale: Option<u32>,
    pub nullable: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_expression: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub generated_expression: Option<String>,
    pub identity: bool,
    pub auto_increment: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub collation: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub comment: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sensitivity: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConstraintKind {
    Primary,
    Unique,
    Foreign,
    Check,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Constraint {
    pub name: String,
    pub kind: ConstraintKind,
    #[serde(default)]
    pub columns: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub referenced_relation: Option<ObjectRef>,
    #[serde(default)]
    pub referenced_columns: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub check_expression: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub update_action: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delete_action: Option<String>,
    pub deferrable: bool,
    pub validated: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SortDirection {
    Asc,
    Desc,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct IndexKey {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub column: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expression: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub direction: Option<SortDirection>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Index {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub method: Option<String>,
    #[serde(default)]
    pub keys: Vec<IndexKey>,
    #[serde(default)]
    pub included_columns: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub predicate: Option<String>,
    pub unique: bool,
    pub valid: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Relation {
    pub object: ObjectRef,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub comment: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub row_estimate: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub partition_parent: Option<ObjectRef>,
    #[serde(default)]
    pub partition_children: Vec<ObjectRef>,
    #[serde(default)]
    pub columns: Vec<Column>,
    #[serde(default)]
    pub constraints: Vec<Constraint>,
    #[serde(default)]
    pub indexes: Vec<Index>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Routine {
    pub object: ObjectRef,
    #[serde(default)]
    pub arguments: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub return_type: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub language: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub comment: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DatabaseObject {
    pub object: ObjectRef,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub comment: Option<String>,
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn valid_snapshot() -> CatalogSnapshot {
        CatalogSnapshot::new(
            Uuid::from_u128(1),
            DatabaseEngine::Postgres,
            "app",
            Utc::now(),
            "a".repeat(64),
            CatalogContents::default(),
        )
        .unwrap()
    }

    #[test]
    fn constructor_fixes_schema_version_and_validates_fingerprint() {
        let snapshot = valid_snapshot();
        assert_eq!(snapshot.schema_version(), CATALOG_SCHEMA_VERSION);
        assert_eq!(snapshot.fingerprint(), "a".repeat(64));
        assert!(CatalogSnapshot::new(
            Uuid::from_u128(1),
            DatabaseEngine::Postgres,
            "app",
            Utc::now(),
            "A".repeat(64),
            CatalogContents::default(),
        )
        .is_err());
    }

    #[test]
    fn deserialization_rejects_old_or_future_schema_versions() {
        let mut value = serde_json::to_value(valid_snapshot()).unwrap();
        value["schemaVersion"] = json!(1);
        assert!(serde_json::from_value::<CatalogSnapshot>(value).is_err());
    }
}
