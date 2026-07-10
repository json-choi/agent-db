// Migration change-log + applied-state manager. Resolves the migrations folder from the
// connection's project folder (auto-detecting common ORM layouts, remembering a per-connection
// override that is invalidated when the project folder changes), shows the change log,
// auto-generated rollback SQL, drift vs the live DB, and — when a tracker is detected — the
// applied state plus in-app apply/rollback of the earliest-pending / latest-applied migration.
import { useCallback, useEffect, useRef, useState } from "react";
import { listen } from "@tauri-apps/api/event";
import {
  analyzeMigrations,
  detectMigrationsDir,
  pickFolder,
  runMigrationScript,
  startMigrationWatch,
} from "../../ipc/commands";
import type { ConnectionProfile, MigrationReport, MigrationView } from "../../ipc/types";
import { errMessage } from "../../ipc/types";
import { Icon } from "../../components/Icon";
import InfoTip from "../../components/InfoTip";
import { useI18n } from "../../lib/i18n";
import "./migrations.css";

const keyFor = (id: string) => `dopedb.migrationsDir.${id}`;
const ANALYZE_TIMEOUT_MS = 12_000;
const DETECT_TIMEOUT_MS = 5_000;
const WATCH_TIMEOUT_MS = 3_000;

function withTimeout<T>(promise: Promise<T>, ms: number, message: string): Promise<T> {
  let timer: number | undefined;
  const timeout = new Promise<never>((_, reject) => {
    timer = window.setTimeout(() => reject(new Error(message)), ms);
  });
  return Promise.race([promise, timeout]).finally(() => window.clearTimeout(timer));
}

// Stored override remembers which projectDir it was derived from, so editing the
// connection's projectDir re-triggers detection instead of being short-circuited forever.
type Saved = { dir: string; projectDir: string | null };
function loadSaved(id: string): Saved | null {
  const raw = localStorage.getItem(keyFor(id));
  if (!raw) return null;
  try {
    const v = JSON.parse(raw);
    if (v && typeof v.dir === "string") return { dir: v.dir, projectDir: v.projectDir ?? null };
  } catch {
    /* legacy plain-string value — ignore, re-detect */
  }
  return null;
}

type Confirm = { version: string; direction: "apply" | "rollback" };

