"use client";

import { FormEvent, useState } from "react";
import { useRouter } from "next/navigation";

export function CreateWorkspaceForm() {
  const router = useRouter();
  const [name, setName] = useState("");
  const [error, setError] = useState("");
  const [pending, setPending] = useState(false);

  async function submit(event: FormEvent) {
    event.preventDefault();
    setPending(true);
    setError("");
    const response = await fetch("/api/v1/workspaces", {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({ name }),
    }).catch(() => null);
    setPending(false);
    if (!response?.ok) {
      const body = await response?.json().catch(() => null);
      setError(body?.error ?? "워크스페이스를 만들지 못했습니다.");
      return;
    }
    setName("");
    router.refresh();
  }

  return (
    <form className="create-form" onSubmit={submit}>
      <label htmlFor="workspace-name">새 워크스페이스</label>
      <div><input id="workspace-name" value={name} onChange={(event) => setName(event.target.value)} maxLength={120} placeholder="예: Data Platform" required /><button type="submit" disabled={pending}>{pending ? "생성 중" : "만들기"}</button></div>
      {error ? <small className="form-error">{error}</small> : null}
    </form>
  );
}
