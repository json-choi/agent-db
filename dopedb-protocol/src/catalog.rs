//! Catalog V2 DTOs shared by introspection, CLI, ERD, DDL, and table editing.

use chrono::{DateTime, Utc};
use serde::de::Error as _;
use serde::{Deserialize, Deserializer, Serialize};
use sha2::{Digest, Sha256};
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
    /// Build a canonical snapshot and derive its SHA-256 fingerprint from schema
    /// metadata only. Capture time and connection id are excluded so collecting the
    /// same database metadata again produces the same fingerprint.
    pub fn capture(
        connection_id: Uuid,
        engine: DatabaseEngine,
        database: impl Into<String>,
        captured_at: DateTime<Utc>,
        mut contents: CatalogContents,
    ) -> Result<Self, CatalogValidationError> {
        canonicalize_contents(&mut contents);
        let database = database.into();
        let fingerprint = catalog_fingerprint(engine, &database, &contents);
        Self::new(
            connection_id,
            engine,
            database,
            captured_at,
            fingerprint,
            contents,
        )
    }

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

    /// Recompute the canonical metadata fingerprint without trusting the wire/cache
    /// value stored in this snapshot.
    pub fn canonical_fingerprint(&self) -> String {
        let mut contents = CatalogContents {
            namespaces: self.namespaces.clone(),
            relations: self.relations.clone(),
            routines: self.routines.clone(),
            other_objects: self.other_objects.clone(),
        };
        canonicalize_contents(&mut contents);
        catalog_fingerprint(self.engine, &self.database, &contents)
    }

    pub fn has_canonical_fingerprint(&self) -> bool {
        self.fingerprint == self.canonical_fingerprint()
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

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct CatalogFingerprintPayload<'a> {
    schema_version: u32,
    engine: DatabaseEngine,
    database: &'a str,
    namespaces: &'a [Namespace],
    relations: &'a [Relation],
    routines: &'a [Routine],
    other_objects: &'a [DatabaseObject],
}

fn catalog_fingerprint(
    engine: DatabaseEngine,
    database: &str,
    contents: &CatalogContents,
) -> String {
    let payload = CatalogFingerprintPayload {
        schema_version: CATALOG_SCHEMA_VERSION,
        engine,
        database,
        namespaces: &contents.namespaces,
        relations: &contents.relations,
        routines: &contents.routines,
        other_objects: &contents.other_objects,
    };
    // These DTOs contain no fallible custom serializers, so serialization failure is
    // an internal invariant violation rather than input-dependent behavior.
    let canonical =
        serde_json::to_vec(&payload).expect("Catalog fingerprint payload must always serialize");
    lower_hex(&Sha256::digest(canonical))
}

fn lower_hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        output.push(HEX[(byte >> 4) as usize] as char);
        output.push(HEX[(byte & 0x0f) as usize] as char);
    }
    output
}

fn canonicalize_contents(contents: &mut CatalogContents) {
    contents.namespaces.sort_by(|left, right| {
        (left.name.as_str(), left.comment.as_deref())
            .cmp(&(right.name.as_str(), right.comment.as_deref()))
    });
    for relation in &mut contents.relations {
        relation.columns.sort_by(|left, right| {
            (left.ordinal, &left.name)
                .cmp(&(right.ordinal, &right.name))
                .then_with(|| compare_serialized(left, right))
        });
        relation.constraints.sort_by(|left, right| {
            (constraint_kind_rank(left.kind), left.name.as_str())
                .cmp(&(constraint_kind_rank(right.kind), right.name.as_str()))
                .then_with(|| compare_serialized(left, right))
        });
        relation.indexes.sort_by(|left, right| {
            left.name
                .cmp(&right.name)
                .then_with(|| compare_serialized(left, right))
        });
        relation.partition_children.sort_by(compare_object_ref);
    }
    contents.relations.sort_by(|left, right| {
        compare_object_ref(&left.object, &right.object)
            .then_with(|| compare_serialized(left, right))
    });
    contents.routines.sort_by(|left, right| {
        compare_object_ref(&left.object, &right.object)
            .then_with(|| compare_serialized(left, right))
    });
    contents.other_objects.sort_by(|left, right| {
        compare_object_ref(&left.object, &right.object)
            .then_with(|| compare_serialized(left, right))
    });
}

fn compare_serialized<T: Serialize>(left: &T, right: &T) -> std::cmp::Ordering {
    // Catalog DTO fields serialize infallibly. A deterministic empty fallback keeps
    // this comparator total even if a future custom field serializer can fail.
    serde_json::to_vec(left)
        .unwrap_or_default()
        .cmp(&serde_json::to_vec(right).unwrap_or_default())
}

