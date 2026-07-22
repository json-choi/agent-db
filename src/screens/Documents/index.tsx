// Ad-hoc read-only document query console for MongoDB connections (the "documents"
// tab's counterpart to the SQL console). find/aggregate/count are built from JSON
// textareas, parsed client-side, then run through the same approved read-only path SQL uses.
import { useEffect, useMemo, useState } from "react";
import { useQuery } from "@tanstack/react-query";
import { runDocumentQuery } from "../../ipc/commands";
import type { ConnectionProfile, DocumentPage, DocumentQuery, QueryResult } from "../../ipc/types";
import { errMessage } from "../../ipc/types";
import DataGrid from "../../components/DataGrid";
import { Icon } from "../../components/Icon";
import ResultToolbar from "../../components/ResultToolbar";
import { catalogQuery } from "../../lib/queries";
import { documentsToGrid } from "../../lib/documentGrid";
import { stamp } from "../../lib/export";
import { useI18n } from "../../lib/i18n";
import { useQueryRun } from "../../lib/useQueryRun";
import "./documents.css";

type Op = "find" | "aggregate" | "count";

function parseJsonField(text: string, label: string): unknown {
  const trimmed = text.trim();
  if (!trimmed) return undefined;
  try {
    return JSON.parse(trimmed);
  } catch (e) {
    throw new Error(`${label}: ${e instanceof Error ? e.message : String(e)}`);
  }
}

function isDocumentQueryShape(value: unknown): value is DocumentQuery {
  return (
    typeof value === "object" &&
    value !== null &&
    "op" in value &&
    "collection" in value
  );
}

