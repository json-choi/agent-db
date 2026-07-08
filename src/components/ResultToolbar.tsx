// Compact export/copy controls for any result grid: Copy (TSV) · CSV · JSON.
// Always operate on the FULL result rows, not a display-sliced subset.
import { downloadCsv, downloadJson, toTsv } from "../lib/export";
import { useI18n } from "../lib/i18n";
import { useToast } from "./Toast";
import "./ResultToolbar.css";

export default function ResultToolbar({
  columns,
  rows,
  filenameBase,
  scopeLabel,
}: {
  columns: string[];
  rows: unknown[][];
  filenameBase: string;
  // Optional on-surface scope for page-limited exports (e.g. "page"). Default keeps
  // the bare "CSV"/"JSON" labels so existing callers (Sql, Agent) are unchanged.
  scopeLabel?: string;
}) {
  const { t } = useI18n();
  const toast = useToast();
  return (
    <span className="result-tools">
      <button
        className="btn small"
        title={t("results.copyTitle")}
        onClick={() =>
          navigator.clipboard
            .writeText(toTsv(columns, rows))
            .then(() => toast(t("results.copyRows", { count: rows.length })))
            .catch(() => toast(t("results.copyFailed"), "error"))
        }
      >
        {t("results.copy")}
      </button>
      <button
        className="btn small"
        title={t("results.downloadCsvTitle")}
        onClick={() => downloadCsv(filenameBase, columns, rows)}
      >
        {scopeLabel ? t("results.exportCsv", { scope: scopeLabel }) : "CSV"}
      </button>
      <button
        className="btn small"
        title={t("results.downloadJsonTitle")}
        onClick={() => downloadJson(filenameBase, columns, rows)}
      >
        {scopeLabel ? t("results.exportJson", { scope: scopeLabel }) : "JSON"}
      </button>
    </span>
  );
}
