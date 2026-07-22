// Workspace-scoped shared connection collection. Templates intentionally exclude
// credentials; role and membership are resolved server-side on every request.
import { and, desc, eq, isNull } from "drizzle-orm";
import { db } from "../../../../../../lib/db";
import { env } from "../../../../../../lib/env";
import { jsonError, mutationAllowed } from "../../../../../../lib/http";
import { workspaceAuditEvent, workspaceConnection } from "../../../../../../lib/schema";
import { authorizeWorkspace } from "../../../../../../lib/workspace-authorization";
import { parseSharedConnection, publicConnection } from "../../../../../../lib/workspace-connections";

type RouteContext = { params: Promise<{ workspaceId: string }> };

export async function GET(request: Request, context: RouteContext) {
  const { workspaceId } = await context.params;
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
  return Response.json({
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
  const authorization = await authorizeWorkspace(request, workspaceId, "write");
  if (!authorization.ok) return jsonError(authorization.error, authorization.status);
  let input;
  try {
    input = parseSharedConnection(await request.json());
  } catch (error) {
    return jsonError(error instanceof Error ? error.message : "Invalid connection template", 400);
  }
  const [created] = await db
    .insert(workspaceConnection)
    .values({
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
    })
    .returning();
  await db.insert(workspaceAuditEvent).values({
    organizationId: workspaceId,
    actorUserId: authorization.session.user.id,
    action: "connection.share",
    resourceType: "connection",
    resourceId: created.id,
    redactedSummary: { name: created.name, engine: created.engine, environment: created.environment },
    requestId: crypto.randomUUID(),
  });
  return Response.json({
    connection: publicConnection(created, authorization.role, authorization.accessMode),
  }, { status: 201 });
}
