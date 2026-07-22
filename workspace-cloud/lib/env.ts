// Server-only environment access. Values are read lazily so static pages can build
// without production secrets; request handlers fail closed when configuration is absent.
import "server-only";

function required(name: string): string {
  const value = process.env[name]?.trim();
  if (!value) throw new Error(`Missing required environment variable: ${name}`);
  return value;
}

export const env = {
  appOrigin: () => required("BETTER_AUTH_URL").replace(/\/$/, ""),
  authSecret: () => required("BETTER_AUTH_SECRET"),
  databaseUrl: () => required("DATABASE_URL"),
  googleClientId: () => required("GOOGLE_CLIENT_ID"),
  googleClientSecret: () => required("GOOGLE_CLIENT_SECRET"),
};
