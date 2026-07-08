// Per-connection SafetySettings editor. Loads via get_safety, saves via set_safety.
import { useEffect, useState } from "react";
import { getSafety, setSafety } from "../../ipc/commands";
import type { SafetySettings } from "../../ipc/types";
import { errMessage } from "../../ipc/types";
import { Icon } from "../../components/Icon";
import { useToast } from "../../components/Toast";
import { useI18n, type I18nKey } from "../../lib/i18n";
import "./safety.css";

const TOGGLES: { key: keyof SafetySettings; label: I18nKey; hint: I18nKey }[] = [
  { key: "requireApproval", label: "safety.requireApproval", hint: "safety.requireApprovalHint" },
  { key: "allowWrites", label: "safety.allowWrites", hint: "safety.allowWritesHint" },
  { key: "autoRunReads", label: "safety.autoRunReads", hint: "safety.autoRunReadsHint" },
  { key: "wrapWritesInTx", label: "safety.wrapWritesInTx", hint: "safety.wrapWritesInTxHint" },
  { key: "explainPreview", label: "safety.explainPreview", hint: "safety.explainPreviewHint" },
];

const NUMBERS: { key: keyof SafetySettings; label: I18nKey; hint: I18nKey }[] = [
  { key: "maxRows", label: "safety.maxRows", hint: "safety.maxRowsHint" },
  { key: "execPreviewRowLimit", label: "safety.execPreviewRowLimit", hint: "safety.execPreviewRowLimitHint" },
];

export default function Safety({ connectionId }: { connectionId: string }) {
  const { t } = useI18n();
  const [settings, setSettings] = useState<SafetySettings | null>(null);
  const [msg, setMsg] = useState<string | null>(null); // load-failure only; save feedback goes through toast
  const [busy, setBusy] = useState(false);
  const toast = useToast();

  useEffect(() => {
    let alive = true;
    setSettings(null);
    setMsg(null);
    getSafety(connectionId)
      .then((s) => {
        if (alive) setSettings(s);
      })
      .catch((e) => {
        if (alive) setMsg(errMessage(e));
      });
    return () => {
      alive = false;
    };
  }, [connectionId]);

  if (!settings) {
    return <div className={msg ? "screen muted" : "screen muted loading"}>{msg ?? t("safety.loading")}</div>;
  }

  function set<K extends keyof SafetySettings>(key: K, value: SafetySettings[K]) {
    setSettings((s) => (s ? { ...s, [key]: value } : s));
  }

  async function save() {
    if (!settings) return;
    setBusy(true);
    try {
      await setSafety(connectionId, settings);
      toast(t("safety.saved"));
    } catch (e) {
      toast(errMessage(e), "error");
    } finally {
      setBusy(false);
    }
  }

  return (
    <div className="screen safety safety-screen">
      <div className="safety-hero">
        <div className="safety-title-row">
          <h2>{t("safety.title")}</h2>
          <span className="ui-help" title={t("safety.body")} aria-label={t("safety.body")}>
            ?
          </span>
        </div>
        <span
          className={
            settings.allowWrites
              ? "safety-mode safety-mode-risk"
              : "safety-mode safety-mode-safe"
          }
        >
          {settings.allowWrites ? t("safety.modeWrites") : t("safety.modeReadOnly")}
        </span>
      </div>

      <div className="safety-policy-grid ds-policy-grid" aria-label={t("safety.policySummary")}>
        <article className="safety-policy-card ds-policy-card ds-policy-card-trust">
          <Icon name="database" />
          <div>
            <span>{t("safety.policyReads")}</span>
            <strong>{settings.autoRunReads ? t("safety.policyOn") : t("safety.policyManual")}</strong>
            <small>{t("safety.policyReadsBody")}</small>
          </div>
        </article>
        <article
          className={
            settings.allowWrites
              ? "safety-policy-card ds-policy-card ds-policy-card-risk"
              : "safety-policy-card ds-policy-card"
          }
        >
          <Icon name={settings.allowWrites ? "alert" : "circleSlash"} />
          <div>
            <span>{t("safety.policyWrites")}</span>
            <strong>{settings.allowWrites ? t("safety.policyAllowed") : t("safety.policyBlocked")}</strong>
            <small>{t("safety.policyWritesBody")}</small>
          </div>
        </article>
        <article className="safety-policy-card ds-policy-card">
          <Icon name="check" />
          <div>
            <span>{t("safety.policyApproval")}</span>
            <strong>{settings.requireApproval ? t("safety.policyOn") : t("safety.policyOff")}</strong>
            <small>{t("safety.policyApprovalBody")}</small>
          </div>
        </article>
        <article className="safety-policy-card ds-policy-card">
          <Icon name="refresh" />
          <div>
            <span>{t("safety.policyPreview")}</span>
            <strong>{settings.explainPreview ? t("safety.policyOn") : t("safety.policyOff")}</strong>
            <small>{t("safety.policyPreviewBody")}</small>
          </div>
        </article>
      </div>

      <div className="safety-controls">
        <section className="safety-control-panel">
          <h3>{t("safety.guardrails")}</h3>
          {TOGGLES.map((item) => (
            <label key={item.key} className="safety-check">
              <input
                type="checkbox"
                checked={settings[item.key] as boolean}
                onChange={(e) => set(item.key, e.target.checked as never)}
              />
              <span>
                <strong>{t(item.label)}</strong>
              </span>
              <span className="ui-help" title={t(item.hint)} aria-label={t(item.hint)}>
                ?
              </span>
            </label>
          ))}
        </section>

        <section className="safety-control-panel compact">
          <h3>{t("safety.limits")}</h3>
          {NUMBERS.map((n) => (
            <label key={n.key} className="safety-number">
              <span>{t(n.label)}</span>
              <input
                type="number"
                min={n.key === "maxRows" ? 1 : 0}
                step={1}
                value={settings[n.key] as number}
                onChange={(e) => {
                  // Clamp to backend-enforced bounds; guard NaN from an empty field.
                  const raw = Math.floor(Number(e.target.value));
                  const v =
                    n.key === "maxRows"
                      ? Math.min(100000, Math.max(1, raw || 1))
                      : Math.min(1000000, Math.max(0, raw || 0));
                  set(n.key, v as never);
                }}
              />
              <span className="ui-help" title={t(n.hint)} aria-label={t(n.hint)}>
                ?
              </span>
            </label>
          ))}
        </section>
      </div>

      <div className="safety-save-row">
        <button className="btn primary" disabled={busy} onClick={save}>
          {busy ? t("common.saving") : t("common.save")}
        </button>
      </div>
    </div>
  );
}
