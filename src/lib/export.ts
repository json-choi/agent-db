// Shared result-export helpers. CSV/JSON shaping lives in sqlBuild (pure, tested);
// this file owns the browser side: clipboard text and file downloads.
import { toCsv, toJson } from "./sqlBuild";

function download(name: string, text: string, mime: string) {
  const url = URL.createObjectURL(new Blob([text], { type: mime }));
  const a = document.createElement("a");
  a.href = url;
  a.download = name;
  a.click();
  URL.revokeObjectURL(url);
}

// UTF-8 BOM so Excel opens non-ASCII (e.g. Korean) CSV correctly.
export function downloadCsv(base: string, columns: string[], rows: unknown[][]) {
  download(`${base}.csv`, "\uFEFF" + toCsv(columns, rows), "text/csv");
}

export function downloadJson(base: string, columns: string[], rows: unknown[][]) {
  // Pretty-print small exports; skip the 2-space indent past 5000 rows so the JSON string
  // is ~half the size and stringify runs ~2x faster on the main thread. toJson stays the
  // pretty path (its self-test pins that output); large path shapes rows inline & compact.
  const text =
    rows.length > 5000
      ? JSON.stringify(rows.map((r) => Object.fromEntries(columns.map((c, i) => [c, r[i] ?? null]))))
      : toJson(columns, rows);
  download(`${base}.json`, text, "application/json");
}

function cellText(v: unknown): string {
  if (v === null || v === undefined) return "";
  return typeof v === "object" ? JSON.stringify(v) : String(v);
}

// Tab-separated text for pasting into spreadsheets.
export function toTsv(columns: string[], rows: unknown[][]): string {
  return [columns, ...rows].map((r) => r.map(cellText).join("\t")).join("\n");
}

// Filename-safe local timestamp, e.g. 2026-07-03-14-05-09.
export function stamp(): string {
  const d = new Date();
  const p = (n: number) => String(n).padStart(2, "0");
  return `${d.getFullYear()}-${p(d.getMonth() + 1)}-${p(d.getDate())}-${p(d.getHours())}-${p(d.getMinutes())}-${p(d.getSeconds())}`;
}
