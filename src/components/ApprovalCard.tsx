// L4 — the human approval gate, as UX. Given a connection + a SQL string, this card
// runs L1 classify + L3 preview, renders the full risk picture, and gates execution:
//   - read-only SELECTs may auto-run when the connection's autoRunReads is on;
//   - writes / DDL / privilege are ALWAYS hard-gated behind an explicit Approve,
//     and Approve is disabled unless the connection allows writes.
// Nothing here is trusted for safety — the Rust core re-enforces every gate (L2).

import { useEffect, useRef, useState } from "react";
import {
  approveOperation,
  cancelQuery,
  proposeSql,
  rejectOperation,
  runSql,
} from "../ipc/commands";
import type {
  Classification,
  Engine,
  ExecOutcome,
  PreviewReport,
  RiskLevel,
  SafetySettings,
  SqlOperationProposal,
} from "../ipc/types";
import { errMessage, isQueryCancellationError } from "../ipc/types";
import { Icon, type IconName } from "./Icon";
import LazySqlViewer from "./LazySqlViewer";
import { useI18n, type I18nKey } from "../lib/i18n";
import "./ApprovalCard.css";

const ENGINE_LABEL: Record<Engine, string> = {
  postgres: "PostgreSQL",
  mysql: "MySQL",
  sqlite: "SQLite",
  mongodb: "MongoDB",
};

function riskClass(risk: RiskLevel): string {
  return `badge risk-${risk}`;
}

const RISK_LABEL: Record<RiskLevel, I18nKey> = {
  low: "approval.riskLow",
  medium: "approval.riskMedium",
  high: "approval.riskHigh",
};

function StatusGlyph({
  label,
  icon = "info",
  tone,
}: {
  label: string;
  icon?: IconName;
  tone?: "ok" | "warning" | "danger";
}) {
  return (
    <span
      className={
        "badge icon-only-badge" +
        (tone === "ok" ? " status-ok" : tone === "danger" ? " status-error" : "")
      }
      title={label}
      aria-label={label}
      role="img"
    >
      <Icon name={icon} />
    </span>
  );
}

