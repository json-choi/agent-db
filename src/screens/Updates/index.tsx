import { getVersion } from "@tauri-apps/api/app";
import { relaunch } from "@tauri-apps/plugin-process";
import { check, type DownloadEvent, type Update } from "@tauri-apps/plugin-updater";
import { useEffect, useMemo, useState } from "react";
import { errMessage } from "../../ipc/types";

type UpdateState =
  | "idle"
  | "checking"
  | "available"
  | "current"
  | "downloading"
  | "ready"
  | "error";

function bytes(n: number | null) {
  if (n == null || !Number.isFinite(n)) return null;
  if (n < 1024 * 1024) return `${Math.max(1, Math.round(n / 1024))} KB`;
  return `${(n / 1024 / 1024).toFixed(1)} MB`;
}

function pct(done: number, total: number | null) {
  if (!total || total <= 0) return null;
  return Math.min(100, Math.round((done / total) * 100));
}

export default function Updates() {
  const [state, setState] = useState<UpdateState>("idle");
  const [currentVersion, setCurrentVersion] = useState<string>("unknown");
  const [update, setUpdate] = useState<Update | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [downloaded, setDownloaded] = useState(0);
  const [contentLength, setContentLength] = useState<number | null>(null);

  async function refresh() {
    setState("checking");
    setError(null);
    setDownloaded(0);
    setContentLength(null);
    try {
      const [version, next] = await Promise.all([getVersion(), check()]);
      setCurrentVersion(version);
      setUpdate(next);
      setState(next ? "available" : "current");
    } catch (e) {
      setUpdate(null);
      setState("error");
      setError(errMessage(e));
    }
  }

  async function install() {
    if (!update) return;
    setState("downloading");
    setError(null);
    setDownloaded(0);
    setContentLength(null);
    try {
      await update.downloadAndInstall((event: DownloadEvent) => {
        if (event.event === "Started") {
          setContentLength(event.data.contentLength ?? null);
          setDownloaded(0);
        } else if (event.event === "Progress") {
          setDownloaded((n) => n + event.data.chunkLength);
        } else if (event.event === "Finished") {
          setState("ready");
        }
      });
      setState("ready");
      await relaunch();
    } catch (e) {
      setState("error");
      setError(errMessage(e));
    }
  }

  useEffect(() => {
    void refresh();
  }, []);

  const progress = useMemo(() => pct(downloaded, contentLength), [downloaded, contentLength]);
  const releaseNotes = update?.body?.trim();

  return (
    <div className="screen updates">
      <div className="updates-head">
        <div>
          <h2>App updates</h2>
          <p className="muted">
            Checks GitHub Releases for a signed dopedb update package.
          </p>
        </div>
        <button className="btn small" disabled={state === "checking" || state === "downloading"} onClick={refresh}>
          Check again
        </button>
      </div>

      <div className="card update-card">
        <div className="update-row">
          <span className="muted">Installed version</span>
          <strong>{currentVersion}</strong>
        </div>
        <div className="update-row">
          <span className="muted">Latest release</span>
          <strong>{update ? update.version : state === "checking" ? "checking..." : "none"}</strong>
        </div>
        <div className="update-row">
          <span className="muted">Status</span>
          <span className={`badge update-state update-${state}`}>
            {state === "available"
              ? "update available"
              : state === "current"
                ? "up to date"
                : state}
          </span>
        </div>

        {state === "downloading" && (
          <div className="update-progress">
            <div className="bar">
              <span style={{ width: `${progress ?? 12}%` }} />
            </div>
            <div className="muted">
              {progress == null
                ? `${bytes(downloaded) ?? "Downloading"} received`
                : `${progress}% (${bytes(downloaded)} / ${bytes(contentLength)})`}
            </div>
          </div>
        )}

        {releaseNotes && (
          <details className="release-notes">
            <summary>Release notes</summary>
            <pre>{releaseNotes}</pre>
          </details>
        )}

        {error && <div className="error">Update check failed: {error}</div>}

        <div className="form-actions">
          <button
            className="btn primary"
            disabled={!update || state === "checking" || state === "downloading"}
            onClick={install}
          >
            {state === "downloading" ? "Installing..." : "Update and relaunch"}
          </button>
          <a className="btn" href="https://github.com/json-choi/dopedb/releases/latest">
            Open GitHub Releases
          </a>
        </div>
      </div>
    </div>
  );
}
