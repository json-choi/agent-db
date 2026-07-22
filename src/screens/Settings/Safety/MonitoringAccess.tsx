// Compact PostgreSQL monitoring-role control. The backend owns the fixed GRANT/REVOKE
// and MCP safety decision; this panel only exposes status, explicit confirmation, and
// a DBA-copy fallback without turning on arbitrary database writes.
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { setPostgresMonitoring } from "../../../ipc/commands";
import { errMessage } from "../../../ipc/types";
import ConfirmButton from "../../../components/ConfirmButton";
import { Icon } from "../../../components/Icon";
import Skeleton from "../../../components/Skeleton";
import { useToast } from "../../../components/Toast";
import { monitoringStatusQuery, qk } from "../../../lib/queries";
import { useI18n } from "../../../lib/i18n";

const GRANT_SQL = "GRANT pg_monitor TO CURRENT_USER;";

export default function MonitoringAccess({ connectionId }: { connectionId: string }) {
  const { t } = useI18n();
  const toast = useToast();
  const queryClient = useQueryClient();
  const statusQuery = useQuery(monitoringStatusQuery(connectionId));
  const change = useMutation({
    mutationFn: (enabled: boolean) =>
      setPostgresMonitoring(connectionId, enabled, true),
    onSuccess: (status) => {
      queryClient.setQueryData(qk.monitoring(connectionId), status);
      toast(status.roleGranted ? t("safety.monitoringEnabled") : t("safety.monitoringRevoked"));
    },
    onError: (error) => toast(errMessage(error), "error"),
  });

  function copyGrant() {
    if (!navigator.clipboard?.writeText) {
      toast(t("safety.monitoringCopyFailed"), "error");
      return;
    }
    void navigator.clipboard.writeText(GRANT_SQL).then(
      () => toast(t("safety.monitoringCopied")),
      () => toast(t("safety.monitoringCopyFailed"), "error"),
    );
  }

  if (statusQuery.isPending) {
    return (
      <section className="monitoring-access ds-panel">
        <Skeleton lines={2} />
      </section>
    );
  }

  if (statusQuery.error || !statusQuery.data) {
    return (
      <section className="monitoring-access ds-panel ds-tone-danger" role="alert">
        <div className="monitoring-access-head">
          <div className="ds-title-line">
            <Icon name="alert" />
            <h3>{t("safety.monitoringTitle")}</h3>
          </div>
        </div>
        <p className="error">
          {t("safety.monitoringError", { error: errMessage(statusQuery.error) })}
        </p>
      </section>
    );
  }

  const status = statusQuery.data;
  const postgres = status.engine === "postgres";
  const tone = status.roleGranted ? "ds-tone-trust" : "ds-tone-risk";
  const coverageLabel = status.roleGranted
    ? t("safety.monitoringCoverageFull")
    : status.coverage === "basic"
      ? t("safety.monitoringCoverageBasic")
      : t("safety.monitoringCoverageLimited");
  const coverageNote = status.roleGranted
    ? t("safety.monitoringFullHint")
    : status.coverage === "basic"
      ? t("safety.monitoringBasicHint")
      : t("safety.monitoringLimitedHint");

  return (
    <section className={`monitoring-access ds-panel ${tone}`}>
      <div className="monitoring-access-head">
        <div>
          <div className="ds-title-line">
            <Icon name="database" />
            <h3>{t("safety.monitoringTitle")}</h3>
          </div>
          <p className="muted">{t("safety.monitoringBody")}</p>
        </div>
        <span className={`badge ${status.roleGranted ? "status-ok" : "risk-medium"}`}>
          {coverageLabel}
        </span>
      </div>

      <div className="monitoring-access-state">
        {status.currentUser && (
          <span>
            {t("safety.monitoringUser")} <code>{status.currentUser}</code>
          </span>
        )}
        <span className="muted">{coverageNote}</span>
        {postgres && !status.canManage && !status.roleGranted && (
          <span className="muted">{t("safety.monitoringAdminHint")}</span>
        )}
      </div>

      {postgres && status.roleAvailable && (
        <div className="monitoring-access-actions ds-action-row ds-control-row">
          {status.roleGranted ? (
            <ConfirmButton
              className="btn danger small"
              disabled={change.isPending}
              confirmLabel={t("safety.monitoringRevokeConfirm")}
              onConfirm={() => change.mutate(false)}
            >
              {t("safety.monitoringRevoke")}
            </ConfirmButton>
          ) : (
            <ConfirmButton
              className="btn primary small"
              disabled={change.isPending}
              confirmLabel={t("safety.monitoringEnableConfirm")}
              onConfirm={() => change.mutate(true)}
            >
              {change.isPending
                ? t("safety.monitoringWorking")
                : t("safety.monitoringEnable")}
            </ConfirmButton>
          )}
          {!status.roleGranted && (
            <button className="btn small" disabled={change.isPending} onClick={copyGrant}>
              <Icon name="copy" />
              {t("safety.monitoringCopyGrant")}
            </button>
          )}
        </div>
      )}

      {postgres && !status.roleAvailable && (
        <p className="muted">{t("safety.monitoringRoleUnavailable")}</p>
      )}
    </section>
  );
}
