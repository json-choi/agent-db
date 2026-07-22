// Mutation surface for one shared template. UUID lookup is always intersected with
// the authenticated organization to prevent cross-workspace identifier access.
import { and, eq, isNull, sql } from "drizzle-orm";
import { db } from "../../../../../../../lib/db";
import { env } from "../../../../../../../lib/env";
import { jsonError, mutationAllowed } from "../../../../../../../lib/http";
import { workspaceAuditEvent, workspaceConnection } from "../../../../../../../lib/schema";
import { authorizeWorkspace } from "../../../../../../../lib/workspace-authorization";
import { parseSharedConnection, publicConnection } from "../../../../../../../lib/workspace-connections";

type RouteContext = { params: Promise<{ workspaceId: string; connectionId: string }> };

export async function POST(request: Request, context: RouteContext) {
  if (!mutationAllowed(request, env.appOrigin())) return jsonError("Invalid request origin", 403);
  const { workspaceId, connectionId } = await context.params;
  const body = (await request.json().catch(() => null)) as { action?: unknown } | null;
  if (body?.action !== "read" && body?.action !== "write") {
    return jsonError("Action must be read or write", 400);
  }
  const authorization = await authorizeWorkspace(request, workspaceId, body.action);
  if (!authorization.ok) return jsonError(authorization.error, authorization.status);
  const connection = await db.query.workspaceConnection.findFirst({
    where: and(
      eq(workspaceConnection.id, connectionId),
      eq(workspaceConnection.organizationId, workspaceId),
      isNull(workspaceConnection.deletedAt),
    ),
    columns: { id: true, revision: true },
  });
  if (!connection) return jsonError("Connection not found", 404);
  return Response.json({
    allowed: true,
    action: body.action,
    role: authorization.role,
    accessMode: authorization.accessMode,
    revision: connection.revision,
  });
}

export async function PATCH(request: Request, context: RouteContext) {
  if (!mutationAllowed(request, env.appOrigin())) return jsonError("Invalid request origin", 403);
  const { workspaceId, connectionId } = await context.params;
  const authorization = await authorizeWorkspace(request, workspaceId, "write");
  if (!authorization.ok) return jsonError(authorization.error, authorization.status);
  let input;
  try {
    input = parseSharedConnection(await request.json());
  } catch (error) {
    return jsonError(error instanceof Error ? error.message : "Invalid connection template", 400);
  }
  const [updated] = await db
    .update(workspaceConnection)
    .set({
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
      revision: sql`${workspaceConnection.revision} + 1`,
      updatedAt: new Date(),
    })
    .where(and(
      eq(workspaceConnection.id, connectionId),
      eq(workspaceConnection.organizationId, workspaceId),
      isNull(workspaceConnection.deletedAt),
    ))
    .returning();
  if (!updated) return jsonError("Connection not found", 404);
  await db.insert(workspaceAuditEvent).values({
    organizationId: workspaceId,
    actorUserId: authorization.session.user.id,
    action: "connection.update",
    resourceType: "connection",
    resourceId: updated.id,
    redactedSummary: { name: updated.name, revision: updated.revision },
    requestId: crypto.randomUUID(),
  });
  return Response.json({
    connection: publicConnection(updated, authorization.role, authorization.accessMode),
  });
}

export async function DELETE(request: Request, context: RouteContext) {
  if (!mutationAllowed(request, env.appOrigin())) return jsonError("Invalid request origin", 403);
  const { workspaceId, connectionId } = await context.params;
  const authorization = await authorizeWorkspace(request, workspaceId, "manage");
  if (!authorization.ok) return jsonError(authorization.error, authorization.status);
  const [deleted] = await db
    .update(workspaceConnection)
    .set({ deletedAt: new Date(), updatedAt: new Date() })
    .where(and(
      eq(workspaceConnection.id, connectionId),
      eq(workspaceConnection.organizationId, workspaceId),
      isNull(workspaceConnection.deletedAt),
    ))
    .returning({ id: workspaceConnection.id, name: workspaceConnection.name });
  if (!deleted) return jsonError("Connection not found", 404);
  await db.insert(workspaceAuditEvent).values({
    organizationId: workspaceId,
    actorUserId: authorization.session.user.id,
    action: "connection.delete",
    resourceType: "connection",
    resourceId: deleted.id,
    redactedSummary: { name: deleted.name },
    requestId: crypto.randomUUID(),
  });
  return new Response(null, { status: 204 });
}
