//! Catalog V2 cache adapter.
//!
//! The existing UI/MCP catalog stays wire-compatible while persistence uses the
//! canonical protocol DTO. Every read is authorized and pinned before consulting
//! SQLite, and every write is compare-and-swap against that same pin.

use std::collections::{BTreeSet, HashSet};

use chrono::Utc;
use dopedb_protocol::catalog as v2;
use uuid::Uuid;

use crate::connection::{ConnectionAccess, ConnectionContext, ConnectionManager};
use crate::error::{AppError, AppResult};
use crate::model::{ConnectionProfile, Engine};
use crate::store::{CacheWriteOutcome, Store};

use super::{Catalog, Column, DatabaseObject, ForeignKey, Index, Table};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CatalogReadMode {
    /// Authorize online, return a current canonical snapshot when present, otherwise
    /// introspect live and write it through.
    CacheFirst,
    /// Always introspect the target database and do not mutate the cache.
    LiveNoCache,
    /// Delete the current scoped cache first, introspect live, then write through.
    Refresh,
}

/// Authorize the exact active scope and return only an already-persisted snapshot.
/// Agent startup uses this latency-bounded path so a cache miss never opens the
/// target database or waits for full introspection before spawning the CLI.
pub(crate) async fn load_cached_catalog(
    store: &Store,
    connections: &ConnectionManager,
    connection_id: Uuid,
) -> AppResult<Option<Catalog>> {
    let context = connections
        .pin(connection_id, ConnectionAccess::Read)
        .await?;
    Ok(store
        .get_catalog_if_current(context.pin())
        .await?
        .map(|snapshot| from_snapshot(&snapshot)))
}

pub(crate) async fn load_catalog(
    store: &Store,
    connections: &ConnectionManager,
    connection_id: Uuid,
    mode: CatalogReadMode,
) -> AppResult<Catalog> {
    let context = connections
        .pin(connection_id, ConnectionAccess::Read)
        .await?;

    if mode == CatalogReadMode::CacheFirst {
        if let Some(snapshot) = store.get_catalog_if_current(context.pin()).await? {
            return Ok(from_snapshot(&snapshot));
        }
    } else if mode == CatalogReadMode::Refresh {
        // The context's scope guard prevents a workspace/account switch between this
        // delete and the subsequent CAS write.
        store.clear_schema_cache(connection_id).await?;
    }

    introspect_and_maybe_store(store, context, mode).await
}

async fn introspect_and_maybe_store(
    store: &Store,
    context: ConnectionContext,
    mode: CatalogReadMode,
) -> AppResult<Catalog> {
    let lease = context.connect().await?;
    let catalog = super::introspect(lease.live()).await?;
    if mode == CatalogReadMode::LiveNoCache {
        return Ok(catalog);
    }
    if lease.pin().profile.database.trim().is_empty() {
        // Catalog V2 deliberately requires a stable database identity. Engines that
        // allow an omitted default database remain usable, but are not persisted
        // until that identity can be represented without inventing one.
        return Ok(catalog);
    }

    let snapshot = to_snapshot(&lease.pin().profile, &catalog)?;
    match store.put_catalog_if_current(lease.pin(), &snapshot).await? {
        CacheWriteOutcome::Stored | CacheWriteOutcome::NotPersisted => Ok(catalog),
        CacheWriteOutcome::Stale => Err(AppError::Blocked {
            reason: "workspace or connection access changed; retry schema loading".into(),
        }),
    }
}

fn to_snapshot(profile: &ConnectionProfile, catalog: &Catalog) -> AppResult<v2::CatalogSnapshot> {
    let namespaces = catalog
        .tables
        .iter()
        .filter_map(|table| table.schema.clone())
        .chain(
            catalog
                .objects
                .iter()
                .filter_map(|object| object.schema.clone()),
        )
        .collect::<BTreeSet<_>>()
        .into_iter()
        .map(|name| v2::Namespace {
            name,
            comment: None,
        })
        .collect();

    let relations = catalog
        .tables
        .iter()
        .map(table_to_relation)
        .collect::<Vec<_>>();
    let mut routines = Vec::new();
    let mut other_objects = Vec::new();
    for object in &catalog.objects {
        let object_ref = v2::ObjectRef {
            catalog: None,
            namespace: object.schema.clone(),
            name: object.name.clone(),
            kind: object_kind(&object.kind),
            native_id: None,
        };
        if matches!(object.kind.as_str(), "function" | "procedure") {
            routines.push(v2::Routine {
                object: v2::ObjectRef {
                    kind: v2::ObjectKind::Routine,
                    ..object_ref
                },
                native_kind: Some(object.kind.clone()),
                arguments: Vec::new(),
                return_type: None,
                language: None,
                comment: None,
                detail: object.detail.clone(),
                parent: object.parent.clone(),
            });
        } else {
            other_objects.push(v2::DatabaseObject {
                object: object_ref,
                native_kind: Some(object.kind.clone()),
                comment: None,
                detail: object.detail.clone(),
                parent: object.parent.clone(),
            });
        }
    }

    v2::CatalogSnapshot::capture(
        profile.id,
        engine(profile.engine),
        profile.database.clone(),
        Utc::now(),
        v2::CatalogContents {
            namespaces,
            relations,
            routines,
            other_objects,
        },
    )
    .map_err(|error| AppError::Config(format!("could not build Catalog V2: {error}")))
}