export default function ApprovalCard({
  connectionId,
  engine,
  sql,
  safety,
  initialProposal,
  rationale,
  collapseSql = false,
  onExecuted,
  onReject,
}: {
  connectionId: string;
  engine: Engine;
  sql: string;
  safety: SafetySettings;
  initialProposal?: SqlOperationProposal;
  rationale?: string;
  collapseSql?: boolean;
  onExecuted: (outcome: ExecOutcome) => void;
  onReject?: () => void;
}) {
  const { t } = useI18n();
  const [cls, setCls] = useState<Classification | null>(null);
  const [preview, setPreview] = useState<PreviewReport | null>(null);
  const [proposal, setProposal] = useState<SqlOperationProposal | null>(null);
  const [proposalVersion, setProposalVersion] = useState(0);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const [decided, setDecided] = useState<null | "approved" | "rejected">(null);
  const [cancelled, setCancelled] = useState(false);
  const [confirmation, setConfirmation] = useState("");
  // The in-flight query id, so Cancel can signal it. Held in a ref (not state) since
  // execute() reads it synchronously and it never needs to re-render. `cancelledRef`
  // mirrors the flag so execute()'s catch sees it without a stale closure.
  const queryId = useRef<string | null>(null);
  const cancelledRef = useRef(false);
  // Elapsed seconds while a query runs, so a slow query reads differently from a hung one.
  const [elapsed, setElapsed] = useState(0);

  // L1 + L3 whenever the statement changes.
  useEffect(() => {
    let alive = true;
    setCls(null);
    setPreview(null);
    setProposal(null);
    setError(null);
    setDecided(null);
    setCancelled(false);
    setConfirmation("");
    if (!sql.trim()) return;
    (async () => {
      try {
        const p =
          initialProposal && proposalVersion === 0
            ? initialProposal
            : await proposeSql(connectionId, sql);
        if (!alive) return;
        setProposal(p);
        setCls(p.classification);
        setPreview(p.preview);
      } catch (e) {
        if (alive) setError(errMessage(e));
      }
    })();
    return () => {
      alive = false;
    };
  }, [connectionId, initialProposal, proposalVersion, sql]);

  const isRead = cls?.kind === "read";
  const isWrite = !!cls && !isRead;
  const writesBlocked = isWrite && !safety.allowWrites;
  const confirmationPhrase = proposal?.confirmationPhrase ?? null;
  const confirmationMatches =
    confirmationPhrase === null || confirmation === confirmationPhrase;
  // Reads auto-run only when the connection allows it. Target mutations always
  // stay behind an exact Operation approval regardless of legacy saved settings.
  const canAutoRun = isRead && proposal?.autoRun === true;

  async function execute() {
    if (!proposal) return;
    const id = proposal.operationId;
    queryId.current = id;
    cancelledRef.current = false;
    setBusy(true);
    setError(null);
    setCancelled(false);
    try {
      if (proposal.approvalRequired) {
        await approveOperation(
          proposal.operationId,
          proposal.payloadHash,
          confirmationPhrase ? confirmation : undefined,
        );
      }
      const outcome = await runSql(proposal.operationId);
      setDecided("approved");
      onExecuted(outcome);
    } catch (e) {
      // A local cancel click is benign only when Rust confirms a read cancellation.
      // An interrupted write is `outcomeUnknown` and must stay visible.
      if (cancelledRef.current && isQueryCancellationError(e)) setCancelled(true);
      else setError(errMessage(e));
    } finally {
      queryId.current = null;
      setBusy(false);
    }
  }

  async function reject() {
    if (!proposal || busy) return;
    setBusy(true);
    setError(null);
    try {
      if (proposal.approvalRequired) {
        await rejectOperation(proposal.operationId, proposal.payloadHash);
      }
      setDecided("rejected");
      onReject?.();
    } catch (e) {
      setError(errMessage(e));
    } finally {
      setBusy(false);
    }
  }

  function cancel() {
    if (queryId.current) {
      cancelledRef.current = true;
      void cancelQuery(queryId.current);
    }
  }

  // Tick the elapsed counter while busy; reset+clear when done or unmounted.
  useEffect(() => {
    if (!busy) {
      setElapsed(0);
      return;
    }
    const timer = setInterval(() => setElapsed((s) => s + 1), 1000);
    return () => clearInterval(timer);
  }, [busy]);

  // Auto-run reads (per settings) exactly once, after classification lands.
  useEffect(() => {
    if (canAutoRun && decided === null && !busy) {
      void execute();
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [canAutoRun]);

  const previewN =
    preview?.exactRows ?? preview?.estimatedRows ?? null;
  const compact = collapseSql;
  const sqlBlock = <LazySqlViewer value={sql} minHeight={collapseSql ? "56px" : "80px"} />;
  const approvalHead = (
    <div className="approval-head">
      {cls ? (
        <>
          <span className="badge kind">{cls.kind.toUpperCase()}</span>
          <span className={riskClass(cls.risk)}>{t(RISK_LABEL[cls.risk])}</span>
          <span className="badge dialect">{ENGINE_LABEL[engine]}</span>
          {cls.noWhere && (
            <span className="badge nowhere">{t("approval.noWhere")}</span>
          )}
          {cls.statementCount > 1 && (
            <span className="badge nowhere">
              {t("sql.statementCount", { count: cls.statementCount })}
            </span>
          )}
        </>
      ) : (
        <StatusGlyph label={t("approval.checkingSafety")} icon="refresh" />
      )}
    </div>
  );
  const tablesBlock = cls && cls.tables.length > 0 && (
    <div className="tables">
      <span className="label">{t("approval.targetTables")}</span>{" "}
      {cls.tables.map((tbl) => (
        <code key={tbl}>{tbl}</code>
      ))}
    </div>
  );
  const previewBlock = (
    <div className="preview">
      <span className="label">{t("approval.impactPreview")}</span>
      {!preview ? (
        <StatusGlyph label={t("approval.estimatingImpact")} icon="refresh" />
      ) : isWrite && !writesBlocked && previewN === null ? (
        // A runnable write with NO row estimate (skipped over threshold, or an EXPLAIN
        // that yielded no count) means approving a destructive statement blind — surface
        // it. Not for writes-disabled (can't run) or reads (a null estimate is benign).
        <span className="impact-warn">
          {" "}
          <Icon name="alert" /> {t("approval.impactUnknown")}
          {preview.note && <em className="muted"> — {preview.note}</em>}
        </span>
      ) : (
        <span>
          {" "}
          {preview.mode === "explain" && t("approval.modeExplain")}
          {preview.mode === "execRollback" && t("approval.modeExecRollback")}
          {preview.mode === "skipped" && t("approval.modeSkipped")}
          {previewN !== null && (
            <>
              {" — "}
              <strong>{previewN.toLocaleString()}</strong> {t("approval.rows")}
            </>
          )}
          {preview.note && <em className="muted"> — {preview.note}</em>}
        </span>
      )}
    </div>
  );
  const planBlock = preview?.plan && (
    <details className="plan">
      <summary>{t("sql.queryPlan")}</summary>
      <pre>{preview.plan}</pre>
    </details>
  );
  const notesBlock = cls?.notes.map((n, i) => (
    <div key={i} className="note muted">
      - {n}
    </div>
  ));
  const payloadHashBlock = proposal && (
    <div className="note muted">
      {t("approval.payloadHash")} <code>{proposal.payloadHash}</code>
    </div>
  );
  const confirmationBlock = confirmationPhrase && (
    <label className="approval-confirmation">
      <span>
        {t("approval.confirmationPrompt")} <code>{confirmationPhrase}</code>
      </span>
      <input
        value={confirmation}
        onChange={(event) => setConfirmation(event.target.value)}
        placeholder={confirmationPhrase}
        autoComplete="off"
        spellCheck={false}
      />
    </label>
  );
  const compactStatus = writesBlocked
    ? t("approval.writesDisabledCompact")
    : !cls
      ? t("approval.checkingSafety")
      : !preview
        ? t("approval.checkingImpact")
        : previewN !== null
          ? t(
              previewN === 1 ? "approval.rowsInScope" : "approval.rowsInScopePlural",
              { count: previewN.toLocaleString() },
            )
          : t("approval.readyToReview");

  return (
    <div className="card approval">
      {!compact && approvalHead}

      {rationale && (
        <div className="restatement">
          <div className="label">
            {compact ? t("approval.change") : t("approval.review")}
          </div>
          <p>{rationale}</p>
        </div>
      )}

      {compact && (
        <div
          className={
            "badge icon-only-badge" +
            (writesBlocked ? " status-error" : previewN !== null ? " status-ok" : "")
          }
          title={compactStatus}
          aria-label={compactStatus}
          role="img"
        >
          <Icon
            name={
              writesBlocked
                ? "circleSlash"
                : !cls || !preview
                  ? "refresh"
                  : previewN !== null
                    ? "check"
                    : "info"
            }
          />
        </div>
      )}

      {!compact && sqlBlock}
      {!compact && tablesBlock}
      {!compact && previewBlock}
      {!compact && planBlock}
      {!compact && notesBlock}
      {!compact && payloadHashBlock}
      {confirmationBlock}

      {error && <div className="error">{error}</div>}
      {/* Additive, not a terminal branch — the action buttons below stay reachable so a
          cancelled query can simply be run again. */}
      {cancelled && <StatusGlyph label={t("sql.cancelled")} icon="circleSlash" />}

      {decided === "approved" ? (
        <StatusGlyph label={t("approval.executed")} icon="check" tone="ok" />
      ) : decided === "rejected" ? (
        // Not a dead-end: keep the statement visible above and let the user undo the
        // rejection to approve it, rather than forcing a re-issue.
        <div className="approval-actions ds-action-row ds-control-row">
          <StatusGlyph label={t("approval.rejected")} icon="circleSlash" tone="danger" />
          <button
            className="btn"
            onClick={() => {
              setDecided(null);
              setProposalVersion((version) => version + 1);
            }}
          >
            {t("approval.reconsider")}
          </button>
        </div>
      ) : busy ? (
        <div className="approval-actions ds-action-row ds-control-row">
          <StatusGlyph
            label={`${canAutoRun ? t("approval.readOnlyRunning") : t("approval.running")} ${elapsed}s`}
            icon="refresh"
          />
          <button className="btn" onClick={cancel}>
            {t("common.cancel")}
          </button>
        </div>
      ) : canAutoRun && !cancelled ? (
        <StatusGlyph label={t("approval.readOnlyAutoRunning")} icon="play" />
      ) : (
        <div className="approval-actions ds-action-row ds-control-row">
          {writesBlocked && !compact && (
            <div className="error">{t("approval.writesDisabledBody")}</div>
          )}
          <button
            className="btn primary"
            disabled={busy || !proposal || writesBlocked || !confirmationMatches}
            onClick={() => void execute()}
          >
            {isWrite
              ? compact
                ? t("approval.applyChange")
                : t("approval.approveAndRunWrite")
              : t("sql.run")}
          </button>
          <button
            className="btn"
            disabled={busy || !proposal}
            onClick={() => void reject()}
          >
            {t("approval.reject")}
          </button>
        </div>
      )}
      {compact && (
        <div className="review-disclosures">
          <details className="safety-details">
            <summary>{t("approval.safetyDetails")}</summary>
            {approvalHead}
            {tablesBlock}
            {previewBlock}
            {planBlock}
            {notesBlock}
            {payloadHashBlock}
          </details>
          <details className="generated-sql">
            <summary>{t("approval.generatedSql")}</summary>
            {sqlBlock}
          </details>
        </div>
      )}
    </div>
  );
}
