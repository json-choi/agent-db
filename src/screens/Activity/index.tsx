// Unified activity view. Query history stays optimized for replay, while the
// append-only audit log remains available as lazy-loaded security detail.
import {
  useCallback,
  useEffect,
  useMemo,
  useRef,
  useState,
  type SyntheticEvent,
} from "react";
import {
  auditSnapshot as fetchAuditSnapshot,
  auditVerify,
  listHistory,
} from "../../ipc/commands";
import type {
  AuditSnapshot,
  ConnectionProfile,
  HistoryEntry,
} from "../../ipc/types";
import { errMessage } from "../../ipc/types";
import { Icon, type IconName } from "../../components/Icon";
import { useToast } from "../../components/Toast";
import { fullTime, relTime } from "../../lib/relTime";
import { useI18n } from "../../lib/i18n";
import "./activity.css";

const CAP = 200;

function duration(ms: number | null): string {
  if (ms == null) return "—";
  return ms < 1000 ? `${ms} ms` : `${(ms / 1000).toFixed(2)} s`;
}

function firstLine(sql: string): string {
  const line = sql.trim().split("\n")[0];
  return line.length > 120 ? `${line.slice(0, 120)}…` : line;
}

function statusIcon(status: string): IconName {
  if (status === "ok" || status === "success" || status === "done") return "check";
  if (status === "error" || status === "blocked" || status === "failed") return "alert";
  return "info";
}

function short(hash: string | null): string {
  if (!hash) return "∅";
  return hash.length > 12 ? `${hash.slice(0, 12)}…` : hash;
}

