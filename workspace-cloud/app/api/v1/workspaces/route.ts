import { auth } from "../../../../lib/auth";
import { env } from "../../../../lib/env";
import { jsonError, mutationAllowed } from "../../../../lib/http";

export async function GET(request: Request) {
  const session = await auth.api.getSession({ headers: request.headers });
  if (!session) return jsonError("Unauthorized", 401);
  const workspaces = await auth.api.listOrganizations({ headers: request.headers });
  return Response.json({ workspaces });
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
  return Response.json({ workspace }, { status: 201 });
}
