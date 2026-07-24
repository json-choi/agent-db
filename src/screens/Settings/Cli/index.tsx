// User-consented installation of the version-matched CLI sidecar. Read state stays in
// the app-wide query cache; installation updates that cache only after the backend's
// atomic copy and optional user-PATH edit both return a receipt.
import { useState } from "react";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import { installCli } from "../../../ipc/commands";
import { errMessage } from "../../../ipc/types";
import ConfirmButton from "../../../components/ConfirmButton";
import InfoTip from "../../../components/InfoTip";
import Skeleton from "../../../components/Skeleton";
import { useToast } from "../../../components/Toast";
import { useI18n } from "../../../lib/i18n";
import { cliInstallationStatusQuery, qk } from "../../../lib/queries";
import "./cli.css";

export default function CliSettings() {
  const { t } = useI18n();
  const toast = useToast();
  const queryClient = useQueryClient();
  const statusQ = useQuery(cliInstallationStatusQuery());
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const status = statusQ.data ?? null;

  async function install() {
    if (!status) return;
    setBusy(true);
    setError(null);
    try {
      const receipt = await installCli(
        status.pathChangeRequired && status.pathChangeSupported,
        status.conflict,
      );
      queryClient.setQueryData(qk.cliInstallation(), receipt.status);
      toast(
        receipt.pathChanged
          ? t("cli.installedWithPath")
          : receipt.binaryChanged
            ? t("cli.installed")
            : t("cli.alreadyCurrent"),
      );
    } catch (reason) {
      const message = errMessage(reason);
      setError(message);
      toast(message, "error");
    } finally {
      setBusy(false);
    }
  }

  const ready = !!status?.current && !!status?.pathConfigured;
  const actionLabel = status?.current
    ? status.pathChangeRequired
      ? t("cli.configurePath")
      : t("cli.reinstall")
    : status?.installed
      ? t("cli.update")
      : t("cli.install");

  return (
    <div className="screen cli-settings">
      <div className="settings-title-row">
        <h2>{t("cli.title")}</h2>
        <InfoTip label={t("cli.description")} />
      </div>

      {(error || statusQ.error) && (
        <div className="error">
          {t("cli.error", { error: error ?? errMessage(statusQ.error) })}
        </div>
      )}
      {!status && statusQ.isPending ? (
        <Skeleton lines={4} />
      ) : (
        status && (
          <>
            <div className="cli-status-list">
              <div>
                <span className="muted">{t("cli.version")}</span>
                <strong>{status.version}</strong>
              </div>
              <div>
                <span className="muted">{t("cli.inAppPath")}</span>
                <code>{status.inAppDirectory ?? t("common.unknown")}</code>
              </div>
              <div>
                <span className="muted">{t("cli.installPath")}</span>
                <code>{status.installPath}</code>
              </div>
              <div>
                <span className="muted">{t("cli.binaryStatus")}</span>
                <strong>
                  {status.current
                    ? t("cli.current")
                    : status.installed
                      ? t("cli.outdated")
                      : t("cli.notInstalled")}
                </strong>
              </div>
              <div>
                <span className="muted">{t("cli.pathStatus")}</span>
                <strong>
                  {status.pathConfigured ? t("cli.pathReady") : t("cli.pathMissing")}
                </strong>
              </div>
            </div>

            {status.conflict && (
              <div className="error">{t("cli.conflict", { path: status.installPath })}</div>
            )}
            {status.pathChangePreview && (
              <div className="cli-path-change">
                <strong>{t("cli.pathChange")}</strong>
                <p className="muted">{t("cli.pathConsent")}</p>
                <pre>{status.pathChangePreview}</pre>
              </div>
            )}

            <div className="form-actions ds-control-row">
              {status.conflict ? (
                <ConfirmButton
                  className="btn primary"
                  disabled={busy || !status.bundledAvailable}
                  confirmLabel={t("cli.replaceConfirm")}
                  onConfirm={() => void install()}
                >
                  {actionLabel}
                </ConfirmButton>
              ) : (
                <button
                  className="btn primary"
                  disabled={busy || !status.bundledAvailable || ready}
                  onClick={() => void install()}
                >
                  {busy ? t("cli.working") : ready ? t("cli.ready") : actionLabel}
                </button>
              )}
              <button
                className="btn"
                disabled={busy || statusQ.isFetching}
                onClick={() => {
                  setError(null);
                  void statusQ.refetch();
                }}
              >
                {t("cli.refresh")}
              </button>
            </div>
          </>
        )
      )}
    </div>
  );
}