fn table_to_relation(table: &Table) -> v2::Relation {
    let primary_columns = table
        .columns
        .iter()
        .filter(|column| column.pk)
        .map(|column| column.name.clone())
        .collect::<Vec<_>>();
    let mut constraints = Vec::new();
    if !primary_columns.is_empty() {
        constraints.push(v2::Constraint {
            name: format!("pk_{}", table.name),
            kind: v2::ConstraintKind::Primary,
            columns: primary_columns,
            referenced_relation: None,
            referenced_columns: Vec::new(),
            check_expression: None,
            update_action: None,
            delete_action: None,
            deferrable: false,
            validated: true,
        });
    }
    let mut foreign_keys = table.foreign_keys.iter().collect::<Vec<_>>();
    foreign_keys.sort_by(|left, right| {
        (
            left.column.as_str(),
            left.references_schema.as_deref(),
            left.references_table.as_str(),
            left.references_column.as_str(),
        )
            .cmp(&(
                right.column.as_str(),
                right.references_schema.as_deref(),
                right.references_table.as_str(),
                right.references_column.as_str(),
            ))
    });
    constraints.extend(
        foreign_keys
            .into_iter()
            .enumerate()
            .map(|(index, foreign_key)| v2::Constraint {
                name: format!("fk_{}_{}_{}", table.name, foreign_key.column, index + 1),
                kind: v2::ConstraintKind::Foreign,
                columns: vec![foreign_key.column.clone()],
                referenced_relation: Some(v2::ObjectRef {
                    catalog: None,
                    namespace: foreign_key.references_schema.clone(),
                    name: foreign_key.references_table.clone(),
                    kind: v2::ObjectKind::Table,
                    native_id: None,
                }),
                referenced_columns: vec![foreign_key.references_column.clone()],
                check_expression: None,
                update_action: None,
                delete_action: None,
                deferrable: false,
                validated: true,
            }),
    );

    v2::Relation {
        object: v2::ObjectRef {
            catalog: None,
            namespace: table.schema.clone(),
            name: table.name.clone(),
            kind: object_kind(&table.kind),
            native_id: None,
        },
        comment: None,
        row_estimate: table.row_estimate,
        partition_parent: None,
        partition_children: Vec::new(),
        columns: table
            .columns
            .iter()
            .enumerate()
            .map(|(index, column)| v2::Column {
                name: column.name.clone(),
                ordinal: u32::try_from(index + 1).unwrap_or(u32::MAX),
                native_type: column.data_type.clone(),
                type_family: type_family(&column.data_type),
                length: None,
                precision: None,
                scale: None,
                nullable: column.nullable,
                default_expression: None,
                generated_expression: None,
                identity: false,
                auto_increment: false,
                collation: None,
                comment: None,
                sensitivity: None,
            })
            .collect(),
        constraints,
        indexes: table
            .indexes
            .iter()
            .map(|index| v2::Index {
                name: index.name.clone(),
                method: None,
                keys: index
                    .columns
                    .iter()
                    .map(|column| v2::IndexKey {
                        column: Some(column.clone()),
                        expression: None,
                        direction: None,
                    })
                    .collect(),
                included_columns: Vec::new(),
                predicate: None,
                unique: index.unique,
                valid: true,
            })
            .collect(),
    }
}

fn from_snapshot(snapshot: &v2::CatalogSnapshot) -> Catalog {
    Catalog {
        tables: snapshot.relations().iter().map(relation_to_table).collect(),
        objects: snapshot
            .routines()
            .iter()
            .map(|routine| DatabaseObject {
                schema: routine.object.namespace.clone(),
                name: routine.object.name.clone(),
                kind: routine
                    .native_kind
                    .clone()
                    .unwrap_or_else(|| "function".into()),
                detail: routine.detail.clone().or_else(|| {
                    (!routine.arguments.is_empty()).then(|| routine.arguments.join(", "))
                }),
                parent: routine.parent.clone(),
            })
            .chain(snapshot.other_objects().iter().map(|object| {
                DatabaseObject {
                    schema: object.object.namespace.clone(),
                    name: object.object.name.clone(),
                    kind: object
                        .native_kind
                        .clone()
                        .unwrap_or_else(|| object_kind_name(object.object.kind).into()),
                    detail: object.detail.clone().or_else(|| object.comment.clone()),
                    parent: object.parent.clone(),
                }
            }))
            .collect(),
    }
}

