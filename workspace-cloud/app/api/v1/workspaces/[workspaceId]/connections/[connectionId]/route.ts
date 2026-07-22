// Mutation surface for one shared template. UUID lookup is always intersected with
// the authenticated organization to prevent cross-workspace identifier access.
import { and, eq, isNull, sql } from "drizzle-orm";
import { db } from "../../../../../../../lib/db";
import { env } from "../../../../../../../lib/env";
import { isUuid, jsonError, mutationAllowed, privateJson } from "../../../../../../../lib/http";
import { workspaceAuditEvent, workspaceConnection } from "../../../../../../../lib/schema";
import { authorizeWorkspace } from "../../../../../../../lib/workspace-authorization";
import { parseSharedConnection, publicConnection } from "../../../../../../../lib/workspace-connections";

type RouteContext = { params: Promise<{ workspaceId: string; connectionId: string }> };

export async function POST(request: Request, context: RouteContext) {
  if (!mutationAllowed(request, env.appOrigin())) return jsonError("Invalid request origin", 403);
  const { workspaceId, connectionId } = await context.params;
  if (!isUuid(workspaceId) || !isUuid(connectionId)) {
    return jsonError("Invalid workspace or connection id", 400);
  }
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
  return privateJson({
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
  if (!isUuid(workspaceId) || !isUuid(connectionId)) {
    return jsonError("Invalid workspace or connection id", 400);
  }
  const authorization = await authorizeWorkspace(request, workspaceId, "write");
  if (!authorization.ok) return jsonError(authorization.error, authorization.status);
  let input;
  try {
    input = parseSharedConnection(await request.json());
  } catch (error) {
    return jsonError(error instanceof Error ? error.message : "Invalid connection template", 400);
  }
  const updatedAt = new Date();
  const requestId = crypto.randomUUID();
  const [updatedRows] = await db.batch([
    db.update(workspaceConnection)
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
        updatedAt,
      })
      .where(and(
        eq(workspaceConnection.id, connectionId),
        eq(workspaceConnection.organizationId, workspaceId),
        isNull(workspaceConnection.deletedAt),
      ))
      .returning(),
    db.execute(sql`
      INSERT INTO ${workspaceAuditEvent}
        ("organization_id", "actor_user_id", "action", "resource_type",
         "resource_id", "redacted_summary", "request_id")
      SELECT connection."organization_id", ${authorization.session.user.id},
             'connection.update', 'connection', connection."id"::text,
             jsonb_build_object('name', connection."name", 'revision', connection."revision"),
             ${requestId}::uuid
      FROM ${workspaceConnection} AS connection
      WHERE connection."id" = ${connectionId}::uuid
        AND connection."organization_id" = ${workspaceId}
        AND connection."updated_at" = ${updatedAt}
        AND connection."deleted_at" IS NULL
    `),
  ]);
  const updated = updatedRows[0];
  if (!updated) return jsonError("Connection not found", 404);
  return privateJson({
    connection: publicConnection(updated, authorization.role, authorization.accessMode),
  });
}

export async function DELETE(request: Request, context: RouteContext) {
  if (!mutationAllowed(request, env.appOrigin())) return jsonError("Invalid request origin", 403);
  const { workspaceId, connectionId } = await context.params;
  if (!isUuid(workspaceId) || !isUuid(connectionId)) {
    return jsonError("Invalid workspace or connection id", 400);
  }
  // Editors own connection lifecycle; admin/owner capabilities are reserved for
  // membership and workspace administration.
  const authorization = await authorizeWorkspace(request, workspaceId, "write");
  if (!authorization.ok) return jsonError(authorization.error, authorization.status);
  const deletedAt = new Date();
  const requestId = crypto.randomUUID();
  const [deletedRows] = await db.batch([
    db.update(workspaceConnection)
      .set({ deletedAt, updatedAt: deletedAt })
      .where(and(
        eq(workspaceConnection.id, connectionId),
        eq(workspaceConnection.organizationId, workspaceId),
        isNull(workspaceConnection.deletedAt),
      ))
      .returning({ id: workspaceConnection.id, name: workspaceConnection.name }),
    db.execute(sql`
      INSERT INTO ${workspaceAuditEvent}
        ("organization_id", "actor_user_id", "action", "resource_type",
         "resource_id", "redacted_summary", "request_id")
      SELECT connection."organization_id", ${authorization.session.user.id},
             'connection.delete', 'connection', connection."id"::text,
             jsonb_build_object('name', connection."name"), ${requestId}::uuid
      FROM ${workspaceConnection} AS connection
      WHERE connection."id" = ${connectionId}::uuid
        AND connection."organization_id" = ${workspaceId}
        AND connection."deleted_at" = ${deletedAt}
    `),
  ]);
  const deleted = deletedRows[0];
  if (!deleted) return jsonError("Connection not found", 404);
  return new Response(null, {
    status: 204,
    headers: { "cache-control": "private, no-store" },
  });
}