export default function Documents({
  connection,
  draft,
}: {
  connection: ConnectionProfile;
  draft?: string | null;
}) {
  const { t } = useI18n();
  const catalog = useQuery(catalogQuery(connection.id));
  const tables = catalog.data?.tables ?? [];

  const [collection, setCollection] = useState("");
  useEffect(() => {
    if (!collection && tables.length > 0) setCollection(tables[0].name);
  }, [tables, collection]);

  const [op, setOp] = useState<Op>("find");
  const [filterText, setFilterText] = useState("");
  const [projectionText, setProjectionText] = useState("");
  const [sortText, setSortText] = useState("");
  const [limit, setLimit] = useState(100);
  const [pipelineText, setPipelineText] = useState("[]");
  const [countFilterText, setCountFilterText] = useState("");

  // Loads an Activity row's replayed query (JSON-serialized DocumentQuery, see
  // App.tsx's loadSql) into the form. `draft` itself is the effect dependency, so a
  // reapplied identical string never loops and there is no separate "consumed" flag.
  useEffect(() => {
    if (draft == null) return;
    let parsed: unknown;
    try {
      parsed = JSON.parse(draft);
    } catch (e) {
      console.warn("failed to parse document draft:", e);
      return;
    }
    if (!isDocumentQueryShape(parsed)) {
      console.warn("document draft is not a DocumentQuery:", parsed);
      return;
    }
    setCollection(parsed.collection);
    setOp(parsed.op);
    if (parsed.op === "find") {
      setFilterText(parsed.filter !== undefined ? JSON.stringify(parsed.filter, null, 2) : "");
      setProjectionText(
        parsed.projection !== undefined ? JSON.stringify(parsed.projection, null, 2) : "",
      );
      setSortText(parsed.sort !== undefined ? JSON.stringify(parsed.sort, null, 2) : "");
      if (typeof parsed.limit === "number") setLimit(parsed.limit);
    } else if (parsed.op === "aggregate") {
      setPipelineText(JSON.stringify(parsed.pipeline, null, 2));
    } else if (parsed.op === "count") {
      setCountFilterText(parsed.filter !== undefined ? JSON.stringify(parsed.filter, null, 2) : "");
    }
  }, [draft]);

  const { running, cancelled, execute: runQuery, cancel } = useQueryRun();
  const [result, setResult] = useState<{ page: DocumentPage; at: string } | null>(null);
  const [parseErr, setParseErr] = useState<string | null>(null);
  const [runErr, setRunErr] = useState<string | null>(null);

  function buildQuery(): DocumentQuery | null {
    try {
      if (op === "find") {
        return {
          op: "find",
          collection,
          filter: parseJsonField(filterText, t("documents.filter")),
          projection: parseJsonField(projectionText, t("documents.projection")),
          sort: parseJsonField(sortText, t("documents.sort")),
          limit,
        };
      }
      if (op === "aggregate") {
        const pipeline = parseJsonField(pipelineText, t("documents.pipeline")) ?? [];
        if (!Array.isArray(pipeline)) throw new Error(t("documents.pipeline"));
        return { op: "aggregate", collection, pipeline };
      }
      return { op: "count", collection, filter: parseJsonField(countFilterText, t("documents.filter")) };
    } catch (e) {
      setParseErr(errMessage(e));
      return null;
    }
  }

  async function execute() {
    if (!collection || running) return;
    setParseErr(null);
    setRunErr(null);
    const query = buildQuery();
    if (!query) return;

    try {
      await runQuery(async (id) => {
        const page = await runDocumentQuery(connection.id, query, true, id);
        setResult({ page, at: new Date().toLocaleTimeString() });
      });
    } catch (e) {
      setRunErr(errMessage(e));
      setResult(null);
    }
  }

  const grid = useMemo(
    () => documentsToGrid(result?.page.documents ?? []),
    [result],
  );
  const gridResult: QueryResult = {
    columns: grid.columns,
    rows: grid.rows,
    rowCount: result?.page.docCount ?? 0,
    truncated: result?.page.truncated ?? false,
    durationMs: result?.page.durationMs ?? 0,
  };

  return (
    <div className="screen documents">
      <div className="ds-toolbar documents-toolbar ds-control-row">
        <h2>{t("documents.title")}</h2>
        <label className="documents-field">
          {t("documents.collection")}
          <select value={collection} onChange={(e) => setCollection(e.target.value)}>
            {tables.length === 0 && <option value="">{t("documents.noCollections")}</option>}
            {tables.map((table) => (
              <option key={table.name} value={table.name}>
                {table.name}
              </option>
            ))}
          </select>
        </label>
        <label className="documents-field">
          {t("documents.operation")}
          <select value={op} onChange={(e) => setOp(e.target.value as Op)}>
            <option value="find">find</option>
            <option value="aggregate">aggregate</option>
            <option value="count">count</option>
          </select>
        </label>
        <span className="ds-toolbar-spacer" />
        <button
          className="btn primary"
          disabled={!collection || running}
          onClick={() => void execute()}
        >
          <Icon name="play" />
          {running ? t("documents.running") : t("documents.run")}
        </button>
        {running && (
          <button className="btn" onClick={cancel}>
            {t("documents.cancel")}
          </button>
        )}
      </div>

      {op === "find" && (
        <div className="documents-params">
          <label>
            {t("documents.filter")}
            <textarea
              value={filterText}
              onChange={(e) => setFilterText(e.target.value)}
              placeholder="{}"
            />
          </label>
          <label>
            {t("documents.projection")}
            <textarea
              value={projectionText}
              onChange={(e) => setProjectionText(e.target.value)}
              placeholder="{}"
            />
          </label>
          <label>
            {t("documents.sort")}
            <textarea
              value={sortText}
              onChange={(e) => setSortText(e.target.value)}
              placeholder="{}"
            />
          </label>
          <label className="documents-limit">
            {t("documents.limit")}
            <input
              type="number"
              min={1}
              value={limit}
              onChange={(e) => setLimit(Number(e.target.value) || 100)}
            />
          </label>
        </div>
      )}
      {op === "aggregate" && (
        <div className="documents-params">
          <label className="documents-pipeline">
            {t("documents.pipeline")}
            <textarea
              value={pipelineText}
              onChange={(e) => setPipelineText(e.target.value)}
              placeholder="[]"
            />
          </label>
        </div>
      )}
      {op === "count" && (
        <div className="documents-params">
          <label>
            {t("documents.filter")}
            <textarea
              value={countFilterText}
              onChange={(e) => setCountFilterText(e.target.value)}
              placeholder="{}"
            />
          </label>
        </div>
      )}

      {parseErr && <div className="error">{parseErr}</div>}
      {runErr && <div className="error">{runErr}</div>}
      {cancelled && <div className="muted">{t("documents.cancelled")}</div>}

      {result && (
        <div className="results">
          <div className="result-meta muted">
            {t("documents.docCount", { count: result.page.docCount })}
            {result.page.truncated && ` - ${t("documents.truncated")}`} · {result.page.durationMs} ms ·{" "}
            {result.at}
            {" · "}
            <ResultToolbar
              columns={gridResult.columns}
              rows={gridResult.rows}
              filenameBase={`documents-${collection}-${stamp()}`}
            />
          </div>
          {gridResult.columns.length > 0 ? (
            <DataGrid result={gridResult} />
          ) : (
            <div className="muted">{t("documents.noDocuments")}</div>
          )}
        </div>
      )}
    </div>
  );
}
