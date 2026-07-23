"use client";

// Flat managed-access setup flow: connect a provider, select a shared connection and
// provider resource, then enable per-member short-lived database credentials.
import { useCallback, useEffect, useMemo, useState } from "react";

type Provider = {
  id: string;
  name: string;
  availability: "available" | "planned";
  configured: boolean;
  note: string;
  leaseSeconds: number | null;
};

type Integration = {
  id: string;
  provider: string;
  displayName: string;
  grantedScope: string | null;
  updatedAt: string;
};

type SharedConnection = {
  id: string;
  name: string;
  engine: string;
  credentialMode: "managed" | "member_local";
  allowWrites: boolean;
};

type Resource = {
  id: string;
  name: string;
  kind?: "postgres" | "mysql";
  production?: boolean;
  ready?: boolean;
};

type ResourceSelection = {
  organization: string;
  database: string;
  branch: string;
};

type ManagedConnection = {
  connectionId: string;
  integrationId: string;
  resource: ResourceSelection & { engine: "postgres" | "mysql" };
};

const emptySelection: ResourceSelection = {
  organization: "",
  database: "",
  branch: "",
};

async function responseError(response: Response | null, fallback: string) {
  const body = await response?.json().catch(() => null);
  return typeof body?.error === "string" ? body.error : fallback;
}

