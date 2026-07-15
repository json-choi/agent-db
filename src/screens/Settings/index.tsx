// Settings menu — houses everything that isn't a data view: the MCP server config and
// per-connection safety. Moved out of the top tab bar so tabs stay data-focused.
import { useEffect, useRef, useState } from "react";
import type { Update } from "@tauri-apps/plugin-updater";
import type { ConnectionProfile } from "../../ipc/types";
import InfoTip from "../../components/InfoTip";
import { useI18n } from "../../lib/i18n";
import Mcp from "./Mcp";
import Safety from "./Safety";
import Updates from "./Updates";
import "./settings.css";

type Section = "mcp" | "safety" | "updates" | "language";

export default function Settings({
  connection,
  onClose,
  refreshSafety,
  initialSection,
  onMcpChanged,
  availableUpdate,
  onUpdateChecked,
}: {
  connection: ConnectionProfile | null;
  onClose: () => void;
  // Re-loads the App's per-connection safety so Safety edits apply without reselecting.
  refreshSafety: () => void;
  initialSection?: Section;
  // Re-checks global MCP setup status after one-click platform changes.
  onMcpChanged?: () => void;
  availableUpdate?: Update | null;
  onUpdateChecked?: (update: Update | null) => void;
}) {
  const { lang, setLang, t } = useI18n();
  const [section, setSection] = useState<Section>(
    initialSection ?? (connection ? "safety" : "mcp"),
  );

  // Safety may have changed while this menu was open — refresh App's copy on the way out.
  function close() {
    refreshSafety();
    onClose();
  }

  // Esc closes the overlay, matching other full-screen overlays. Ref keeps the handler
  // pinned to the latest close() (refreshSafety side-effect) without re-binding.
  const closeRef = useRef(close);
  closeRef.current = close;
  useEffect(() => {
    const h = (e: KeyboardEvent) => {
      if (e.key !== "Escape") return;
      // Don't hijack Escape while a field has focus — close() reloads Safety and would
      // discard unsaved edits. Let the input's own Escape (blur/revert) win instead.
      if ((e.target as HTMLElement)?.closest("input, textarea, select")) return;
      closeRef.current();
    };
    window.addEventListener("keydown", h);
    return () => window.removeEventListener("keydown", h);
  }, []);

  return (
    <div className="settings">
      <aside className="settings-nav">
        <div className="settings-head">
          <strong>{t("common.settings")}</strong>
          <button className="btn small" onClick={close}>
            {t("common.done")}
          </button>
        </div>
        <button
          className={section === "mcp" ? "snav active" : "snav"}
          onClick={() => setSection("mcp")}
        >
          {t("mcp.server")}
        </button>
        <button
          className={section === "safety" ? "snav active" : "snav"}
          onClick={() => setSection("safety")}
          disabled={!connection}
          title={connection ? undefined : t("settings.selectConnectionTitle")}
        >
          {t("settings.safety")}
          {connection ? ` · ${connection.name || t("app.unnamed")}` : ""}
        </button>
        <button
          className={section === "language" ? "snav active" : "snav"}
          onClick={() => setSection("language")}
        >
          {t("settings.languageTitle")}
        </button>
        <button
          className={section === "updates" ? "snav active" : "snav"}
          onClick={() => setSection("updates")}
        >
          {t("settings.updates")}
        </button>
      </aside>

      <div className="settings-body">
        {section === "mcp" && <Mcp onMcpChanged={onMcpChanged} />}
        {section === "updates" && (
          <Updates initialUpdate={availableUpdate} onChecked={onUpdateChecked} />
        )}
        {section === "language" && (
          <div className="screen form">
            <div className="settings-title-row">
              <h2>{t("settings.languageTitle")}</h2>
              <InfoTip label={t("settings.languageBody")} />
            </div>
            <label>
              {t("language.label")}
              <select value={lang} onChange={(e) => setLang(e.target.value as typeof lang)}>
                <option value="ko">{t("language.korean")}</option>
                <option value="en">{t("language.english")}</option>
              </select>
            </label>
          </div>
        )}
        {section === "safety" &&
          (connection ? (
            <Safety connectionId={connection.id} />
          ) : (
            <div className="muted">{t("settings.selectConnection")}</div>
          ))}
      </div>
    </div>
  );
}
