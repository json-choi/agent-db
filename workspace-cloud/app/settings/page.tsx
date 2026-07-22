// Authenticated workspace and device-session console. Server rendering resolves the
// current Better Auth identity before exposing any organization administration UI.
import { and, eq } from "drizzle-orm";
import { headers } from "next/headers";
import { redirect } from "next/navigation";
import { auth } from "../../lib/auth";
import { db } from "../../lib/db";
import { member } from "../../lib/schema";
import { Brand } from "../components/Brand";
import { CreateWorkspaceForm } from "./CreateWorkspaceForm";
import { SignOutButton } from "./SignOutButton";
import { WorkspaceAccessPanel } from "./WorkspaceAccessPanel";

export const dynamic = "force-dynamic";

export default async function SettingsPage({
  searchParams,
}: {
  searchParams: Promise<{ workspace?: string | string[] }>;
}) {
  const params = await searchParams;
  const requestedWorkspaceId =
    typeof params.workspace === "string" &&
    /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i.test(params.workspace)
      ? params.workspace
      : null;
  const requestHeaders = await headers();
  const session = await auth.api.getSession({ headers: requestHeaders });
  const encodedWorkspaceId = requestedWorkspaceId
    ? encodeURIComponent(requestedWorkspaceId)
    : null;
  const settingsPath = encodedWorkspaceId
    ? `/settings?workspace=${encodedWorkspaceId}#workspace-${encodedWorkspaceId}`
    : "/settings";
  if (!session) {
    redirect(`/auth/sign-in?returnTo=${encodeURIComponent(settingsPath)}`);
  }
  const [workspaces, sessions] = await Promise.all([
    auth.api.listOrganizations({ headers: requestHeaders }),
    auth.api.listSessions({ headers: requestHeaders }),
  ]);
  const workspaceRoles = new Map(await Promise.all(workspaces.map(async (workspace) => {
    const membership = await db.query.member.findFirst({
      where: and(eq(member.organizationId, workspace.id), eq(member.userId, session.user.id)),
    });
    return [workspace.id, membership?.role ?? "viewer"] as const;
  })));
  const focusedWorkspaceId = workspaces.some(
    (workspace) => workspace.id === requestedWorkspaceId,
  )
    ? requestedWorkspaceId
    : null;
  const orderedWorkspaces = focusedWorkspaceId
    ? [
        ...workspaces.filter((workspace) => workspace.id === focusedWorkspaceId),
        ...workspaces.filter((workspace) => workspace.id !== focusedWorkspaceId),
      ]
    : workspaces;

  return (
    <main className="console-shell">
      <aside className="console-nav">
        <Brand />
        <nav><a className="active" href="#workspaces"><span>01</span> Workspaces</a><a href="#devices"><span>02</span> Devices</a></nav>
        <SignOutButton />
      </aside>
      <div className="console-main">
        <header className="console-header"><div><p className="eyebrow">BETTER AUTH / DRIZZLE</p><h1>워크스페이스 설정</h1></div><div className="user-chip"><span>{session.user.name.slice(0, 1).toUpperCase()}</span><div><strong>{session.user.name}</strong><small>{session.user.email}</small></div></div></header>
        <section id="workspaces" className="console-section">
          <div className="section-heading"><div><span>01</span><h2>Workspaces</h2></div><p>Better Auth Organization 멤버십이 권한 경계를 관리합니다.</p></div>
          <div className="workspace-grid">
            {orderedWorkspaces.map((workspace) => (
              <article
                className={`workspace-card-wrap${
                  workspace.id === focusedWorkspaceId ? " focused" : ""
                }`}
                id={`workspace-${workspace.id}`}
                key={workspace.id}
              >
                <div className="workspace-card">
                  <div className="workspace-monogram">
                    {workspace.name.slice(0, 2).toUpperCase()}
                  </div>
                  <div><h3>{workspace.name}</h3><p>{workspace.slug}</p></div>
                  <span className="status-dot">{workspaceRoles.get(workspace.id)}</span>
                </div>
                {["admin", "owner"].includes(workspaceRoles.get(workspace.id) ?? "") ? (
                  <WorkspaceAccessPanel workspaceId={workspace.id} />
                ) : null}
              </article>
            ))}
            {workspaces.length === 0 ? <div className="empty-state">아직 연결된 워크스페이스가 없습니다.</div> : null}
          </div>
          <CreateWorkspaceForm />
        </section>
        <section id="devices" className="console-section">
          <div className="section-heading"><div><span>02</span><h2>Active sessions</h2></div><p>Better Auth가 관리하는 브라우저와 데스크톱 Bearer 세션입니다.</p></div>
          <div className="device-table">
            {sessions.map((item) => <div className="device-row" key={item.id}><span className="device-icon">{item.userAgent?.includes("Mozilla") ? "◎" : "▣"}</span><div><strong>{item.userAgent?.includes("Mozilla") ? "Web browser" : "DopeDB desktop"}</strong><small>{item.ipAddress ?? "protected session"}</small></div><time>{item.id === session.session.id ? "현재 세션" : new Date(item.updatedAt).toLocaleDateString("ko-KR")}</time></div>)}
          </div>
        </section>
      </div>
    </main>
  );
}
