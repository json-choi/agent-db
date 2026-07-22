"use client";

// Workspace membership administration. Mutations are confirmed by the server and
// the rendered list is then reloaded from Better Auth's organization state.
import { FormEvent, useEffect, useState } from "react";

type WorkspaceMember = {
  id: string;
  userId: string;
  name: string;
  email: string;
  role: string;
};
type PendingInvitation = {
  id: string;
  email: string;
  role: string | null;
  inviteUrl: string;
};

const roleLabel: Record<string, string> = {
  analyst: "읽기 전용",
  editor: "읽기 / 쓰기",
  admin: "관리자",
  owner: "소유자",
  viewer: "보기 전용",
};

export function WorkspaceAccessPanel({ workspaceId }: { workspaceId: string }) {
  const [members, setMembers] = useState<WorkspaceMember[]>([]);
  const [invitations, setInvitations] = useState<PendingInvitation[]>([]);
  const [email, setEmail] = useState("");
  const [role, setRole] = useState("analyst");
  const [pending, setPending] = useState(false);
  const [error, setError] = useState("");

  async function load() {
    const response = await fetch(`/api/v1/workspaces/${workspaceId}/members`);
    if (!response.ok) return;
    const body = await response.json();
    setMembers(body.members);
    setInvitations(body.invitations);
  }

  useEffect(() => { void load(); }, [workspaceId]);

  async function invite(event: FormEvent) {
    event.preventDefault();
    setPending(true);
    setError("");
    const response = await fetch(`/api/v1/workspaces/${workspaceId}/members`, {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({ email, role }),
    }).catch(() => null);
    setPending(false);
    if (!response?.ok) {
      const body = await response?.json().catch(() => null);
      setError(body?.error ?? "초대를 만들지 못했습니다.");
      return;
    }
    setEmail("");
    await load();
  }

  async function updateRole(memberId: string, nextRole: string) {
    setError("");
    const response = await fetch(`/api/v1/workspaces/${workspaceId}/members`, {
      method: "PATCH",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({ memberId, role: nextRole }),
    }).catch(() => null);
    if (!response?.ok) {
      const body = await response?.json().catch(() => null);
      setError(body?.error ?? "권한을 변경하지 못했습니다.");
      return;
    }
    await load();
  }

  return (
    <div className="access-panel">
      <div className="access-heading"><strong>멤버 및 RBAC</strong><small>권한은 모든 API 요청에서 서버가 다시 확인합니다.</small></div>
      <div className="member-list">
        {members.map((item) => (
          <div className="member-row" key={item.id}>
            <div><strong>{item.name}</strong><small>{item.email}</small></div>
            {item.role === "owner" ? <span>{roleLabel.owner}</span> : (
              <select value={item.role} onChange={(event) => void updateRole(item.id, event.target.value)}>
                <option value="viewer">보기 전용 (실행 불가)</option>
                <option value="analyst">읽기 전용</option>
                <option value="editor">읽기 / 쓰기</option>
                <option value="admin">관리자</option>
              </select>
            )}
          </div>
        ))}
      </div>
      <form className="invite-form ds-control-row" onSubmit={invite}>
        <input type="email" value={email} onChange={(event) => setEmail(event.target.value)} placeholder="member@company.com" required />
        <select value={role} onChange={(event) => setRole(event.target.value)}>
          <option value="viewer">보기 전용 (실행 불가)</option>
          <option value="analyst">읽기 전용</option>
          <option value="editor">읽기 / 쓰기</option>
          <option value="admin">관리자</option>
        </select>
        <button type="submit" disabled={pending}>{pending ? "생성 중" : "초대 링크 만들기"}</button>
      </form>
      {invitations.length > 0 ? (
        <div className="invitation-list">
          {invitations.map((item) => (
            <div className="invitation-row" key={item.id}>
              <div><strong>{item.email}</strong><small>{roleLabel[item.role ?? ""] ?? item.role}</small></div>
              <button type="button" onClick={() => void navigator.clipboard.writeText(item.inviteUrl)}>링크 복사</button>
            </div>
          ))}
          <p>초대 링크는 해당 이메일로 Google 로그인한 사용자만 수락할 수 있습니다.</p>
        </div>
      ) : null}
      {error ? <small className="form-error">{error}</small> : null}
    </div>
  );
}
