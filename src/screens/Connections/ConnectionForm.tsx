// Connection create/edit form: fields, connection-URL paste import, save/test actions.
// Split out of the old Connections/index.tsx (see DatabaseExplorer.tsx for the sidebar
// tree that used to live alongside it).
import { useState } from "react";
import { useQuery } from "@tanstack/react-query";
import {
  installDriver,
  pickFile,
  testConnectionProfile,
  upsertConnection,
} from "../../ipc/commands";
import type {
  ConnectionProfile,
  DriverDescriptor,
  Engine,
  Provider,
} from "../../ipc/types";
import { errMessage } from "../../ipc/types";
import { Icon } from "../../components/Icon";
import InfoTip from "../../components/InfoTip";
import { useToast } from "../../components/Toast";
import { isDocumentEngine } from "../../lib/capabilities";
import { useI18n } from "../../lib/i18n";
import { driversQuery } from "../../lib/queries";
import "./connections.css";

const DEFAULT_PORT: Record<Engine, number> = {
  postgres: 5432,
  mysql: 3306,
  sqlite: 0,
  mongodb: 27017,
};

const PROVIDER_ORDER: Provider[] = ["auto", "generic", "neon", "planetScale"];

function compatibleDrivers(
  drivers: DriverDescriptor[],
  engine: Engine,
  provider: Provider,
): DriverDescriptor[] {
  return drivers.filter(
    (driver) =>
      driver.engine === engine &&
      (provider === "auto" || driver.supportedProviders.includes(provider)),
  );
}

type ParsedConnectionUrl = {
  update: Partial<ConnectionProfile>;
  password: string | null;
};

function decodeUrlPart(value: string): string {
  try {
    return decodeURIComponent(value);
  } catch {
    return value;
  }
}

function firstSearchParam(params: URLSearchParams, keys: string[]): string | null {
  for (const key of keys) {
    const value = params.get(key);
    if (value != null && value !== "") return value;
  }
  return null;
}

type UrlMetaParams = {
  name: string;
  env: string | null;
  readonlyDefault: boolean | null;
  allowWrites: boolean | null;
};

// name/env/readonlyDefault/allowWrites read the same DopeDB meta keys regardless of
// engine — shared by parseMongoConnectionUrl and parseConnectionUrl. `nameFallback` is
// the engine-specific fallback (database, then host) when no name param is present.
function parseUrlMetaParams(params: URLSearchParams, nameFallback: string): UrlMetaParams {
  return {
    name: firstSearchParam(params, ["name", "connectionName", "connection_name"]) || nameFallback,
    env: firstSearchParam(params, ["env", "environment"]),
    readonlyDefault: parseOptionalBoolean(
      firstSearchParam(params, ["readonly", "readOnly", "read_only"]),
    ),
    allowWrites: parseOptionalBoolean(
      firstSearchParam(params, ["allowWrites", "allow_writes", "writes"]),
    ),
  };
}

function parseOptionalBoolean(value: string | null): boolean | null {
  if (value == null) return null;
  const normalized = value.trim().toLowerCase();
  if (["1", "true", "yes", "on"].includes(normalized)) return true;
  if (["0", "false", "no", "off"].includes(normalized)) return false;
  return null;
}

function normalizeSslMode(engine: Engine, value: string | null): string | null {
  if (!value) return null;
  const normalized = value.trim().toLowerCase().replace(/_/g, "-");
  if (["1", "true", "yes", "on"].includes(normalized)) return "require";
  if (["0", "false", "no", "off"].includes(normalized)) {
    return engine === "mysql" ? "disabled" : "disable";
  }
  if (engine === "mysql") {
    if (normalized === "disable") return "disabled";
    if (normalized === "prefer") return "preferred";
    if (normalized === "require") return "required";
    if (normalized === "verify-full") return "verify-identity";
  }
  return normalized;
}

function sqliteDatabaseFromUrl(url: URL): string {
  if (url.protocol === "file:") return decodeUrlPart(url.pathname);
  if (url.hostname && !url.pathname) return decodeUrlPart(url.hostname);
  if (url.hostname) return decodeUrlPart(`${url.hostname}${url.pathname}`);
  return decodeUrlPart(url.pathname);
}

