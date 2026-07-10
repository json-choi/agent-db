// Side panel showing one cell's full value: pretty-printed JSON when the value is (or
// parses to) an object/array, wrapped plain text otherwise. Copy button lifts the
// displayed text to the clipboard.
import { useEffect } from "react";
import { useI18n } from "../lib/i18n";
import { Icon } from "./Icon";
import { useToast } from "./Toast";
import "./grid.css";

function render(value: unknown): { text: string; json: boolean } {
  if (value === null || value === undefined) return { text: "NULL", json: false };
  if (typeof value === "object") return { text: JSON.stringify(value, null, 2), json: true };
  const s = String(value);
  if (typeof value === "string") {
    try {
      const p = JSON.parse(s);
      if (p && typeof p === "object") return { text: JSON.stringify(p, null, 2), json: true };
    } catch {
      /* not JSON — fall through to plain text */
    }
  }
  return { text: s, json: false };
}

export default function CellViewer({
  value,
  column,
  onClose,
}: {
  value: unknown;
  column: string;
  onClose: () => void;
}) {
  const { text, json } = render(value);
  const { t } = useI18n();
  const toast = useToast();
  // Close on Escape — DataGrid uses Escape to clear cell selection, so leaving the panel
  // stuck open while the selection clears is jarring.
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") onClose();
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [onClose]);
  return (
    <div className="cell-viewer">
      <div className="panel-head">
        <strong>{column}</strong>
        <div className="panel-head-actions">
          <button
            className="btn small"
            onClick={() =>
              navigator.clipboard
                .writeText(text)
                .then(() => toast(t("common.copied")))
                .catch(() => toast(t("results.copyFailed"), "error"))
            }
          >
            <Icon name="copy" /> {t("common.copy")}
          </button>
          <button className="btn small" onClick={onClose} aria-label={t("common.close")}>
            <Icon name="close" />
          </button>
        </div>
      </div>
      <pre className={json ? "cell-body json" : "cell-body"}>{text}</pre>
    </div>
  );
}