export default function Migrations({
  connection,
  onClose,
}: {
  connection: ConnectionProfile;
  onClose: () => void;
}) {
  const { t } = useI18n();
  const [dir, setDir] = useState("");
  const [report, setReport] = useState<MigrationReport | null>(null);
  const [err, setErr] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const [open, setOpen] = useState<Record<string, boolean>>({});
  const [copied, setCopied] = useState<string | null>(null);
  const [watchErr, setWatchErr] = useState<string | null>(null);
  const [confirm, setConfirm] = useState<Confirm | null>(null);
  const [reviewed, setReviewed] = useState(false);
  const [running, setRunning] = useState(false);
  const [actionErr, setActionErr] = useState<string | null>(null);
  const [success, setSuccess] = useState<string | null>(null);
  const dirRef = useRef("");
  const analyzeSeq = useRef(0);

  const persist = useCallback(() => {
    if (!dirRef.current) return;
    localStorage.setItem(
      keyFor(connection.id),
      JSON.stringify({ dir: dirRef.current, projectDir: connection.projectDir ?? null }),
    );
  }, [connection.id, connection.projectDir]);

  const analyze = useCallback(
    async (d: string) => {
      if (!d.trim()) return false;
      const seq = ++analyzeSeq.current;
      setBusy(true);
      setErr(null);
      try {
        const r = await withTimeout(
          analyzeMigrations(d, connection.id),
          ANALYZE_TIMEOUT_MS,
          t("migrations.analyzeTimeout"),
        );
        if (seq !== analyzeSeq.current) return false;
        setReport(r);
        if (r.error) setErr(r.error);
        else persist(); // only remember a folder that actually analyzed
        return !r.error;
      } catch (e) {
        if (seq === analyzeSeq.current) {
          const msg = errMessage(e);
          setErr(msg);
          if (msg === t("migrations.analyzeTimeout")) localStorage.removeItem(keyFor(connection.id));
        }
        return false;
      } finally {
        if (seq === analyzeSeq.current) setBusy(false);
      }
    },
    [connection.id, persist, t],
  );

  const run = useCallback(
    async (d: string) => {
      const trimmed = d.trim();
      if (!trimmed) return;
      setDir(trimmed);
      dirRef.current = trimmed;
      const analyzed = await analyze(trimmed);
      if (!analyzed) return;
      try {
        await withTimeout(
          startMigrationWatch(trimmed),
          WATCH_TIMEOUT_MS,
          t("migrations.watchTimeout"),
        );
        setWatchErr(null);
      } catch (e) {
        setWatchErr(errMessage(e)); // surface instead of swallowing
      }
    },
    [analyze, t],
  );

  // Resolve the folder for the current connection: a stored override still matching the
  // connection's projectDir > auto-detect from projectDir > projectDir itself.
  const resolve = useCallback(() => {
    setReport(null);
    setErr(null);
    setWatchErr(null);
    setConfirm(null);
    const saved = loadSaved(connection.id);
    if (saved && saved.dir && saved.projectDir === (connection.projectDir ?? null)) {
      void run(saved.dir);
      return;
    }
    setDir("");
    dirRef.current = "";
    if (connection.projectDir) {
      void withTimeout(
        detectMigrationsDir(connection.projectDir),
        DETECT_TIMEOUT_MS,
        t("migrations.detectTimeout"),
      )
        .then((found) => {
          const target = found ?? connection.projectDir ?? "";
          setDir(target);
          dirRef.current = target;
          if (found) void run(found);
        })
        .catch((e) => {
          setErr(errMessage(e));
          setDir(connection.projectDir ?? "");
        });
    }
  }, [connection.id, connection.projectDir, run, t]);

  useEffect(() => {
    resolve();
  }, [resolve]);

  // Clear the remembered override and re-detect from the connection's projectDir.
  const changeFolder = useCallback(() => {
    localStorage.removeItem(keyFor(connection.id));
    resolve();
  }, [connection.id, resolve]);

  // Re-analyze (debounced) whenever the backend reports a file change.
  useEffect(() => {
    let t: number | undefined;
    const p = listen("migrations:changed", () => {
      window.clearTimeout(t);
      t = window.setTimeout(() => void analyze(dirRef.current), 400);
    }).catch((e) => console.error("migrations watch listen failed:", e));
    return () => {
      window.clearTimeout(t);
      void p.then((u) => u && u());
    };
  }, [analyze]);

  // Esc closes the confirmation panel if open, otherwise the screen.
  useEffect(() => {
    const h = (e: KeyboardEvent) => {
      if (e.key !== "Escape") return;
      if (confirm) setConfirm(null);
      else onClose();
    };
    window.addEventListener("keydown", h);
    return () => window.removeEventListener("keydown", h);
  }, [confirm, onClose]);

  const copy = (id: string, text: string) => {
    void navigator.clipboard.writeText(text);
    setCopied(id);
    setTimeout(() => setCopied(null), 1200);
  };

  const openConfirm = (version: string, direction: "apply" | "rollback") => {
    setConfirm({ version, direction });
    setReviewed(false);
    setActionErr(null);
  };

  const execute = async () => {
    if (!confirm) return;
    setRunning(true);
    setActionErr(null);
    try {
      await runMigrationScript(connection.id, dirRef.current, confirm.version, confirm.direction, true);
      setConfirm(null);
      setReviewed(false);
      setSuccess(
        t(confirm.direction === "apply" ? "migrations.appliedSuccess" : "migrations.rolledBackSuccess", {
          version: confirm.version,
        }),
      );
      await analyze(dirRef.current); // re-analyze so the applied badge flips
      setTimeout(() => setSuccess(null), 5000);
    } catch (e) {
      setActionErr(errMessage(e)); // backend error, verbatim
    } finally {
      setRunning(false);
    }
  };

  const drift = report?.drift;
  const inSync =
    drift &&
    drift.pendingTables.length === 0 &&
    drift.extraTables.length === 0 &&
    drift.columnDiffs.length === 0;

  const migs = report?.migrations ?? [];
  const applyTarget = migs.find((m) => m.applied !== true); // earliest pending / unknown
  const rollbackTarget = [...migs].reverse().find((m) => m.applied === true); // latest applied
  const anyPartial = migs.some((m) => m.partialParse);

  return (
    <div className="screen migrations">
      <div className="mig-top">
        <strong>{t("migrations.title")}</strong>
        <button className="btn small" onClick={onClose} title={t("migrations.closeTitle")}>
          {t("common.close")}
        </button>
      </div>

      <div className="mig-bar">
        <input
          className="mig-dir"
          value={dir}
          onChange={(e) => setDir(e.target.value)}
          onKeyDown={(e) => e.key === "Enter" && void run(dir)}
          placeholder={t("migrations.dirPlaceholder")}
        />
        <button
          className="btn small"
          disabled={busy}
          onClick={() => void pickFolder().then((d) => d && (setDir(d), void run(d)))}
        >
          {t("migrations.browse")}
        </button>
        <button className="btn primary small" disabled={busy || !dir.trim()} onClick={() => void run(dir)}>
          {busy ? t("migrations.analyzing") : t("migrations.analyze")}
        </button>
        {localStorage.getItem(keyFor(connection.id)) && (
          <button className="btn small" onClick={changeFolder} title={t("migrations.changeFolderTitle")}>
            {t("migrations.changeFolder")}
          </button>
        )}
      </div>

      {!connection.projectDir && !report && (
        <InfoTip label={t("migrations.projectDirTip")} />
      )}

      {watchErr && (
        <div className="mig-warn-banner">
          <span>{t("migrations.watchUnavailable", { error: watchErr })}</span>
          <button onClick={() => setWatchErr(null)} title={t("migrations.dismiss")} aria-label={t("common.close")}>
            <Icon name="close" />
          </button>
        </div>
      )}
      {success && <div className="mig-ok">{success}</div>}
      {err && <div className="error">{err}</div>}

      {report && !report.error && (
        <>
          {anyPartial && <div className="mig-note">{t("migrations.partialParseNote")}</div>}

          {drift ? (
            <div className={inSync ? "drift ok" : "drift"}>
              {inSync ? (
                <span>{t("migrations.inSync")}</span>
              ) : (
                <>
                  {drift.pendingTables.length > 0 && (
                    <div>
                      <strong>{t("migrations.pendingTablesLabel")}</strong>{" "}
                      {drift.pendingTables.map((tbl) => (
                        <code key={tbl}>{tbl}</code>
                      ))}
                    </div>
                  )}
                  {drift.extraTables.length > 0 && (
                    <div>
                      <strong>{t("migrations.extraTablesLabel")}</strong>{" "}
                      {drift.extraTables.map((tbl) => (
                        <code key={tbl}>{tbl}</code>
                      ))}
                    </div>
                  )}
                  {drift.columnDiffs.map((c) => (
                    <div key={c.table}>
                      <strong>{c.table}:</strong>
                      {c.missingInDb.length > 0 && <> {t("migrations.missingInDb")} {c.missingInDb.map((x) => <code key={x}>{x}</code>)}</>}
                      {c.extraInDb.length > 0 && <> {t("migrations.extraInDb")} {c.extraInDb.map((x) => <code key={x}>{x}</code>)}</>}
                    </div>
                  ))}
                </>
              )}
            </div>
          ) : (
            <div className="mig-drift-unavailable">{t("migrations.driftUnavailable")}</div>
          )}

          <div className="mig-tracker muted">
            {report.tracker ? (
              <>
                {t("migrations.trackingLabel")} <strong>{report.tracker}</strong>
                {report.trackerTable && <> ({report.trackerTable})</>}
              </>
            ) : (
              <>{t("migrations.noTracker")}</>
            )}
          </div>

          <div className="mig-meta muted">
            {t("migrations.metaLine", { count: migs.length, dir: report.dir })}
          </div>

          <ul className="mig-list">
            {migs.map((m) => {
              const id = `${m.version}/${m.name}`;
              const isOpen = open[id];
              return (
                <li key={id} className="mig-item">
                  <button className="mig-head" onClick={() => setOpen((o) => ({ ...o, [id]: !o[id] }))}>
                    <span className="tw"><Icon name={isOpen ? "chevronDown" : "chevronRight"} /></span>
                    <span className="mig-ver">{m.version}</span>
                    <span className="mig-name">{m.name}</span>
                    <span className="mig-badges">
                      {m.applied === true && <span className="badge applied">{t("migrations.appliedBadge")}</span>}
                      {m.applied === false && <span className="badge pending">{t("migrations.pendingBadge")}</span>}
                      {m.applied == null && <span className="badge unknown">{t("common.unknown")}</span>}
                      <span className="badge kind">{t("migrations.changesCount", { count: m.changes.length })}</span>
                      {m.hasDownFile ? (
                        <span className="badge">{t("migrations.hasDown")}</span>
                      ) : (
                        <span className="badge risk-medium">{t("migrations.noDownFile")}</span>
                      )}
                      {m.partialParse && <span className="badge risk-medium">{t("migrations.partialParseBadge")}</span>}
                      {m.parseError && <span className="badge risk-high">{t("migrations.parseErrorBadge")}</span>}
                    </span>
                  </button>

                  {isOpen && (
                    <div className="mig-body">
                      {m.parseError && (
                        <div className="error">
                          {t("migrations.parseErrorLabel")} {m.parseError}
                        </div>
                      )}
                      <ul className="mig-changes">
                        {m.changes.map((c, i) => (
                          <li key={i} className={c.reversible ? "chg" : "chg manual"}>
                            <span className="chg-sum">{c.summary}</span>
                            {!c.reversible && <span className="chg-flag">{t("migrations.manualRollbackFlag")}</span>}
                          </li>
                        ))}
                      </ul>
                      {m.generatedDown ? (
                        <div className="mig-down">
                          <div className="mig-down-head">
                            <span className="label">{t("migrations.generatedRollbackLabel")}</span>
                            <button className="btn small" onClick={() => copy(id, m.generatedDown)}>
                              {copied === id ? t("common.copied") : t("common.copy")}
                            </button>
                          </div>
                          <pre>{m.generatedDown}</pre>
                        </div>
                      ) : (
                        <div className="muted">{t("migrations.noAutoReversible")}</div>
                      )}

                      <ActionRow
                        m={m}
                        applyTarget={applyTarget}
                        rollbackTarget={rollbackTarget}
                        running={running}
                        onOpen={openConfirm}
                      />

                      {confirm?.version === m.version && (
                        <ConfirmPanel
                          m={m}
                          confirm={confirm}
                          reviewed={reviewed}
                          setReviewed={setReviewed}
                          running={running}
                          actionErr={actionErr}
                          onCancel={() => setConfirm(null)}
                          onExecute={() => void execute()}
                        />
                      )}
                    </div>
                  )}
                </li>
              );
            })}
          </ul>
        </>
      )}

      {!report && !err && (
        <InfoTip
          label={t("migrations.reportOverviewTip", { database: connection.database })}
        />
      )}
    </div>
  );
}