export default function Activity({
  connection,
  onLoadSql,
  initialAuditOpen = false,
  onInitialAuditOpenConsumed,
}: {
  connection: ConnectionProfile;
  onLoadSql: (sql: string) => void;
  initialAuditOpen?: boolean;
  onInitialAuditOpenConsumed?: () => void;
}) {
  const { t } = useI18n();
  const toast = useToast();

  const [rows, setRows] = useState<HistoryEntry[]>([]);
  const [historyError, setHistoryError] = useState<string | null>(null);
  const [historyLoading, setHistoryLoading] = useState(true);
  const [refreshing, setRefreshing] = useState(true);
  const [text, setText] = useState("");
  const [statusFilter, setStatusFilter] = useState("");
  const [originFilter, setOriginFilter] = useState("");

  const [verdict, setVerdict] = useState<AuditSnapshot["verdict"] | null>(null);
  const [integrityError, setIntegrityError] = useState<string | null>(null);
  const [auditSnapshot, setAuditSnapshot] = useState<AuditSnapshot | null>(null);
  const [auditDetailsError, setAuditDetailsError] = useState<string | null>(null);
  const [auditDetailsLoading, setAuditDetailsLoading] = useState(false);
  const [auditOpen, setAuditOpen] = useState(initialAuditOpen);

  // Audit rows can be numerous, so verification happens immediately but the full list
  // is fetched only after the disclosure is opened. Once requested, Refresh keeps it
  // synchronized with the verdict.
  const auditListWanted = useRef(initialAuditOpen);
  const auditListInFlight = useRef(false);
  const refreshRequest = useRef(0);
  const auditRequest = useRef(0);

  useEffect(() => {
    if (initialAuditOpen) onInitialAuditOpenConsumed?.();
    // This compatibility callback is intentionally consumed once on mount.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  const refresh = useCallback(async () => {
    const request = ++refreshRequest.current;
    const auditToken = ++auditRequest.current;
    const includeAuditRows = auditListWanted.current;

    setRefreshing(true);
    setHistoryLoading(true);
    setHistoryError(null);
    setIntegrityError(null);
    if (includeAuditRows) {
      auditListInFlight.current = true;
      setAuditDetailsLoading(true);
      setAuditDetailsError(null);
    }

    // Apply the primary query history as soon as it settles; a large hash-chain
    // verification must not hold the replay surface behind it.
    const historyTask = listHistory(connection.id).then(
      (nextRows) => {
        if (request === refreshRequest.current) {
          setRows(nextRows);
          setHistoryLoading(false);
          setRefreshing(false);
        }
      },
      (error) => {
        if (request === refreshRequest.current) {
          setHistoryError(errMessage(error));
          setHistoryLoading(false);
          setRefreshing(false);
        }
      },
    );

    const auditTask = includeAuditRows
      ? fetchAuditSnapshot(connection.id).then(
          (nextSnapshot) => {
            if (auditToken !== auditRequest.current) return;
            setVerdict(nextSnapshot.verdict);
            setAuditSnapshot(nextSnapshot);
          },
          (error) => {
            if (auditToken !== auditRequest.current) return;
            // Keep the last-good snapshot (mirrors the history handler above) —
            // a transient refresh failure must not blank a verified trail.
            setIntegrityError(errMessage(error));
            setAuditDetailsError(errMessage(error));
          },
        )
      : auditVerify(connection.id).then(
          (nextVerdict) => {
            if (auditToken === auditRequest.current) setVerdict(nextVerdict);
          },
          (error) => {
            if (auditToken === auditRequest.current) setIntegrityError(errMessage(error));
          },
        );

    await Promise.allSettled([historyTask, auditTask]);

    // A keyed Activity instance normally handles connection changes; the sequences also
    // prevent slower manual/lazy requests from overwriting newer results.
    if (request !== refreshRequest.current) return;

    if (includeAuditRows && auditToken === auditRequest.current) {
      auditListInFlight.current = false;
      setAuditDetailsLoading(false);
    }
  }, [connection.id]);

  useEffect(() => {
    void refresh();
    return () => {
      refreshRequest.current += 1;
      auditRequest.current += 1;
    };
  }, [refresh]);

  const loadAuditDetails = useCallback(async () => {
    if (auditListInFlight.current || auditSnapshot !== null) return;
    const auditToken = ++auditRequest.current;
    auditListWanted.current = true;
    auditListInFlight.current = true;
    setAuditDetailsLoading(true);
    setAuditDetailsError(null);

    const result = await fetchAuditSnapshot(connection.id).then(
      (nextSnapshot) => ({ ok: true as const, value: nextSnapshot }),
      (error) => ({ ok: false as const, error }),
    );

    if (auditToken !== auditRequest.current) return;

    if (result.ok) {
      setAuditSnapshot(result.value);
      setVerdict(result.value.verdict);
      setIntegrityError(null);
    } else {
      setAuditSnapshot(null);
      setAuditDetailsError(errMessage(result.error));
      setIntegrityError(errMessage(result.error));
    }

    auditListInFlight.current = false;
    setAuditDetailsLoading(false);
  }, [auditSnapshot, connection.id]);

  function handleAuditToggle(event: SyntheticEvent<HTMLDetailsElement>) {
    const open = event.currentTarget.open;
    setAuditOpen(open);
    if (open) {
      auditListWanted.current = true;
      void loadAuditDetails();
    }
  }

  const statuses = useMemo(
    () => [...new Set(rows.map((row) => row.status))].sort(),
    [rows],
  );
  const origins = useMemo(
    () => [...new Set(rows.map((row) => row.origin))].sort(),
    [rows],
  );

  const filtered = rows.filter(
    (row) =>
      (!text || row.sql.toLowerCase().includes(text.toLowerCase())) &&
      (!statusFilter || row.status === statusFilter) &&
      (!originFilter || row.origin === originFilter),
  );
  const shown = filtered.slice(0, CAP);

  function load(sql: string) {
    onLoadSql(sql);
    toast(t("activity.loaded"));
  }

  const auditEntries = auditSnapshot?.entries ?? null;
  const detailVerdict = auditSnapshot?.verdict ?? null;
  // firstBadIndex is oldest-first; the displayed entries are newest-first.
  const tamperedId =
    detailVerdict && !detailVerdict.ok && detailVerdict.firstBadIndex != null && auditEntries
      ? auditEntries[auditEntries.length - 1 - detailVerdict.firstBadIndex]?.id ?? null
      : null;
  const tamperedEntry = tamperedId
    ? auditEntries?.find((entry) => entry.id === tamperedId) ?? null
    : null;

  const chainBroken = verdict !== null && !verdict.ok;
  const tamperedTs =
    chainBroken &&
    detailVerdict &&
    !detailVerdict.ok &&
    detailVerdict.firstBadIndex === verdict?.firstBadIndex
      ? tamperedEntry?.ts ?? null
      : null;
  const integrityTitle = integrityError
    ? t("activity.auditUnavailable")
    : verdict === null
      ? t("activity.auditVerifying")
      : chainBroken
        ? tamperedTs
          ? t("activity.auditChainBrokenAt", { time: relTime(tamperedTs) })
          : t("activity.auditChainBroken")
        : t("activity.auditVerified");
  const integrityTone = integrityError || chainBroken ? "ds-tone-danger" : "ds-tone-trust";
  const integrityIcon: IconName = integrityError || chainBroken ? "alert" : verdict ? "check" : "info";
  const busy = refreshing || auditDetailsLoading;

  return (
    <div className="screen activity">
      <header className="activity-head">
        <div className="activity-heading">
          <h2>{t("activity.title")}</h2>
          <p className="muted">{t("activity.description")}</p>
        </div>
        <button className="btn small" onClick={() => void refresh()} disabled={busy}>
          {busy ? "..." : t("common.refresh")}
        </button>
      </header>

      <details
        className={`activity-integrity ds-card ${integrityTone}`}
        open={auditOpen}
        onToggle={handleAuditToggle}
      >
        <summary id="activity-audit-summary">
          <Icon name={integrityIcon} />
          <span className="activity-integrity-copy">
            <strong
              role={integrityError || chainBroken ? "alert" : "status"}
              aria-live="polite"
            >
              {integrityTitle}
            </strong>
            <span className="muted">
              {integrityError
                ? t("activity.auditVerifyError", { error: integrityError })
                : t("activity.auditDescription")}
            </span>
          </span>
          <span className="activity-integrity-action">
            {auditEntries
              ? t("activity.auditDetailsCount", { count: auditEntries.length })
              : t("activity.auditDetails")}
            <Icon name="chevronRight" className="activity-integrity-chevron" />
          </span>
        </summary>

        <section
          className="activity-audit-panel"
          role="region"
          aria-labelledby="activity-audit-summary"
          tabIndex={0}
        >
          <div className="activity-section-heading">
            <h3>{t("activity.auditTitle")}</h3>
            <p className="muted">{t("activity.auditRecordsDescription")}</p>
          </div>

          {auditDetailsError && (
            <div className="error">
              {t("activity.auditLoadError", { error: auditDetailsError })}
            </div>
          )}
          {auditDetailsLoading && auditEntries === null && (
            <div className="muted loading">{t("common.loading")}</div>
          )}
          {!auditDetailsLoading && auditEntries?.length === 0 && !auditDetailsError && (
            <div className="muted empty">{t("activity.auditEmpty")}</div>
          )}

          {auditEntries && auditEntries.length > 0 && (
            <ul className="activity-audit-list">
              {auditEntries.map((entry) => (
                <li
                  key={entry.id}
                  className={`activity-audit-row${entry.id === tamperedId ? " tampered" : ""}`}
                >
                  {entry.id === tamperedId && (
                    <div className="activity-tampered-label error">
                      <Icon name="alert" />
                      {t("activity.auditTampered")}
                    </div>
                  )}
                  <div className="activity-audit-top">
                    <span className={`badge action action-${entry.action}`}>
                      {entry.action}
                    </span>
                    {entry.kind.toLowerCase() !== entry.action.toLowerCase() && (
                      <span className="badge kind">{entry.kind}</span>
                    )}
                    <span className="muted" title={fullTime(entry.ts)}>
                      {relTime(entry.ts)}
                    </span>
                    {entry.approvedBy && (
                      <span className="muted">
                        {t("activity.auditBy", { name: entry.approvedBy })}
                      </span>
                    )}
                  </div>
                  {entry.agentPrompt && (
                    <div className="activity-audit-prompt muted" title={entry.agentPrompt}>
                      “{entry.agentPrompt.length > 120
                        ? `${entry.agentPrompt.slice(0, 120)}…`
                        : entry.agentPrompt}”
                    </div>
                  )}
                  <code className="activity-audit-sql">{entry.sql}</code>
                  {entry.error && <div className="error">{entry.error}</div>}
                  <div className="activity-audit-chain muted">
                    <span title={entry.prevHash ?? ""}>
                      {t("activity.auditPrev", { hash: short(entry.prevHash) })}
                    </span>
                    {" → "}
                    <span title={entry.hash}>
                      {t("activity.auditHash", { hash: short(entry.hash) })}
                    </span>
                    {entry.affectedEstimate !== null && (
                      <span>
                        {" · "}
                        {t("activity.auditRowsEstimate", { count: entry.affectedEstimate })}
                      </span>
                    )}
                  </div>
                </li>
              ))}
            </ul>
          )}
        </section>
      </details>

      <section className="activity-queries" aria-labelledby="activity-query-title">
        <div className="activity-section-heading">
          <h3 id="activity-query-title">{t("activity.queries")}</h3>
          <p className="muted">{t("activity.queriesDescription")}</p>
        </div>

        {rows.length > 0 && (
          <div className="activity-filters">
            <input
              className="activity-filter-text"
              type="search"
              placeholder={t("activity.filterSql")}
              value={text}
              onChange={(event) => setText(event.target.value)}
            />
            <select
              value={statusFilter}
              onChange={(event) => setStatusFilter(event.target.value)}
            >
              <option value="">{t("activity.allStatuses")}</option>
              {statuses.map((status) => (
                <option key={status} value={status}>
                  {status}
                </option>
              ))}
            </select>
            <select
              value={originFilter}
              onChange={(event) => setOriginFilter(event.target.value)}
            >
              <option value="">{t("activity.allOrigins")}</option>
              {origins.map((origin) => (
                <option key={origin} value={origin}>
                  {origin}
                </option>
              ))}
            </select>
          </div>
        )}

        {historyError && (
          <div className="error">
            {t("activity.historyLoadError", { error: historyError })}
          </div>
        )}
        {historyLoading && rows.length === 0 && !historyError && (
          <div className="muted loading">{t("common.loading")}</div>
        )}
        {!historyLoading && !historyError && rows.length === 0 && (
          <div className="muted empty">
            {t("activity.empty", {
              name: connection.name || t("app.thisConnection"),
            })}
          </div>
        )}

        {shown.length > 0 && (
          <div className="activity-table-scroll">
            <table className="activity-query-table">
              <thead>
                <tr>
                  <th>{t("activity.executed")}</th>
                  <th>{t("activity.origin")}</th>
                  <th>{t("activity.kind")}</th>
                  <th>{t("activity.status")}</th>
                  <th className="num">{t("activity.rows")}</th>
                  <th className="num">{t("activity.duration")}</th>
                  <th>{t("activity.sql")}</th>
                </tr>
              </thead>
              <tbody>
                {shown.map((row) => (
                  <tr
                    key={row.id}
                    className="activity-query-row"
                    role="button"
                    tabIndex={0}
                    onClick={() => load(row.sql)}
                    onKeyDown={(event) => {
                      if (event.key === "Enter" || event.key === " ") {
                        event.preventDefault();
                        load(row.sql);
                      }
                    }}
                    title={t("activity.loadTitle")}
                  >
                    <td className="nowrap muted" title={fullTime(row.executedAt)}>
                      {relTime(row.executedAt)}
                    </td>
                    <td>
                      <span className={`badge origin origin-${row.origin}`}>
                        {row.origin}
                      </span>
                    </td>
                    <td>
                      <span className="badge kind">{row.kind}</span>
                    </td>
                    <td>
                      <span
                        className={`badge status icon-only-badge status-${row.status}`}
                        title={row.error ? `${row.status}: ${row.error}` : row.status}
                        aria-label={row.error ? `${row.status}: ${row.error}` : row.status}
                        role="img"
                      >
                        <Icon name={statusIcon(row.status)} />
                      </span>
                    </td>
                    <td className="num">{row.rowCount ?? "—"}</td>
                    <td className="num">{duration(row.durationMs)}</td>
                    <td className="activity-query-sql" title={row.sql}>
                      <code>{firstLine(row.sql)}</code>
                    </td>
                  </tr>
                ))}
              </tbody>
            </table>
          </div>
        )}

        {rows.length > 0 && filtered.length === 0 && (
          <div className="muted empty">{t("activity.noMatches")}</div>
        )}

        {filtered.length > CAP && (
          <div className="muted activity-query-note">
            {t("activity.matching", { cap: CAP, count: filtered.length })}
          </div>
        )}
      </section>
    </div>
  );
}
