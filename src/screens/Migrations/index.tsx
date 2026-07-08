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
          "Migration analysis timed out. Choose the actual migrations folder instead of a broad project folder.",
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
          if (/timed out/i.test(msg)) localStorage.removeItem(keyFor(connection.id));
        }
        return false;
      } finally {
        if (seq === analyzeSeq.current) setBusy(false);
      }
    },
    [connection.id, persist],
  );

  const run = useCallback(
    async (d: string) => {
      const t = d.trim();
      if (!t) return;
      setDir(t);
      dirRef.current = t;
      const analyzed = await analyze(t);
      if (!analyzed) return;
      try {
        await withTimeout(
          startMigrationWatch(t),
          WATCH_TIMEOUT_MS,
          "Migration folder watch timed out.",
        );
        setWatchErr(null);
      } catch (e) {
        setWatchErr(errMessage(e)); // surface instead of swallowing
      }
    },
    [analyze],
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
        "Migration folder auto-detect timed out. Choose the folder manually.",
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
  }, [connection.id, connection.projectDir, run]);

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
      const verb = confirm.direction === "apply" ? "applied" : "rolled back";
      setConfirm(null);
      setReviewed(false);
      setSuccess(`Migration ${confirm.version} ${verb}.`);
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
        <strong>Migrations</strong>
        <button className="btn small" onClick={onClose} title="Close (Esc)">
          Close
        </button>
      </div>

      <div className="mig-bar">
        <input
          className="mig-dir"
          value={dir}
          onChange={(e) => setDir(e.target.value)}
          onKeyDown={(e) => e.key === "Enter" && void run(dir)}
          placeholder="/path/to/migrations  (auto-filled from the connection's project folder)"
        />
        <button
          className="btn small"
          disabled={busy}
          onClick={() => void pickFolder().then((d) => d && (setDir(d), void run(d)))}
        >
          Browse…
        </button>
        <button className="btn primary small" disabled={busy || !dir.trim()} onClick={() => void run(dir)}>
          {busy ? "Analyzing…" : "Analyze"}
        </button>
        {localStorage.getItem(keyFor(connection.id)) && (
          <button className="btn small" onClick={changeFolder} title="Clear the saved folder and re-detect">
            Change folder
          </button>
        )}
      </div>

      {!connection.projectDir && !report && (
        <p className="muted">
          Tip: set a <strong>Project folder</strong> on this connection (Edit) and DopeDB
          will auto-detect its migrations folder.
        </p>
      )}

      {watchErr && (
        <div className="mig-warn-banner">
          <span>live re-analyze unavailable: {watchErr}</span>
          <button onClick={() => setWatchErr(null)} title="Dismiss" aria-label="Close">
            <Icon name="close" />
          </button>
        </div>
      )}
      {success && <div className="mig-ok">{success}</div>}
      {err && <div className="error">{err}</div>}

      {report && !report.error && (
        <>
          {anyPartial && (
            <div className="mig-note">
              Some migrations were only partially parsed — generated rollback SQL and drift below
              are approximate.
            </div>
          )}

          {drift ? (
            <div className={inSync ? "drift ok" : "drift"}>
              {inSync ? (
                <span>✓ Migrations match the live database schema.</span>
              ) : (
                <>
                  {drift.pendingTables.length > 0 && (
                    <div>
                      <strong>Not in DB (pending):</strong>{" "}
                      {drift.pendingTables.map((t) => (
                        <code key={t}>{t}</code>
                      ))}
                    </div>
                  )}
                  {drift.extraTables.length > 0 && (
                    <div>
                      <strong>In DB, not in migrations:</strong>{" "}
                      {drift.extraTables.map((t) => (
                        <code key={t}>{t}</code>
                      ))}
                    </div>
                  )}
                  {drift.columnDiffs.map((c) => (
                    <div key={c.table}>
                      <strong>{c.table}:</strong>
                      {c.missingInDb.length > 0 && <> missing in DB {c.missingInDb.map((x) => <code key={x}>{x}</code>)}</>}
                      {c.extraInDb.length > 0 && <> extra in DB {c.extraInDb.map((x) => <code key={x}>{x}</code>)}</>}
                    </div>
                  ))}
                </>
              )}
            </div>
          ) : (
            <div className="mig-drift-unavailable">
              DB comparison unavailable — could not compare these migrations against the live
              database.
            </div>
          )}

          <div className="mig-tracker muted">
            {report.tracker ? (
              <>
                Tracking: <strong>{report.tracker}</strong>
                {report.trackerTable && <> ({report.trackerTable})</>}
              </>
            ) : (
              <>No migration tracker detected — applied state is unknown.</>
            )}
          </div>

          <div className="mig-meta muted">
            {migs.length} migrations · {report.dir}
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
                      {m.applied === true && <span className="badge applied">applied</span>}
                      {m.applied === false && <span className="badge pending">pending</span>}
                      {m.applied == null && <span className="badge unknown">unknown</span>}
                      <span className="badge kind">{m.changes.length} changes</span>
                      {m.hasDownFile ? (
                        <span className="badge">has down</span>
                      ) : (
                        <span className="badge risk-medium">no down file</span>
                      )}
                      {m.partialParse && <span className="badge risk-medium">partial parse</span>}
                      {m.parseError && <span className="badge risk-high">parse error</span>}
                    </span>
                  </button>

                  {isOpen && (
                    <div className="mig-body">
                      {m.parseError && <div className="error">Parse: {m.parseError}</div>}
                      <ul className="mig-changes">
                        {m.changes.map((c, i) => (
                          <li key={i} className={c.reversible ? "chg" : "chg manual"}>
                            <span className="chg-sum">{c.summary}</span>
                            {!c.reversible && <span className="chg-flag">manual rollback</span>}
                          </li>
                        ))}
                      </ul>
                      {m.generatedDown ? (
                        <div className="mig-down">
                          <div className="mig-down-head">
                            <span className="label">Generated rollback (down)</span>
                            <button className="btn small" onClick={() => copy(id, m.generatedDown)}>
                              {copied === id ? "Copied" : "Copy"}
                            </button>
                          </div>
                          <pre>{m.generatedDown}</pre>
                        </div>
                      ) : (
                        <div className="muted">No auto-reversible changes in this migration.</div>
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
        <p className="muted">
          Shows a change log per migration, its applied state, an auto-generated rollback (down)
          for each — even for dropped columns/tables — and how the files drift from{" "}
          <strong>{connection.database}</strong>.
        </p>
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
  if (m.applied === true) {
    const enabled = m === rollbackTarget;
    return (
      <div className="mig-actions">
        <button
          className="btn danger small"
          disabled={!enabled || running}
          title={enabled ? undefined : `Only the latest applied migration (${rollbackTarget?.version}) can be rolled back.`}
          onClick={() => onOpen(m.version, "rollback")}
        >
          Roll back
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
        title={enabled ? undefined : `Apply the earliest pending migration (${applyTarget?.version}) first.`}
        onClick={() => onOpen(m.version, "apply")}
      >
        Apply
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
  const script = confirm.direction === "apply" ? m.applyScript : m.rollbackScript;
  const manualLines = (script ?? "").split("\n").filter((l) => l.trimStart().startsWith("-- MANUAL:"));
  const irreversible = m.changes.filter((c) => !c.reversible).map((c) => c.summary);
  const writesDisabled = !!actionErr && /allow_writes|writes are disabled/i.test(actionErr);

  return (
    <div className="mig-confirm">
      <div className="label">
        {confirm.direction === "apply" ? "Apply" : "Roll back"} migration <strong>{m.version}</strong>
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
        <div className="muted">No script available for this migration.</div>
      )}

      <div className="mig-warn">
        <strong>This executes against the live database.</strong>
        {irreversible.length > 0 && (
          <>
            <div>Not automatically reversible:</div>
            <ul>
              {irreversible.map((s, i) => (
                <li key={i}>{s}</li>
              ))}
            </ul>
          </>
        )}
        {manualLines.length > 0 && (
          <div>
            Some tracking bookkeeping is marked <code>-- MANUAL</code> and must be completed by hand
            (highlighted above).
          </div>
        )}
      </div>

      <label className="mig-review">
        <input type="checkbox" checked={reviewed} onChange={(e) => setReviewed(e.target.checked)} />
        I reviewed this script
      </label>

      {actionErr && (
        <div className="error">
          {actionErr}
          {writesDisabled && <div className="mig-hint">Enable writes for this connection in Settings ▸ Safety.</div>}
        </div>
      )}

      <div className="mig-confirm-row">
        <button
          className="btn primary small"
          disabled={!reviewed || running || !script}
          onClick={onExecute}
        >
          {running ? "Executing…" : "Execute"}
        </button>
        <button className="btn small" disabled={running} onClick={onCancel}>
          Cancel
        </button>
      </div>
    </div>
  );
}
