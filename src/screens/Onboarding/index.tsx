// First-run onboarding — shown when no database is connected yet. Instead of a blank
// screen, guide the user to connect a database or wire up the MCP server.
import InfoTip from "../../components/InfoTip";
import { useI18n } from "../../lib/i18n";
import "./onboarding.css";

export default function Onboarding({
  onNewConnection,
  onOpenMcp,
}: {
  onNewConnection: () => void;
  onOpenMcp: () => void;
}) {
  const { t } = useI18n();

  return (
    <div className="onboarding">
      <div className="onboarding-inner">
        <div className="onboarding-title-row">
          <h1>{t("onboarding.title")}</h1>
          <InfoTip label={t("onboarding.lead")} />
        </div>

        <div className="onboarding-steps">
          <div className="step-card">
            <div className="step-num">1</div>
            <div className="step-title-row">
              <h3>{t("onboarding.databaseTitle")}</h3>
              <InfoTip label={t("onboarding.databaseBody")} />
            </div>
            <button className="btn primary" onClick={onNewConnection}>
              {t("onboarding.addConnection")}
            </button>
          </div>

          <div className="step-card">
            <div className="step-num">2</div>
            <div className="step-title-row">
              <h3>{t("onboarding.agentTitle")}</h3>
              <InfoTip label={t("onboarding.agentBody")} />
            </div>
            <button className="btn" onClick={onOpenMcp}>
              {t("onboarding.setupMcp")}
            </button>
          </div>
        </div>

        <InfoTip label={t("onboarding.foot")} className="onboarding-foot" />
      </div>
    </div>
  );
}
