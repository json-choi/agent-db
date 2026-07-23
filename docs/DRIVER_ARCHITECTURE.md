# Driver architecture

DopeDB separates three concepts that database tools often collapse:

- **Engine**: the wire/query model (`postgres`, `mysql`, `sqlite`, later `mongodb`,
  `neo4j`).
- **Provider**: a hosting control plane (`generic`, `neon`, `planetScale`, later
  `atlas`, `aura`). A provider never implies one engine; PlanetScale is the immediate
  example because it offers both Vitess/MySQL and PostgreSQL.
- **Driver**: the local adapter that speaks an engine protocol. A saved connection can
  pin `driverId`, or leave it empty so the registry chooses the highest-priority installed
  compatible driver.

```text
Connection form / MCP
        |
        v
Driver registry ---- provider capability overlay
        |
        +---- bundled adapter (SQLx today)
        |
        `---- managed sidecar pack (future Mongo/graph packs)
                    |
                    v
               database endpoint
```

## Layer boundaries

1. `model.rs` owns the persisted `engine`, `provider`, and optional `driverId` contract.
2. `driver/mod.rs` owns discovery, recommendation, compatibility validation, install
   state, capabilities, and runtime dispatch.
3. `connection/pool.rs` is an adapter implementation. It knows how to build SQLx pools,
   but does not choose which driver should handle a profile.
4. `connection/providers.rs` owns provider detection and connection tuning. Provider
   behavior must not be encoded as a new SQL driver when the wire protocol is unchanged.
5. Screens consume serializable driver descriptors. They do not hard-code installed
   versions or infer compatibility from package names.

Higher layers must test capabilities (`sql`, `documents`, `graph`, `transactions`,
`introspection`) rather than driver ids. This allows document and graph results to gain
their own workspaces instead of being flattened into SQL tables.

## Installation modes

`bundled` drivers are compiled into the signed application. PostgreSQL, MySQL/MariaDB,
and SQLite currently use this mode through SQLx, so selecting them requires no download.

`managed` drivers are reserved for independently shipped executable sidecars. Rust crates
cannot safely be downloaded and hot-loaded like JDBC jars: Rust has no stable plugin ABI,
and loading arbitrary dynamic libraries into the Tauri process would expand the trusted
computing base. A managed pack therefore communicates over a versioned local protocol and
can crash or be replaced without corrupting the app process.

A managed pack must not enter the catalog until all of the following exist:

1. Per-platform artifacts for macOS arm64/x64, Windows x64, and supported Linux targets.
2. A versioned driver-host protocol and explicit minimum/maximum app protocol versions.
3. A trusted catalog entry containing artifact size and SHA-256 digest.
4. Signature verification rooted in a public key compiled into the app.
5. Download to a staging path, digest/signature verification, then atomic activation.
6. Storage outside the application bundle with a per-driver/version directory.
7. Rollback to the last verified version and deletion only through the driver manager.

Until those requirements are implemented, a managed catalog entry must remain
`available` and `install_driver` must fail closed rather than execute an unverified file.

## Native provider support

The protocol adapter is only the data plane. Native cloud support belongs in a separate
control-plane client:

- Neon: SQLx PostgreSQL data plane plus projects, branches, computes, roles, databases,
  replicas, and operation polling through the Neon API.
- PlanetScale: SQLx MySQL or PostgreSQL data plane plus branches and provider resources.
  Vitess deploy requests must be capability-gated because PlanetScale PostgreSQL does not
  share that workflow.
- GCP Cloud SQL: SQLx PostgreSQL/MySQL data plane plus a provider adapter that exchanges
  Vercel OIDC through Workload Identity Federation for short-lived IAM DB login tokens.
  The driver pins the instance CA; MySQL cleartext authentication is enabled only over
  that verified TLS channel.
- MongoDB Atlas: the official MongoDB Rust driver for BSON/commands plus the Atlas Admin
  API for managed resources.
- Neo4j Aura: a Bolt/Cypher adapter plus the Aura control plane. Graph results remain
  nodes, relationships, and paths.

## Adding a driver

1. Add the engine only when its query and result model is implemented end to end.
2. Add a registry descriptor and capability set.
3. Implement a bundled adapter or publish a verified sidecar pack.
4. Add provider overlays independently from the engine adapter.
5. Add contract, persistence, compatibility, and failure-closed tests.
6. Expose the engine-specific editor and result surface only after the adapter is usable.