fn relation_to_table(relation: &v2::Relation) -> Table {
    let primary_columns = relation
        .constraints
        .iter()
        .filter(|constraint| constraint.kind == v2::ConstraintKind::Primary)
        .flat_map(|constraint| constraint.columns.iter().cloned())
        .collect::<HashSet<_>>();
    let foreign_keys = relation
        .constraints
        .iter()
        .filter(|constraint| constraint.kind == v2::ConstraintKind::Foreign)
        .flat_map(|constraint| {
            let referenced = constraint.referenced_relation.as_ref();
            constraint
                .columns
                .iter()
                .enumerate()
                .filter_map(move |(index, column)| {
                    let referenced = referenced?;
                    Some(ForeignKey {
                        column: column.clone(),
                        references_table: referenced.name.clone(),
                        references_column: constraint
                            .referenced_columns
                            .get(index)
                            .cloned()
                            .unwrap_or_default(),
                        references_schema: referenced.namespace.clone(),
                    })
                })
        })
        .collect();
    Table {
        schema: relation.object.namespace.clone(),
        name: relation.object.name.clone(),
        kind: object_kind_name(relation.object.kind).into(),
        columns: relation
            .columns
            .iter()
            .map(|column| Column {
                name: column.name.clone(),
                data_type: column.native_type.clone(),
                nullable: column.nullable,
                pk: primary_columns.contains(&column.name),
            })
            .collect(),
        foreign_keys,
        indexes: relation
            .indexes
            .iter()
            .map(|index| Index {
                name: index.name.clone(),
                columns: index
                    .keys
                    .iter()
                    .filter_map(|key| key.column.clone())
                    .collect(),
                unique: index.unique,
            })
            .collect(),
        row_estimate: relation.row_estimate,
    }
}

const fn engine(engine: Engine) -> v2::DatabaseEngine {
    match engine {
        Engine::Postgres => v2::DatabaseEngine::Postgres,
        Engine::Mysql => v2::DatabaseEngine::Mysql,
        Engine::Sqlite => v2::DatabaseEngine::Sqlite,
        Engine::Mongodb => v2::DatabaseEngine::Mongodb,
    }
}

fn object_kind(kind: &str) -> v2::ObjectKind {
    match kind {
        "table" | "collection" => v2::ObjectKind::Table,
        "view" => v2::ObjectKind::View,
        "materialized_view" => v2::ObjectKind::MaterializedView,
        "function" | "procedure" | "routine" => v2::ObjectKind::Routine,
        "sequence" => v2::ObjectKind::Sequence,
        "type" => v2::ObjectKind::Type,
        "trigger" => v2::ObjectKind::Trigger,
        _ => v2::ObjectKind::Other,
    }
}

const fn object_kind_name(kind: v2::ObjectKind) -> &'static str {
    match kind {
        v2::ObjectKind::Table => "table",
        v2::ObjectKind::View => "view",
        v2::ObjectKind::MaterializedView => "materialized_view",
        v2::ObjectKind::Routine => "function",
        v2::ObjectKind::Sequence => "sequence",
        v2::ObjectKind::Type => "type",
        v2::ObjectKind::Trigger => "trigger",
        v2::ObjectKind::Other => "other",
    }
}

