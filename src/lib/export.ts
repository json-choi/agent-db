// Shared result-export helpers. CSV/JSON shaping lives in sqlBuild (pure, tested);
// this file owns the browser side: clipboard text and file downloads.
import { toCsv, toJson } from "./sqlBuild";

export function download(name: string, text: string, mime: string) {
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
  download(`${base}.json`, toJson(columns, rows), "application/json");
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
