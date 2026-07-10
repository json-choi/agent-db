// First-run onboarding — shown when no database is connected yet. Instead of a blank
// screen, guide the user to connect a database or wire up the MCP server.
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
        <h1>{t("onboarding.title")}</h1>
        <p className="lead muted">{t("onboarding.lead")}</p>

        <div className="onboarding-steps">
          <div className="step-card">
            <div className="step-num">1</div>
            <h3>{t("onboarding.databaseTitle")}</h3>
            <p className="muted">{t("onboarding.databaseBody")}</p>
            <button className="btn primary" onClick={onNewConnection}>
              {t("onboarding.addConnection")}
            </button>
          </div>

          <div className="step-card">
            <div className="step-num">2</div>
            <h3>{t("onboarding.agentTitle")}</h3>
            <p className="muted">{t("onboarding.agentBody")}</p>
            <button className="btn" onClick={onOpenMcp}>
              {t("onboarding.setupMcp")}
            </button>
          </div>
        </div>

        <p className="onboarding-foot muted">{t("onboarding.foot")}</p>
      </div>
    </div>
  );
}
