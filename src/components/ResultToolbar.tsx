// Compact export/copy controls for any result grid: Copy (TSV) · CSV · JSON.
// Always operate on the FULL result rows, not a display-sliced subset.
import { downloadCsv, downloadJson, toTsv } from "../lib/export";
import { useToast } from "./Toast";
import "./ResultToolbar.css";

export default function ResultToolbar({
  columns,
  rows,
  filenameBase,
}: {
  columns: string[];
  rows: unknown[][];
  filenameBase: string;
}) {
  const toast = useToast();
  return (
    <span className="result-tools">
      <button
        className="btn small"
        title="Copy all rows as tab-separated text (pastes into Excel/Sheets)"
        onClick={() =>
          navigator.clipboard
            .writeText(toTsv(columns, rows))
            .then(() => toast(`Copied ${rows.length} rows`))
            .catch(() => toast("Copy failed", "error"))
        }
      >
        Copy
      </button>
      <button
        className="btn small"
        title="Download as CSV (opens in Excel)"
        onClick={() => downloadCsv(filenameBase, columns, rows)}
      >
        CSV
      </button>
      <button
        className="btn small"
        title="Download as JSON"
        onClick={() => downloadJson(filenameBase, columns, rows)}
      >
        JSON
      </button>
    </span>
  );
}
