// Resolves which DriverCapability set applies to a saved connection, so screens can gate
// SQL-only UI (tabs, DDL, row editing, schema diff) without re-deriving driver lookup.
// No match (unknown driverId, or no recommended driver for the engine) fails closed to an
// empty set — safer to hide a SQL feature than assume one is available.
import type { ConnectionProfile, DriverCapability, DriverDescriptor } from "../ipc/types";

export function connectionCapabilities(
  drivers: DriverDescriptor[],
  conn: ConnectionProfile,
): Set<DriverCapability> {
  const driver = conn.driverId
    ? drivers.find((d) => d.id === conn.driverId)
    : drivers.find((d) => d.engine === conn.engine && d.recommended);
  return new Set(driver?.capabilities ?? []);
}

export function hasCapability(
  drivers: DriverDescriptor[],
  conn: ConnectionProfile,
  cap: DriverCapability,
): boolean {
  return connectionCapabilities(drivers, conn).has(cap);
}
