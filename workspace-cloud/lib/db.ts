import "server-only";
import { neon } from "@neondatabase/serverless";
import { drizzle } from "drizzle-orm/neon-http";
import { env } from "./env";
import * as schema from "./schema";

const globalForDb = globalThis as typeof globalThis & {
  workspaceDb?: ReturnType<typeof createDb>;
};

function createDb() {
  return drizzle({ client: neon(env.databaseUrl()), schema });
}

export const db = globalForDb.workspaceDb ?? createDb();
if (process.env.NODE_ENV !== "production") globalForDb.workspaceDb = db;
