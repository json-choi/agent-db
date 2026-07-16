// Ad-hoc read-only document query console for MongoDB connections (the "documents"
// tab's counterpart to the SQL console). find/aggregate/count are built from JSON
// textareas, parsed client-side, then run through the same approved read-only path SQL uses.
import { useEffect, useMemo, useRef, useState } from "react";
import { useQuery } from "@tanstack/react-query";
import { cancelQuery, runDocumentQuery } from "../../ipc/commands";
import type { ConnectionProfile, DocumentPage, DocumentQuery, QueryResult } from "../../ipc/types";
import { errMessage } from "../../ipc/types";
import DataGrid from "../../components/DataGrid";
import { Icon } from "../../components/Icon";
import ResultToolbar from "../../components/ResultToolbar";
import { catalogQuery } from "../../lib/queries";
import { documentsToGrid } from "../../lib/documentGrid";
import { stamp } from "../../lib/export";
import { useI18n } from "../../lib/i18n";
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

export default function Documents({ connection }: { connection: ConnectionProfile }) {
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

  const [running, setRunning] = useState(false);
  const [result, setResult] = useState<{ page: DocumentPage; at: string } | null>(null);
  const [parseErr, setParseErr] = useState<string | null>(null);
  const [runErr, setRunErr] = useState<string | null>(null);
  const [cancelled, setCancelled] = useState(false);
  const queryIdRef = useRef<string | null>(null);
  const cancelledRef = useRef(false);

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

    const id = crypto.randomUUID();
    queryIdRef.current = id;
    cancelledRef.current = false;
    setRunning(true);
    setCancelled(false);
    try {
      const page = await runDocumentQuery(connection.id, query, true, id);
      setResult({ page, at: new Date().toLocaleTimeString() });
    } catch (e) {
      if (cancelledRef.current) setCancelled(true);
      else {
        setRunErr(errMessage(e));
        setResult(null);
      }
    } finally {
      queryIdRef.current = null;
      setRunning(false);
    }
  }

  function cancel() {
    if (queryIdRef.current) {
      cancelledRef.current = true;
      void cancelQuery(queryIdRef.current);
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
      <div className="ds-toolbar documents-toolbar">
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