fn compare_object_ref(left: &ObjectRef, right: &ObjectRef) -> std::cmp::Ordering {
    (
        left.catalog.as_deref(),
        left.namespace.as_deref(),
        left.name.as_str(),
        object_kind_rank(left.kind),
        left.native_id.as_deref(),
    )
        .cmp(&(
            right.catalog.as_deref(),
            right.namespace.as_deref(),
            right.name.as_str(),
            object_kind_rank(right.kind),
            right.native_id.as_deref(),
        ))
}

const fn object_kind_rank(kind: ObjectKind) -> u8 {
    match kind {
        ObjectKind::Table => 0,
        ObjectKind::View => 1,
        ObjectKind::MaterializedView => 2,
        ObjectKind::Routine => 3,
        ObjectKind::Sequence => 4,
        ObjectKind::Type => 5,
        ObjectKind::Trigger => 6,
        ObjectKind::Other => 7,
    }
}

const fn constraint_kind_rank(kind: ConstraintKind) -> u8 {
    match kind {
        ConstraintKind::Primary => 0,
        ConstraintKind::Unique => 1,
        ConstraintKind::Foreign => 2,
        ConstraintKind::Check => 3,
    }
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
    /// Engine-native routine kind (`function`, `procedure`, ...), when available.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub native_kind: Option<String>,
    #[serde(default)]
    pub arguments: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub return_type: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub language: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub comment: Option<String>,
    /// Lossless compact metadata used by object explorers.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    /// Owning relation for objects such as table triggers.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DatabaseObject {
    pub object: ObjectRef,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub native_kind: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub comment: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent: Option<String>,
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn valid_snapshot() -> CatalogSnapshot {
        CatalogSnapshot::capture(
            Uuid::from_u128(1),
            DatabaseEngine::Postgres,
            "app",
            Utc::now(),
            CatalogContents::default(),
        )
        .unwrap()
    }

    #[test]
    fn constructor_fixes_schema_version_and_validates_fingerprint() {
        let snapshot = valid_snapshot();
        assert_eq!(snapshot.schema_version(), CATALOG_SCHEMA_VERSION);
        assert!(snapshot.has_canonical_fingerprint());
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
    fn canonical_fingerprint_is_stable_across_collection_and_capture_order() {
        let routine_ref = ObjectRef {
            catalog: None,
            namespace: Some("public".into()),
            name: "calculate".into(),
            kind: ObjectKind::Routine,
            native_id: None,
        };
        let overloaded = vec![
            Routine {
                object: routine_ref.clone(),
                native_kind: Some("function".into()),
                arguments: vec!["text".into()],
                return_type: Some("text".into()),
                language: Some("sql".into()),
                comment: None,
                detail: Some("(text)".into()),
                parent: None,
            },
            Routine {
                object: routine_ref,
                native_kind: Some("function".into()),
                arguments: vec!["integer".into()],
                return_type: Some("integer".into()),
                language: Some("sql".into()),
                comment: None,
                detail: Some("(integer)".into()),
                parent: None,
            },
        ];
        let first = CatalogContents {
            namespaces: vec![
                Namespace {
                    name: "zeta".into(),
                    comment: None,
                },
                Namespace {
                    name: "alpha".into(),
                    comment: Some("first".into()),
                },
            ],
            routines: overloaded,
            ..CatalogContents::default()
        };
        let mut second = first.clone();
        second.namespaces.reverse();
        second.routines.reverse();
        let first = CatalogSnapshot::capture(
            Uuid::from_u128(1),
            DatabaseEngine::Postgres,
            "app",
            Utc::now(),
            first,
        )
        .unwrap();
        let second = CatalogSnapshot::capture(
            Uuid::from_u128(2),
            DatabaseEngine::Postgres,
            "app",
            Utc::now() + chrono::Duration::seconds(1),
            second,
        )
        .unwrap();
        assert_eq!(first.fingerprint(), second.fingerprint());
        assert_eq!(first.namespaces(), second.namespaces());
        assert_eq!(first.routines(), second.routines());
    }

    #[test]
    fn deserialization_rejects_old_or_future_schema_versions() {
        let mut value = serde_json::to_value(valid_snapshot()).unwrap();
        value["schemaVersion"] = json!(1);
        assert!(serde_json::from_value::<CatalogSnapshot>(value).is_err());
    }
}
