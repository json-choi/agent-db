// Pure PlanetScale response normalization shared by the server adapter and tests.
// PlanetScale names its PostgreSQL kind `postgresql`; DopeDB's engine id is `postgres`.

export const planetScaleManagedScopes = [
  "read_organizations",
  "read_databases",
  "read_branches",
  "manage_passwords",
  "manage_production_branch_passwords",
] as const;

export function planetScaleEngine(value: unknown): "postgres" | "mysql" | null {
  if (value === "postgresql" || value === "postgres") return "postgres";
  if (value === "mysql") return "mysql";
  return null;
}

export function missingPlanetScaleManagedScopes(scope: unknown): string[] {
  const granted = new Set(
    typeof scope === "string" ? scope.split(/\s+/).filter(Boolean) : [],
  );
  return planetScaleManagedScopes.filter((item) => !granted.has(item));
}
