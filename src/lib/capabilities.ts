// Resolves which DriverCapability set applies to a saved connection, so screens can gate
// SQL-only UI (tabs, DDL, row editing, schema diff) without re-deriving driver lookup.
// A stale/unknown driverId falls back the same way ConnectionForm's activeDriver does
// (recommended driver for the engine, then the engine's first driver) so the two never
// disagree; only no driver at all for the engine fails closed to an empty set.
import type {
  ConnectionProfile,
  DriverCapability,
  DriverDescriptor,
  Engine,
} from "../ipc/types";

function connectionCapabilities(
  drivers: DriverDescriptor[],
  conn: ConnectionProfile,
): Set<DriverCapability> {
  const driver =
    (conn.driverId ? drivers.find((d) => d.id === conn.driverId) : undefined) ??
    drivers.find((d) => d.engine === conn.engine && d.recommended) ??
    drivers.find((d) => d.engine === conn.engine);
  return new Set(driver?.capabilities ?? []);
}

export function hasCapability(
  drivers: DriverDescriptor[],
  conn: ConnectionProfile,
  cap: DriverCapability,
): boolean {
  return connectionCapabilities(drivers, conn).has(cap);
}

// Document-family engines: no SQL surface, queried through the typed document API.
// The single place a future document engine gets added — screens branch on this
// instead of comparing engine literals (mirrors Engine::is_document in model.rs).
// `undefined` (no connection in scope) counts as not-document.
export function isDocumentEngine(engine: Engine | undefined): boolean {
  return engine === "mongodb";
}