// mongodb:// and mongodb+srv:// support a comma-separated host list (replica set
// members), which the WHATWG URL parser can't represent — parse by hand instead. The
// schema group is deliberately not read from the URL: MongoDB has no schema-diff path,
// and the form hides that field for this engine (see the isMongo branch below).
function parseMongoConnectionUrl(text: string): ParsedConnectionUrl | null {
  const match =
    /^mongodb(\+srv)?:\/\/(?:([^:@/]+)(?::([^@/]*))?@)?([^/?]+)(?:\/([^?]*))?(?:\?(.*))?$/i.exec(
      text,
    );
  if (!match) return null;
  const [, srvFlag, rawUser, rawPass, hostPart, rawDatabase, rawQuery] = match;
  const srv = !!srvFlag;
  const database = rawDatabase ? decodeUrlPart(rawDatabase) : "";
  const params = new URLSearchParams(rawQuery ?? "");

  // Unlike the SQL engines (whose extraParams are only read selectively), every
  // mongo extraParams entry is re-serialized into the driver URI — DopeDB's own
  // meta parameters must not leak into it as unknown MongoDB options.
  const dopedbMetaKeys = new Set([
    "allow_writes", "allowwrites", "connection_name", "connectionname", "env",
    "environment", "name", "pass", "password", "read_only", "readonly", "writes",
  ]);
  const extraParams: Record<string, string> = {};
  params.forEach((value, key) => {
    if (dopedbMetaKeys.has(key.toLowerCase())) return;
    extraParams[key] = value;
  });
  if (srv) extraParams.srv = "true";

  const hosts = hostPart.split(",").map((h) => h.trim()).filter(Boolean);
  let host: string;
  let port: number;
  if (hosts.length > 1) {
    // Replica-set host list: each member resolves its own port, so keep the raw
    // comma-separated string and fall back to the driver default port.
    host = hostPart;
    port = DEFAULT_PORT.mongodb;
  } else {
    const single = hosts[0] ?? "";
    const m = /^(.+?)(?::(\d+))?$/.exec(single);
    host = decodeUrlPart(m?.[1] ?? single);
    port = m?.[2] ? Number(m[2]) : DEFAULT_PORT.mongodb;
  }

  const meta = parseUrlMetaParams(params, database || hosts[0] || "");
  const update: Partial<ConnectionProfile> = {
    name: meta.name,
    engine: "mongodb",
    provider: "auto",
    driverId: null,
    host,
    port,
    database,
    username: rawUser ? decodeUrlPart(rawUser) : "",
    sslmode: "prefer",
    extraParams,
  };
  if (meta.env) update.env = meta.env;
  if (meta.readonlyDefault != null) update.readonlyDefault = meta.readonlyDefault;
  if (meta.allowWrites != null) update.allowWrites = meta.allowWrites;

  return {
    update,
    password:
      (rawPass != null ? decodeUrlPart(rawPass) : "") ||
      firstSearchParam(params, ["password", "pass"]) ||
      null,
  };
}