fn type_family(native_type: &str) -> v2::NormalizedTypeFamily {
    let data_type = native_type.trim().to_ascii_lowercase();
    if data_type.ends_with("[]") || data_type.contains(" array") {
        return v2::NormalizedTypeFamily::Array;
    }
    let base = data_type
        .split(|character: char| character == '(' || character.is_ascii_whitespace())
        .next()
        .unwrap_or(data_type.as_str());
    if matches!(base, "bool" | "boolean") {
        v2::NormalizedTypeFamily::Boolean
    } else if matches!(
        base,
        "int"
            | "int2"
            | "int4"
            | "int8"
            | "integer"
            | "smallint"
            | "bigint"
            | "tinyint"
            | "mediumint"
            | "serial"
            | "smallserial"
            | "bigserial"
            | "year"
    ) {
        v2::NormalizedTypeFamily::Integer
    } else if matches!(base, "decimal" | "numeric" | "money") {
        v2::NormalizedTypeFamily::Decimal
    } else if matches!(base, "float" | "float4" | "float8" | "double" | "real") {
        v2::NormalizedTypeFamily::Float
    } else if matches!(base, "json" | "jsonb") {
        v2::NormalizedTypeFamily::Json
    } else if base == "uuid" {
        v2::NormalizedTypeFamily::Uuid
    } else if matches!(base, "timestamp" | "timestamptz" | "datetime") {
        v2::NormalizedTypeFamily::Timestamp
    } else if base == "date" {
        v2::NormalizedTypeFamily::Date
    } else if matches!(base, "time" | "timetz") {
        v2::NormalizedTypeFamily::Time
    } else if matches!(
        base,
        "bytea" | "blob" | "tinyblob" | "mediumblob" | "longblob" | "binary" | "varbinary"
    ) {
        v2::NormalizedTypeFamily::Binary
    } else if matches!(base, "document" | "bson") {
        v2::NormalizedTypeFamily::Document
    } else if matches!(
        base,
        "char"
            | "character"
            | "varchar"
            | "nvarchar"
            | "nchar"
            | "text"
            | "tinytext"
            | "mediumtext"
            | "longtext"
            | "string"
            | "citext"
            | "xml"
    ) {
        v2::NormalizedTypeFamily::Text
    } else {
        v2::NormalizedTypeFamily::Other
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn profile() -> ConnectionProfile {
        ConnectionProfile {
            id: Uuid::from_u128(1),
            name: "app".into(),
            engine: Engine::Postgres,
            provider: Default::default(),
            driver_id: None,
            host: "localhost".into(),
            port: 5432,
            database: "app".into(),
            username: "user".into(),
            sslmode: "disable".into(),
            extra_params: Default::default(),
            readonly_default: true,
            allow_writes: false,
            secret_ref: None,
            env: None,
            schema_group: None,
            workspace_access: Default::default(),
            credential_mode: Default::default(),
        }
    }

    #[test]
    fn legacy_catalog_round_trips_through_canonical_snapshot() {
        let catalog = Catalog {
            tables: vec![Table {
                schema: Some("public".into()),
                name: "users".into(),
                kind: "table".into(),
                columns: vec![Column {
                    name: "id".into(),
                    data_type: "uuid".into(),
                    nullable: false,
                    pk: true,
                }],
                foreign_keys: Vec::new(),
                indexes: vec![Index {
                    name: "users_id_idx".into(),
                    columns: vec!["id".into()],
                    unique: true,
                }],
                row_estimate: Some(3),
            }],
            objects: vec![
                DatabaseObject {
                    schema: Some("public".into()),
                    name: "rebuild_index".into(),
                    kind: "procedure".into(),
                    detail: Some("(target text)".into()),
                    parent: None,
                },
                DatabaseObject {
                    schema: Some("public".into()),
                    name: "users_updated".into(),
                    kind: "trigger".into(),
                    detail: Some("BEFORE UPDATE".into()),
                    parent: Some("users".into()),
                },
            ],
        };

        let snapshot = to_snapshot(&profile(), &catalog).unwrap();
        assert!(snapshot.has_canonical_fingerprint());
        assert_eq!(from_snapshot(&snapshot), catalog);
    }

    #[test]
    fn synthesized_foreign_key_names_do_not_depend_on_driver_row_order() {
        let mut table = Table {
            schema: Some("public".into()),
            name: "events".into(),
            kind: "table".into(),
            columns: Vec::new(),
            foreign_keys: vec![
                ForeignKey {
                    column: "user_id".into(),
                    references_table: "users".into(),
                    references_column: "id".into(),
                    references_schema: Some("public".into()),
                },
                ForeignKey {
                    column: "account_id".into(),
                    references_table: "accounts".into(),
                    references_column: "id".into(),
                    references_schema: Some("public".into()),
                },
            ],
            indexes: Vec::new(),
            row_estimate: None,
        };
        let first = table_to_relation(&table);
        table.foreign_keys.reverse();
        let second = table_to_relation(&table);

        assert_eq!(first.constraints, second.constraints);
    }

    #[test]
    fn native_type_family_uses_tokens_and_detects_arrays_first() {
        assert_eq!(type_family("integer[]"), v2::NormalizedTypeFamily::Array);
        assert_eq!(type_family("jsonb[]"), v2::NormalizedTypeFamily::Array);
        assert_eq!(type_family("bigint"), v2::NormalizedTypeFamily::Integer);
        assert_eq!(
            type_family("timestamp with time zone"),
            v2::NormalizedTypeFamily::Timestamp
        );
        assert_eq!(type_family("interval"), v2::NormalizedTypeFamily::Other);
        assert_eq!(type_family("point"), v2::NormalizedTypeFamily::Other);
    }
}
