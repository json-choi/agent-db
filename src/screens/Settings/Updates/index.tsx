import { useEffect, useMemo, useState } from "react";
import { getVersion } from "@tauri-apps/api/app";
import { relaunch } from "@tauri-apps/plugin-process";
import { check, type DownloadEvent, type Update } from "@tauri-apps/plugin-updater";
import { Icon, type IconName } from "../../../components/Icon";
import InfoTip from "../../../components/InfoTip";
import { errMessage } from "../../../ipc/types";
import { useI18n } from "../../../lib/i18n";
import "./updates.css";

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

function stateIcon(state: UpdateState): IconName {
  switch (state) {
    case "checking":
      return "refresh";
    case "available":
    case "downloading":
      return "download";
    case "current":
    case "ready":
      return "check";
    case "error":
      return "alert";
    case "idle":
    default:
      return "info";
  }
}

export default function Updates({
  initialUpdate = null,
  onChecked,
}: {
  initialUpdate?: Update | null;
  onChecked?: (update: Update | null) => void;
}) {
  const { t } = useI18n();
  const [state, setState] = useState<UpdateState>(initialUpdate ? "available" : "idle");
  const [currentVersion, setCurrentVersion] = useState<string>(t("common.unknown"));
  const [update, setUpdate] = useState<Update | null>(initialUpdate);
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
      onChecked?.(next);
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
    let alive = true;
    void getVersion().then((version) => {
      if (alive) setCurrentVersion(version);
    });
    if (!initialUpdate) void refresh();
    return () => {
      alive = false;
    };
  }, []);

  useEffect(() => {
    if (!initialUpdate) return;
    setUpdate(initialUpdate);
    setState("available");
    setError(null);
  }, [initialUpdate]);

  const progress = useMemo(() => pct(downloaded, contentLength), [downloaded, contentLength]);
  const releaseNotes = update?.body?.trim();
  const stateLabel =
    state === "available"
      ? t("updates.available")
      : state === "current"
        ? t("updates.current")
        : state === "checking"
          ? t("updates.checking")
          : state === "downloading"
            ? t("updates.downloading")
            : state === "ready"
              ? t("updates.ready")
              : state === "error"
                ? t("updates.error")
                : t("updates.idle");

  return (
    <div className="screen updates">
      <div className="updates-head">
        <div className="updates-title-row">
          <h2>{t("updates.title")}</h2>
          <InfoTip label={t("updates.description")} />
        </div>
        <button className="btn small" disabled={state === "checking" || state === "downloading"} onClick={refresh}>
          {t("updates.checkAgain")}
        </button>
      </div>

      <div className="card update-card">
        <div className="update-row">
          <span className="muted">{t("updates.installedVersion")}</span>
          <strong>{currentVersion}</strong>
        </div>
        <div className="update-row">
          <span className="muted">{t("updates.latestRelease")}</span>
          <strong>
            {update
              ? update.version
              : state === "checking"
                ? t("updates.checking")
                : t("updates.none")}
          </strong>
        </div>
        <div className="update-row">
          <span className="muted">{t("updates.status")}</span>
          <span
            className={`badge update-state icon-only-badge update-${state}`}
            title={stateLabel}
            aria-label={stateLabel}
            role="img"
          >
            <Icon name={stateIcon(state)} />
          </span>
        </div>

        {state === "downloading" && (
          <div className="update-progress">
            <div className="bar">
              <span style={{ width: `${progress ?? 12}%` }} />
            </div>
            <div className="muted">
              {progress == null
                ? t("updates.received", {
                    amount: bytes(downloaded) ?? t("updates.downloadingFallback"),
                  })
                : `${progress}% (${bytes(downloaded)} / ${bytes(contentLength)})`}
            </div>
          </div>
        )}

        {releaseNotes && (
          <details className="release-notes">
            <summary>{t("updates.releaseNotes")}</summary>
            <pre>{releaseNotes}</pre>
          </details>
        )}

        {error && <div className="error">{t("updates.checkFailed", { error })}</div>}

        <div className="form-actions">
          <button
            className="btn primary"
            disabled={!update || state === "checking" || state === "downloading"}
            onClick={install}
          >
            {state === "downloading" ? t("updates.installing") : t("updates.updateAndRelaunch")}
          </button>
          <a className="btn" href="https://github.com/json-choi/dopedb/releases/latest">
            {t("updates.openReleases")}
          </a>
        </div>
      </div>
    </div>
  );
}