function ActionRow({
  m,
  applyTarget,
  rollbackTarget,
  running,
  onOpen,
}: {
  m: MigrationView;
  applyTarget?: MigrationView;
  rollbackTarget?: MigrationView;
  running: boolean;
  onOpen: (version: string, direction: "apply" | "rollback") => void;
}) {
  const { t } = useI18n();
  if (m.applied === true) {
    const enabled = m === rollbackTarget;
    return (
      <div className="mig-actions">
        <button
          className="btn danger small"
          disabled={!enabled || running}
          title={
            enabled
              ? undefined
              : t("migrations.rollbackOnlyLatestHint", { version: rollbackTarget?.version ?? "" })
          }
          onClick={() => onOpen(m.version, "rollback")}
        >
          {t("migrations.rollback")}
        </button>
      </div>
    );
  }
  const enabled = m === applyTarget;
  return (
    <div className="mig-actions">
      <button
        className="btn small"
        disabled={!enabled || running}
        title={
          enabled ? undefined : t("migrations.applyEarliestHint", { version: applyTarget?.version ?? "" })
        }
        onClick={() => onOpen(m.version, "apply")}
      >
        {t("migrations.apply")}
      </button>
    </div>
  );
}

function ConfirmPanel({
  m,
  confirm,
  reviewed,
  setReviewed,
  running,
  actionErr,
  onCancel,
  onExecute,
}: {
  m: MigrationView;
  confirm: Confirm;
  reviewed: boolean;
  setReviewed: (v: boolean) => void;
  running: boolean;
  actionErr: string | null;
  onCancel: () => void;
  onExecute: () => void;
}) {
  const { t } = useI18n();
  const script = confirm.direction === "apply" ? m.applyScript : m.rollbackScript;
  const manualLines = (script ?? "").split("\n").filter((l) => l.trimStart().startsWith("-- MANUAL:"));
  const irreversible = m.changes.filter((c) => !c.reversible).map((c) => c.summary);
  const writesDisabled = !!actionErr && /allow_writes|writes are disabled/i.test(actionErr);

  return (
    <div className="mig-confirm">
      <div className="label">
        {t(
          confirm.direction === "apply" ? "migrations.applyMigrationLabel" : "migrations.rollbackMigrationLabel",
          { version: m.version },
        )}
      </div>

      {script ? (
        <pre className="mig-script">
          {script.split("\n").map((ln, i) => (
            <span key={i} className={ln.trimStart().startsWith("-- MANUAL:") ? "mig-manual-line" : undefined}>
              {ln + "\n"}
            </span>
          ))}
        </pre>
      ) : (
        <div className="muted">{t("migrations.noScriptAvailable")}</div>
      )}

      <div className="mig-warn">
        <strong>{t("migrations.executesLiveWarning")}</strong>
        {irreversible.length > 0 && (
          <>
            <div>{t("migrations.notReversibleLabel")}</div>
            <ul>
              {irreversible.map((s, i) => (
                <li key={i}>{s}</li>
              ))}
            </ul>
          </>
        )}
        {manualLines.length > 0 && (
          <div>
            {t("migrations.manualBookkeepingNotePrefix")}
            <code>-- MANUAL</code>
            {t("migrations.manualBookkeepingNoteSuffix")}
          </div>
        )}
      </div>

      <label className="mig-review">
        <input type="checkbox" checked={reviewed} onChange={(e) => setReviewed(e.target.checked)} />
        {t("migrations.reviewedLabel")}
      </label>

      {actionErr && (
        <div className="error">
          {actionErr}
          {writesDisabled && <div className="mig-hint">{t("migrations.enableWritesHint")}</div>}
        </div>
      )}

      <div className="mig-confirm-row ds-action-row">
        <button
          className="btn primary small"
          disabled={!reviewed || running || !script}
          onClick={onExecute}
        >
          {running ? t("migrations.executing") : t("migrations.execute")}
        </button>
        <button className="btn small" disabled={running} onClick={onCancel}>
          {t("common.cancel")}
        </button>
      </div>
    </div>
  );
}
