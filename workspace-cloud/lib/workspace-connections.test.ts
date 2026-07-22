import { describe, expect, it } from "vitest";
import { parseSharedConnection } from "./workspace-connections";

const validTemplate = {
  name: "Analytics",
  engine: "postgres",
  provider: "neon",
  driverId: null,
  host: "db.example.com",
  port: 5432,
  database: "analytics",
  sslmode: "require",
  readonlyDefault: true,
  allowWrites: false,
  env: "prod",
  schemaGroup: null,
};

describe("parseSharedConnection", () => {
  it("accepts a redacted endpoint template", () => {
    expect(parseSharedConnection(validTemplate)).toEqual(validTemplate);
  });

  it.each(["password", "token", "username", "connectionUrl", "secretRef"])(
    "rejects secret-bearing field %s",
    (field) => {
      expect(() => parseSharedConnection({ ...validTemplate, [field]: "sensitive" }))
        .toThrow(/Secret-bearing field/);
    },
  );

  it("rejects credentials or URLs embedded in the host", () => {
    expect(() => parseSharedConnection({
      ...validTemplate,
      host: "postgresql://user:pass@db.example.com",
    })).toThrow(/Host must not contain credentials/);
  });

  it("rejects control characters in user-visible metadata", () => {
    expect(() => parseSharedConnection({
      ...validTemplate,
      name: "Analytics\nspoofed",
    })).toThrow(/Invalid text value/);
  });

  it.each([0, 65536, 12.5])("rejects invalid port %s", (port) => {
    expect(() => parseSharedConnection({ ...validTemplate, port })).toThrow(/Invalid port/);
  });
});
