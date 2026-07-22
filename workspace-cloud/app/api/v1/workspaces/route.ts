import { and, eq, inArray } from "drizzle-orm";
import { auth } from "../../../../lib/auth";
import { db } from "../../../../lib/db";
import { env } from "../../../../lib/env";
import { jsonError, mutationAllowed, privateJson } from "../../../../lib/http";
import { member } from "../../../../lib/schema";

export async function GET(request: Request) {
  const session = await auth.api.getSession({ headers: request.headers });
  if (!session) return jsonError("Unauthorized", 401);
  const workspaces = await auth.api.listOrganizations({ headers: request.headers });
  const roles = workspaces.length > 0
    ? await db.select({ organizationId: member.organizationId, role: member.role })
        .from(member)
        .where(and(
          eq(member.userId, session.user.id),
          inArray(member.organizationId, workspaces.map((workspace) => workspace.id)),
        ))
    : [];
  const roleByWorkspace = new Map(roles.map((membership) => [
    membership.organizationId,
    membership.role,
  ]));
  return privateJson({
    workspaces: workspaces.map((workspace) => ({
      ...workspace,
      role: roleByWorkspace.get(workspace.id) ?? "viewer",
    })),
  });
}

export async function POST(request: Request) {
  if (!mutationAllowed(request, env.appOrigin())) return jsonError("Invalid request origin", 403);
  const session = await auth.api.getSession({ headers: request.headers });
  if (!session) return jsonError("Unauthorized", 401);
  const body = (await request.json().catch(() => null)) as { name?: string } | null;
  const name = body?.name?.trim();
  if (!name || name.length > 120) return jsonError("Workspace name must be 1–120 characters", 400);
  const base = name
    .normalize("NFKD")
    .toLowerCase()
    .replace(/[^a-z0-9]+/g, "-")
    .replace(/^-|-$/g, "")
    .slice(0, 48) || "workspace";
  const slug = `${base}-${crypto.randomUUID().slice(0, 8)}`;
  const workspace = await auth.api.createOrganization({
    headers: request.headers,
    body: { name, slug },
  });
  return privateJson({ workspace }, { status: 201 });
}
