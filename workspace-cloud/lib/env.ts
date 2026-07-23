// Server-only environment access. Values are read lazily so static pages can build
// without production secrets; request handlers fail closed when configuration is absent.
import "server-only";

function required(name: string): string {
  const value = process.env[name]?.trim();
  if (!value) throw new Error(`Missing required environment variable: ${name}`);
  return value;
}

function appOrigin(): string {
  const raw = required("BETTER_AUTH_URL");
  const url = new URL(raw);
  const localDevelopment =
    process.env.NODE_ENV !== "production" &&
    url.protocol === "http:" &&
    ["localhost", "127.0.0.1", "[::1]"].includes(url.hostname);
  if (
    (url.protocol !== "https:" && !localDevelopment) ||
    url.username ||
    url.password ||
    url.pathname !== "/" ||
    url.search ||
    url.hash
  ) {
    throw new Error("BETTER_AUTH_URL must be an HTTPS origin");
  }
  return url.origin;
}

function authSecret(): string {
  const value = required("BETTER_AUTH_SECRET");
  if (value.length < 32) throw new Error("BETTER_AUTH_SECRET must be at least 32 characters");
  return value;
}

function optional(name: string): string | null {
  return process.env[name]?.trim() || null;
}

export const env = {
  appOrigin,
  authSecret,
  credentialKey: () => required("WORKSPACE_CREDENTIAL_KEY"),
  databaseUrl: () => required("DATABASE_URL"),
  googleClientId: () => required("GOOGLE_CLIENT_ID"),
  googleClientSecret: () => required("GOOGLE_CLIENT_SECRET"),
  planetScaleClientId: () => optional("PLANETSCALE_CLIENT_ID"),
  planetScaleClientSecret: () => optional("PLANETSCALE_CLIENT_SECRET"),
};
