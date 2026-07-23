import { describe, expect, it } from "vitest";
import {
  missingPlanetScaleManagedScopes,
  planetScaleEngine,
  planetScaleManagedScopes,
} from "./planetscale-core";

describe("PlanetScale response normalization", () => {
  it("maps the provider's postgresql kind to the DopeDB postgres engine", () => {
    expect(planetScaleEngine("postgresql")).toBe("postgres");
    expect(planetScaleEngine("mysql")).toBe("mysql");
    expect(planetScaleEngine("mongodb")).toBeNull();
  });

  it("fails closed when a managed-access OAuth scope is absent", () => {
    const all = planetScaleManagedScopes.join(" ");
    expect(missingPlanetScaleManagedScopes(all)).toEqual([]);
    expect(missingPlanetScaleManagedScopes(all.replace("manage_passwords", "")))
      .toEqual(["manage_passwords"]);
  });
});