export function ProviderAccessPanel({ workspaceId }: { workspaceId: string }) {
  const [providers, setProviders] = useState<Provider[]>([]);
  const [integrations, setIntegrations] = useState<Integration[]>([]);
  const [connections, setConnections] = useState<SharedConnection[]>([]);
  const [managedConnections, setManagedConnections] = useState<ManagedConnection[]>([]);
  const [selectedConnectionId, setSelectedConnectionId] = useState("");
  const [selectedIntegrationId, setSelectedIntegrationId] = useState("");
  const [selection, setSelection] = useState<ResourceSelection>(emptySelection);
  const [organizations, setOrganizations] = useState<Resource[]>([]);
  const [databases, setDatabases] = useState<Resource[]>([]);
  const [branches, setBranches] = useState<Resource[]>([]);
  const [loading, setLoading] = useState(true);
  const [resourcePending, setResourcePending] = useState(false);
  const [mutation, setMutation] = useState("");
  const [error, setError] = useState("");

  const selectedConnection = useMemo(
    () => connections.find((item) => item.id === selectedConnectionId) ?? null,
    [connections, selectedConnectionId],
  );
  const selectedIntegration = integrations.find(
    (item) => item.id === selectedIntegrationId,
  ) ?? null;
  const currentManagedConnection = managedConnections.find(
    (item) => item.connectionId === selectedConnectionId,
  ) ?? null;

  const load = useCallback(async (signal?: AbortSignal) => {
    setLoading(true);
    const [providerResponse, connectionResponse] = await Promise.all([
      fetch(`/api/v1/workspaces/${workspaceId}/provider-integrations`, {
        cache: "no-store",
        signal,
      }).catch(() => null),
      fetch(`/api/v1/workspaces/${workspaceId}/connections`, {
        cache: "no-store",
        signal,
      }).catch(() => null),
    ]);
    if (signal?.aborted) return;
    if (!providerResponse?.ok || !connectionResponse?.ok) {
      setError(await responseError(
        providerResponse?.ok ? connectionResponse : providerResponse,
        "관리형 접근 설정을 불러오지 못했습니다.",
      ));
      setLoading(false);
      return;
    }
    const providerBody = await providerResponse.json().catch(() => null);
    const connectionBody = await connectionResponse.json().catch(() => null);
    if (
      !Array.isArray(providerBody?.providers)
      || !Array.isArray(providerBody?.integrations)
      || !Array.isArray(providerBody?.managedConnections)
      || !Array.isArray(connectionBody?.connections)
    ) {
      setError("관리형 접근 응답 형식을 확인하지 못했습니다.");
      setLoading(false);
      return;
    }
    const nextConnections = connectionBody.connections as SharedConnection[];
    const nextIntegrations = providerBody.integrations as Integration[];
    setProviders(providerBody.providers);
    setIntegrations(nextIntegrations);
    setManagedConnections(providerBody.managedConnections);
    setConnections(nextConnections);
    setSelectedConnectionId((current) => (
      nextConnections.some((item) => item.id === current)
        ? current
        : nextConnections[0]?.id ?? ""
    ));
    setSelectedIntegrationId((current) => (
      nextIntegrations.some((item) => item.id === current)
        ? current
        : nextIntegrations[0]?.id ?? ""
    ));
    setError("");
    setLoading(false);
  }, [workspaceId]);

  useEffect(() => {
    const controller = new AbortController();
    void load(controller.signal);
    return () => controller.abort();
  }, [load]);

  const discover = useCallback(async (
    kind: "organizations" | "databases" | "branches",
    integrationId: string,
    values: ResourceSelection,
    signal?: AbortSignal,
  ) => {
    const query = new URLSearchParams({ kind });
    if (values.organization) query.set("organization", values.organization);
    if (values.database) query.set("database", values.database);
    setResourcePending(true);
    const response = await fetch(
      `/api/v1/workspaces/${workspaceId}/provider-integrations/${
        integrationId
      }/resources?${query}`,
      { cache: "no-store", signal },
    ).catch(() => null);
    if (signal?.aborted) return null;
    setResourcePending(false);
    if (!response?.ok) {
      setError(await responseError(response, "공급자 리소스를 불러오지 못했습니다."));
      return null;
    }
    const body = await response.json().catch(() => null);
    if (!Array.isArray(body?.resources)) {
      setError("공급자 리소스 응답 형식을 확인하지 못했습니다.");
      return null;
    }
    setError("");
    return body.resources as Resource[];
  }, [workspaceId]);

  useEffect(() => {
    if (!selectedIntegrationId) {
      setOrganizations([]);
      return;
    }
    const controller = new AbortController();
    void discover(
      "organizations",
      selectedIntegrationId,
      emptySelection,
      controller.signal,
    ).then((rows) => {
      if (rows) setOrganizations(rows);
    });
    return () => controller.abort();
  }, [discover, selectedIntegrationId]);

  async function connect(provider: Provider) {
    if (mutation) return;
    setMutation(`connect:${provider.id}`);
    setError("");
    try {
      const response = await fetch(
        `/api/v1/workspaces/${workspaceId}/provider-integrations`,
        {
          method: "POST",
          headers: { "content-type": "application/json" },
          body: JSON.stringify({ provider: provider.id }),
        },
      ).catch(() => null);
      if (!response?.ok) {
        setError(await responseError(response, "공급자 연결을 시작하지 못했습니다."));
        return;
      }
      const body = await response.json().catch(() => null);
      if (typeof body?.authorizationUrl !== "string") {
        setError("공급자 인증 주소를 확인하지 못했습니다.");
        return;
      }
      window.location.assign(body.authorizationUrl);
    } finally {
      setMutation("");
    }
  }

  async function disconnect(integration: Integration) {
    if (mutation || !window.confirm(
      "연결된 DB는 구성원별 자격증명 모드로 돌아갑니다. 공급자 연결을 해제할까요?",
    )) return;
    setMutation(`disconnect:${integration.id}`);
    setError("");
    try {
      const response = await fetch(
        `/api/v1/workspaces/${workspaceId}/provider-integrations/${integration.id}`,
        { method: "DELETE" },
      ).catch(() => null);
      if (!response?.ok) {
        setError(await responseError(response, "공급자 연결을 해제하지 못했습니다."));
        return;
      }
      setSelection(emptySelection);
      setDatabases([]);
      setBranches([]);
      await load();
    } finally {
      setMutation("");
    }
  }

  async function selectOrganization(organization: string) {
    const next = { organization, database: "", branch: "" };
    setSelection(next);
    setDatabases([]);
    setBranches([]);
    if (!organization || !selectedIntegrationId) return;
    const rows = await discover("databases", selectedIntegrationId, next);
    if (rows) setDatabases(rows);
  }

  async function selectDatabase(database: string) {
    const next = { ...selection, database, branch: "" };
    setSelection(next);
    setBranches([]);
    if (!database || !selectedIntegrationId) return;
    const rows = await discover("branches", selectedIntegrationId, next);
    if (rows) setBranches(rows);
  }

  async function updateMode(mode: "managed" | "member_local") {
    if (!selectedConnection || mutation) return;
    if (
      mode === "managed"
      && (
        !selectedIntegration
        || !selection.organization
        || !selection.database
        || !selection.branch
      )
    ) {
      setError("조직, 데이터베이스, 브랜치를 모두 선택해 주세요.");
      return;
    }
    setMutation(`mode:${selectedConnection.id}`);
    setError("");
    try {
      const response = await fetch(
        `/api/v1/workspaces/${workspaceId}/connections/${
          selectedConnection.id
        }/managed-access`,
        {
          method: "PUT",
          headers: { "content-type": "application/json" },
          body: JSON.stringify(mode === "managed" ? {
            mode,
            integrationId: selectedIntegration!.id,
            resource: {
              ...selection,
              engine: selectedConnection.engine,
            },
          } : { mode }),
        },
      ).catch(() => null);
      if (!response?.ok) {
        setError(await responseError(response, "DB 접근 방식을 변경하지 못했습니다."));
        return;
      }
      await load();
    } finally {
      setMutation("");
    }
  }

  const compatibleDatabases = databases.filter(
    (item) => !selectedConnection || item.kind === selectedConnection.engine,
  );
  const resourceComplete = Boolean(
    selection.organization && selection.database && selection.branch,
  );
  const engineSupported = selectedConnection?.engine === "postgres"
    || selectedConnection?.engine === "mysql";

  return (
    <section className="provider-access-panel">
      <header className="provider-access-heading">
        <div>
          <strong>관리형 DB 접근</strong>
          <small>구성원별 최소 권한 자격증명을 15분 동안만 발급합니다.</small>
        </div>
        <span>암호 저장 없음</span>
      </header>

      {loading ? <p className="provider-empty">공급자 설정을 확인하는 중입니다.</p> : (
        <>
          <div className="provider-catalog">
            {providers.map((provider) => {
              const connected = integrations.some(
                (item) => item.provider === provider.id,
              );
              const available = provider.availability === "available" && provider.configured;
              return (
                <div className="provider-row" key={provider.id}>
                  <div>
                    <strong>{provider.name}</strong>
                    <small>{provider.note}</small>
                  </div>
                  {connected ? <span className="provider-state">연결됨</span> : (
                    <button
                      type="button"
                      disabled={!available || mutation !== ""}
                      onClick={() => void connect(provider)}
                    >
                      {provider.availability === "planned"
                        ? "준비 중"
                        : provider.configured
                          ? "연결"
                          : "서버 설정 필요"}
                    </button>
                  )}
                </div>
              );
            })}
          </div>

          {integrations.length > 0 ? (
            <div className="integration-list">
              {integrations.map((integration) => (
                <div className="integration-row" key={integration.id}>
                  <div>
                    <strong>{integration.displayName}</strong>
                    <small>마지막 확인 {new Date(integration.updatedAt).toLocaleString("ko-KR")}</small>
                  </div>
                  <button
                    type="button"
                    disabled={mutation !== ""}
                    onClick={() => void disconnect(integration)}
                  >
                    연결 해제
                  </button>
                </div>
              ))}
            </div>
          ) : null}

          <div className="managed-access-flow">
            <label>
              <span>1 · 공유 연결</span>
              <select
                value={selectedConnectionId}
                onChange={(event) => {
                  setSelectedConnectionId(event.target.value);
                  setSelection(emptySelection);
                  setDatabases([]);
                  setBranches([]);
                }}
                disabled={connections.length === 0}
              >
                {connections.length === 0 ? <option value="">공유된 DB가 없습니다</option> : null}
                {connections.map((connection) => (
                  <option value={connection.id} key={connection.id}>
                    {connection.name} · {connection.engine} · {
                      connection.credentialMode === "managed" ? "자동 발급" : "개별 입력"
                    }
                  </option>
                ))}
              </select>
            </label>
            <label>
              <span>2 · 공급자 계정</span>
              <select
                value={selectedIntegrationId}
                onChange={(event) => {
                  setSelectedIntegrationId(event.target.value);
                  setSelection(emptySelection);
                  setDatabases([]);
                  setBranches([]);
                }}
                disabled={integrations.length === 0}
              >
                {integrations.length === 0 ? <option value="">먼저 공급자를 연결하세요</option> : null}
                {integrations.map((integration) => (
                  <option value={integration.id} key={integration.id}>
                    {integration.displayName}
                  </option>
                ))}
              </select>
            </label>
            <div className="managed-resource-row">
              <label>
                <span>3 · 조직</span>
                <select
                  value={selection.organization}
                  onChange={(event) => void selectOrganization(event.target.value)}
                  disabled={!selectedIntegration || resourcePending}
                >
                  <option value="">선택</option>
                  {organizations.map((item) => (
                    <option value={item.name} key={item.id}>{item.name}</option>
                  ))}
                </select>
              </label>
              <label>
                <span>DB</span>
                <select
                  value={selection.database}
                  onChange={(event) => void selectDatabase(event.target.value)}
                  disabled={!selection.organization || resourcePending}
                >
                  <option value="">선택</option>
                  {compatibleDatabases.map((item) => (
                    <option value={item.name} key={item.id}>{item.name}</option>
                  ))}
                </select>
              </label>
              <label>
                <span>브랜치</span>
                <select
                  value={selection.branch}
                  onChange={(event) => setSelection({
                    ...selection,
                    branch: event.target.value,
                  })}
                  disabled={!selection.database || resourcePending}
                >
                  <option value="">선택</option>
                  {branches.filter((item) => item.ready !== false).map((item) => (
                    <option value={item.name} key={item.id}>
                      {item.name}{item.production ? " · production" : ""}
                    </option>
                  ))}
                </select>
              </label>
            </div>
            <div className="managed-access-actions ds-control-row">
              <p>
                {!engineSupported && selectedConnection
                  ? "현재 공급자는 PostgreSQL과 MySQL 공유 연결만 지원합니다."
                  : currentManagedConnection
                    ? `현재 ${currentManagedConnection.resource.organization} / ${
                        currentManagedConnection.resource.database
                      } / ${currentManagedConnection.resource.branch}에 자동 연결됩니다.`
                  : selectedConnection?.allowWrites
                  ? "멤버 RBAC에 따라 읽기 전용 또는 읽기·쓰기 권한을 발급합니다."
                  : "이 연결은 모든 구성원에게 읽기 전용 자격증명만 발급합니다."}
              </p>
              <div className="ds-control-row">
                {selectedConnection?.credentialMode === "managed" ? (
                  <button
                    className="muted-button"
                    type="button"
                    disabled={mutation !== ""}
                    onClick={() => void updateMode("member_local")}
                  >
                    개별 자격증명으로 전환
                  </button>
                ) : null}
                <button
                  className="accent-button"
                  type="button"
                  disabled={
                    !selectedConnection
                    || !selectedIntegration
                    || !engineSupported
                    || !resourceComplete
                    || resourcePending
                    || mutation !== ""
                  }
                  onClick={() => void updateMode("managed")}
                >
                  {mutation.startsWith("mode:") ? "적용 중" : "자동 접근 적용"}
                </button>
              </div>
            </div>
          </div>
        </>
      )}
      {error ? <small className="form-error" role="alert">{error}</small> : null}
    </section>
  );
}
