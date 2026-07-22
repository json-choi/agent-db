// Workspace-scoped shared connection collection. Templates intentionally exclude
// credentials; role and membership are resolved server-side on every request.
import { and, desc, eq, isNull } from "drizzle-orm";
import { db } from "../../../../../../lib/db";
import { env } from "../../../../../../lib/env";
import { isUuid, jsonError, mutationAllowed, privateJson } from "../../../../../../lib/http";
import { workspaceAuditEvent, workspaceConnection } from "../../../../../../lib/schema";
import { authorizeWorkspace } from "../../../../../../lib/workspace-authorization";
import { parseSharedConnection, publicConnection } from "../../../../../../lib/workspace-connections";

type RouteContext = { params: Promise<{ workspaceId: string }> };

export async function GET(request: Request, context: RouteContext) {
  const { workspaceId } = await context.params;
  if (!isUuid(workspaceId)) return jsonError("Invalid workspace id", 400);
  const authorization = await authorizeWorkspace(request, workspaceId, "view");
  if (!authorization.ok) return jsonError(authorization.error, authorization.status);
  const rows = await db
    .select()
    .from(workspaceConnection)
    .where(and(
      eq(workspaceConnection.organizationId, workspaceId),
      isNull(workspaceConnection.deletedAt),
    ))
    .orderBy(desc(workspaceConnection.updatedAt));
  return privateJson({
    workspaceId,
    role: authorization.role,
    accessMode: authorization.accessMode,
    connections: rows.map((row) => publicConnection(
      row,
      authorization.role,
      authorization.accessMode,
    )),
  });
}

export async function POST(request: Request, context: RouteContext) {
  if (!mutationAllowed(request, env.appOrigin())) return jsonError("Invalid request origin", 403);
  const { workspaceId } = await context.params;
  if (!isUuid(workspaceId)) return jsonError("Invalid workspace id", 400);
  const authorization = await authorizeWorkspace(request, workspaceId, "manage");
  if (!authorization.ok) return jsonError(authorization.error, authorization.status);
  let input;
  try {
    input = parseSharedConnection(await request.json());
  } catch (error) {
    return jsonError(error instanceof Error ? error.message : "Invalid connection template", 400);
  }
  const connectionId = crypto.randomUUID();
  const [createdRows] = await db.batch([
    db.insert(workspaceConnection).values({
      id: connectionId,
      organizationId: workspaceId,
      name: input.name,
      engine: input.engine,
      provider: input.provider,
      driverId: input.driverId,
      host: input.host,
      port: input.port,
      databaseName: input.database,
      sslmode: input.sslmode,
      readonlyDefault: input.readonlyDefault,
      allowWrites: input.allowWrites,
      environment: input.env,
      schemaGroup: input.schemaGroup,
      createdByUserId: authorization.session.user.id,
    }).returning(),
    db.insert(workspaceAuditEvent).values({
      organizationId: workspaceId,
      actorUserId: authorization.session.user.id,
      action: "connection.share",
      resourceType: "connection",
      resourceId: connectionId,
      redactedSummary: {
        name: input.name,
        engine: input.engine,
        environment: input.env,
      },
      requestId: crypto.randomUUID(),
    }),
  ]);
  const created = createdRows[0];
  return privateJson({
    connection: publicConnection(created, authorization.role, authorization.accessMode),
  }, { status: 201 });
}