function parseConnectionUrl(raw: string): ParsedConnectionUrl | null {
  const text = raw.trim().replace(/^['"`]+|['"`]+$/g, "");
  if (!text) return null;
  if (/^mongodb(\+srv)?:\/\//i.test(text)) return parseMongoConnectionUrl(text);

  let url: URL;
  try {
    url = new URL(text);
  } catch {
    return null;
  }

  const protocol = url.protocol.replace(/:$/, "").toLowerCase();
  const engine: Engine | null =
    protocol === "postgres" || protocol === "postgresql"
      ? "postgres"
      : protocol === "mysql" || protocol === "mariadb"
        ? "mysql"
        : protocol === "sqlite" || protocol === "sqlite3" || protocol === "file"
          ? "sqlite"
          : null;
  if (!engine) return null;

  const extraParams: Record<string, string> = {};
  url.searchParams.forEach((value, key) => {
    if (key.toLowerCase() === "password" || key.toLowerCase() === "pass") return;
    extraParams[key] = value;
  });

  const sslmode = normalizeSslMode(
    engine,
    firstSearchParam(url.searchParams, ["sslmode", "ssl-mode", "sslMode", "ssl"]),
  );
  const database =
    engine === "sqlite"
      ? sqliteDatabaseFromUrl(url)
      : decodeUrlPart(url.pathname.replace(/^\/+/, ""));
  const meta = parseUrlMetaParams(url.searchParams, database || url.hostname || "");
  const update: Partial<ConnectionProfile> = {
    name: meta.name,
    engine,
    provider: "auto",
    driverId: null,
    host: engine === "sqlite" ? "localhost" : decodeUrlPart(url.hostname),
    port: url.port ? Number(url.port) : DEFAULT_PORT[engine],
    database,
    username: decodeUrlPart(url.username),
    sslmode: sslmode ?? "prefer",
    extraParams,
  };
  if (meta.env) update.env = meta.env;
  const schemaGroup = firstSearchParam(url.searchParams, [
    "schemaGroup",
    "schema_group",
    "schema-group",
    "group",
  ]);
  if (schemaGroup) update.schemaGroup = schemaGroup;
  if (meta.readonlyDefault != null) update.readonlyDefault = meta.readonlyDefault;
  if (meta.allowWrites != null) update.allowWrites = meta.allowWrites;

  return {
    update,
    password:
      decodeUrlPart(url.password) ||
      firstSearchParam(url.searchParams, ["password", "pass"]) ||
      null,
  };
}

function blank(): ConnectionProfile {
  return {
    id: crypto.randomUUID(),
    name: "",
    engine: "postgres",
    provider: "auto",
    driverId: null,
    host: "localhost",
    port: 5432,
    database: "",
    username: "",
    sslmode: "prefer",
    extraParams: {},
    readonlyDefault: true,
    allowWrites: false,
    secretRef: null,
    env: null,
    schemaGroup: null,
  };
}

export function ConnectionForm({
  initial,
  onSaved,
  onCancel,
}: {
  initial: ConnectionProfile | null;
  onSaved: (p: ConnectionProfile) => void;
  onCancel: () => void;
}) {
  const { t } = useI18n();
  const toast = useToast();
  const driverCatalog = useQuery(driversQuery());
  const [form, setForm] = useState<ConnectionProfile>(initial ?? blank());
  const [password, setPassword] = useState("");
  const [busy, setBusy] = useState(false);
  // Which action is in flight, so only the clicked button shows progress (busy
  // disables all three).
  const [running, setRunning] = useState<"save" | "test" | null>(null);
  const [installingDriverId, setInstallingDriverId] = useState<string | null>(null);
  const [msg, setMsg] = useState<string | null>(null);
  const [msgErr, setMsgErr] = useState(false);
  const isNew = initial === null;
  function set<K extends keyof ConnectionProfile>(
    key: K,
    value: ConnectionProfile[K],
  ) {
    setForm((f) => ({ ...f, [key]: value }));
  }

  function applyConnectionUrl(raw: string, showFeedback: boolean) {
    const parsed = parseConnectionUrl(raw);
    if (!parsed) return false;
    setForm((current) => ({
      ...current,
      ...parsed.update,
      id: current.id,
      secretRef: current.secretRef,
    }));
    if (parsed.password != null) setPassword(parsed.password);
    setMsg(null);
    setMsgErr(false);
    if (showFeedback) toast(t("connections.clipboardImported"));
    return true;
  }

  async function importConnectionUrlFromClipboard(showFeedback = true) {
    if (!navigator.clipboard?.readText) {
      if (showFeedback) toast(t("connections.clipboardUnavailable"), "error");
      return;
    }
    try {
      const text = await navigator.clipboard.readText();
      const imported = applyConnectionUrl(text, showFeedback);
      if (!imported && showFeedback) {
        toast(t("connections.clipboardNoConnectionUrl"), "error");
      }
    } catch {
      if (showFeedback) toast(t("connections.clipboardUnavailable"), "error");
    }
  }

  async function save() {
    setBusy(true);
    setRunning("save");
    setMsg(null);
    try {
      const saved = await upsertConnection(form, password || undefined);
      setPassword("");
      toast(t("connections.connectionSaved"));
      onSaved(saved);
      setMsg(t("connections.saved"));
      setMsgErr(false);
    } catch (e) {
      setMsg(errMessage(e));
      setMsgErr(true);
    } finally {
      setBusy(false);
      setRunning(null);
    }
  }

  async function test() {
    setBusy(true);
    setRunning("test");
    setMsg(null);
    try {
      // A literal reachability check — dials the current form values WITHOUT
      // saving the connection or storing the secret. Just OK / not OK.
      await testConnectionProfile(form, password || undefined);
      setMsg(`✓ ${t("connections.connectionOk")}`);
      setMsgErr(false);
    } catch (e) {
      setMsg(errMessage(e));
      setMsgErr(true);
    } finally {
      setBusy(false);
      setRunning(null);
    }
  }

  const isSqlite = form.engine === "sqlite";
  const isMongo = form.engine === "mongodb"; // SRV 등 MongoDB URI 고유 폼 필드용 — 문서엔진 일반 분기는 isDocumentEngine
  const srv = form.extraParams.srv === "true";
  function setSrv(checked: boolean) {
    setForm((f) => {
      const extraParams = { ...f.extraParams };
      if (checked) extraParams.srv = "true";
      else delete extraParams.srv;
      return { ...f, extraParams };
    });
  }
  const drivers = compatibleDrivers(driverCatalog.data ?? [], form.engine, form.provider);
  const activeDriver =
    drivers.find((driver) => driver.id === form.driverId) ??
    drivers.find((driver) => driver.recommended) ??
    drivers[0] ??
    null;
  const providers = PROVIDER_ORDER.filter(
    (provider) =>
      provider === "auto" ||
      provider === form.provider ||
      (driverCatalog.data ?? []).some(
        (driver) =>
          driver.engine === form.engine && driver.supportedProviders.includes(provider),
      ),
  );

  async function downloadDriver(driver: DriverDescriptor) {
    setInstallingDriverId(driver.id);
    setMsg(null);
    try {
      await installDriver(driver.id);
      await driverCatalog.refetch();
      setMsg(t("connections.driverInstalled", { name: driver.name }));
      setMsgErr(false);
    } catch (e) {
      setMsg(errMessage(e));
      setMsgErr(true);
    } finally {
      setInstallingDriverId(null);
    }
  }

  function driverStatus(driver: DriverDescriptor): string {
    if (driver.installState === "planned") return t("connections.driverPlanned");
    if (driver.installMode === "bundled") return t("connections.driverBundled");
    if (driver.installState === "installed") {
      return t("connections.driverInstalledStatus");
    }
    return t("connections.driverDownloadRequired");
  }

  return (
    <div
      className="form"
      onKeyDown={(e) => {
        if (
          e.key === "Enter" &&
          (e.target as HTMLElement).tagName === "INPUT" &&
          !busy
        ) {
          e.preventDefault();
          void save();
        } else if (e.key === "Escape") {
          onCancel();
        }
      }}
    >
      <div className="form-head">
        <h2>{isNew ? t("connections.new") : t("connections.edit")}</h2>
        <button
          type="button"
          className="form-close-btn"
          onClick={onCancel}
          title={t("common.close")}
          aria-label={t("common.close")}
        >
          <Icon name="close" />
        </button>
      </div>

      {isNew && (
        <div className="form-import-row">
          <button
            type="button"
            className="btn small"
            disabled={busy}
            onClick={() => void importConnectionUrlFromClipboard(true)}
          >
            <Icon name="copy" />
            {t("connections.importClipboard")}
          </button>
        </div>
      )}

      <label>
        {t("connections.name")}
        <input
          value={form.name}
          onChange={(e) => set("name", e.target.value)}
          placeholder="prod-readonly"
        />
      </label>

      <label>
        {t("connections.engine")}
        <select
          value={form.engine}
          onChange={(e) => {
            const engine = e.target.value as Engine;
            setForm((f) => ({
              ...f,
              engine,
              provider: "auto",
              driverId: null,
              // Keep a user-customized port; only swap when it still matches the
              // outgoing engine's default.
              port: f.port === DEFAULT_PORT[f.engine] ? DEFAULT_PORT[engine] : f.port,
              // MongoDB hides the schema-group field (SQL-only diff feature) — a
              // carried-over value would be invisible yet still block saving.
              schemaGroup: isDocumentEngine(engine) ? null : f.schemaGroup,
            }));
          }}
        >
          <option value="postgres">PostgreSQL</option>
          <option value="mysql">MySQL / MariaDB</option>
          <option value="sqlite">SQLite</option>
          <option value="mongodb">MongoDB</option>
        </select>
      </label>

      <div className="connection-driver-grid">
        <label>
          {t("connections.provider")}
          <select
            value={form.provider}
            onChange={(e) => {
              const provider = e.target.value as Provider;
              setForm((current) => ({ ...current, provider, driverId: null }));
            }}
          >
            {providers.map((provider) => (
              <option key={provider} value={provider}>
                {provider === "auto"
                  ? t("connections.providerAuto")
                  : provider === "generic"
                    ? t("connections.providerGeneric")
                    : provider === "neon"
                      ? t("connections.providerNeon")
                      : t("connections.providerPlanetScale")}
              </option>
            ))}
          </select>
        </label>

        <label>
          <span className="label-with-help">
            {t("connections.driver")}
            <InfoTip label={t("connections.driverHint")} />
          </span>
          <select
            value={form.driverId ?? ""}
            onChange={(e) => set("driverId", e.target.value || null)}
            disabled={driverCatalog.isPending || drivers.length === 0}
          >
            <option value="">{t("connections.driverAutomatic")}</option>
            {drivers.map((driver) => (
              <option key={driver.id} value={driver.id}>
                {driver.name} {driver.version}
              </option>
            ))}
          </select>
        </label>
      </div>

      {activeDriver && (
        <div className="connection-driver-summary">
          <div>
            <strong>{activeDriver.name}</strong>
            <span className="muted">{driverStatus(activeDriver)}</span>
          </div>
          {activeDriver.installMode === "managed" &&
            activeDriver.installState === "available" && (
              <button
                type="button"
                className="btn small"
                disabled={installingDriverId !== null}
                onClick={() => void downloadDriver(activeDriver)}
              >
                {installingDriverId === activeDriver.id
                  ? t("connections.driverDownloading")
                  : t("connections.driverDownload")}
              </button>
            )}
        </div>
      )}

      {isSqlite ? (
        <label>
          {t("connections.databaseFile")}
          <div className="row">
            <input
              className="grow"
              value={form.database}
              onChange={(e) => set("database", e.target.value)}
              placeholder="/path/to/app.db"
            />
            <button
              type="button"
              className="btn small"
              onClick={() => void pickFile().then((f) => f && set("database", f))}
            >
              {t("connections.browse")}
            </button>
          </div>
        </label>
      ) : (
        <>
          <div className="row">
            <label className="grow">
              {t("connections.host")}
              <input
                value={form.host}
                onChange={(e) => set("host", e.target.value)}
              />
            </label>
            <label className="port">
              {t("connections.port")}
              <input
                type="number"
                value={form.port}
                disabled={isMongo && srv}
                onChange={(e) => {
                  // Empty input keeps the previous port instead of silently becoming 0.
                  const v = e.target.value;
                  if (v !== "") set("port", Number(v));
                }}
              />
            </label>
          </div>

          {isMongo && (
            <label className="check">
              <input
                type="checkbox"
                checked={srv}
                onChange={(e) => setSrv(e.target.checked)}
              />
              {t("connections.srv")}
            </label>
          )}

          <label>
            <span className="label-with-help">
              {t("connections.database")}
              {isMongo && <InfoTip label={t("connections.databaseRequiredHint")} />}
            </span>
            <input
              value={form.database}
              required={isMongo}
              onChange={(e) => set("database", e.target.value)}
            />
          </label>

          <div className="row">
            <label className="grow">
              {t("connections.user")}
              <input
                value={form.username}
                onChange={(e) => set("username", e.target.value)}
              />
            </label>
            <label className="grow">
              {t("connections.password")}
              <input
                type="password"
                value={password}
                onChange={(e) => setPassword(e.target.value)}
                placeholder={
                  form.secretRef
                    ? `•••••• (${t("connections.passwordStoredExisting")})`
                    : t("connections.passwordStored")
                }
              />
            </label>
          </div>

          {!isMongo && (
            <label>
              {t("connections.sslMode")}
              <select
                value={form.sslmode}
                onChange={(e) => set("sslmode", e.target.value)}
              >
                <option value="disable">disable</option>
                <option value="prefer">prefer</option>
                <option value="require">require</option>
                <option value="verify-full">verify-full</option>
              </select>
            </label>
          )}
        </>
      )}

      <label>
        <span className="label-with-help">
          {t("connections.environment")}
          <InfoTip label={t("connections.environmentHint")} />
        </span>
        <select
          value={form.env ?? ""}
          onChange={(e) => set("env", e.target.value || null)}
        >
          <option value="">{t("common.none")}</option>
          <option value="dev">dev</option>
          <option value="staging">staging</option>
          <option value="prod">prod</option>
        </select>
      </label>

      {!isMongo && (
        <label>
          {t("connections.schemaGroup")}
          <input
            value={form.schemaGroup ?? ""}
            onChange={(e) => set("schemaGroup", e.target.value.trim() || null)}
            placeholder={t("connections.schemaGroupPlaceholder")}
          />
        </label>
      )}

      <InfoTip label={t("connections.writeAccessHint")} className="connection-write-help" />

      <div className="form-actions">
        <button className="btn primary" disabled={busy} onClick={save}>
          {running === "save" ? t("common.saving") : t("common.save")}
        </button>
        <button className="btn" disabled={busy} onClick={test}>
          {running === "test" ? t("connections.testing") : t("connections.test")}
        </button>
      </div>

      {msg && (
        <div className={msgErr ? "form-msg error" : "form-msg ok"}>{msg}</div>
      )}
    </div>
  );
}
