// Group schema comparison workspace. Loads every member through the shared catalog
// query cache, summarizes all targets against one baseline, and exposes object-level
// before/after details without coupling the workflow to sidebar expansion state.
import { useEffect, useMemo, useState } from "react";
import { useQueries, useQueryClient } from "@tanstack/react-query";
import type { Catalog, ConnectionProfile } from "../../ipc/types";
import { errMessage } from "../../ipc/types";
import EngineMark from "../../components/EngineMark";
import { Icon } from "../../components/Icon";
import Skeleton from "../../components/Skeleton";
import {
  catalogQuery,
  fetchFreshCatalog,
  qk,
} from "../../lib/queries";
import {
  compareCatalogs,
  defaultSchemaBaseline,
  diffCounts,
  schemaGroupIsCompatible,
  type SchemaConnectionGroup,
  type SchemaDiffStatus,
  type SchemaObjectDiff,
  type SchemaObjectType,
} from "../../lib/schemaDiff";
import { useI18n, type I18nKey } from "../../lib/i18n";
import "./schemaDiff.css";

type StatusFilter = Exclude<SchemaDiffStatus, "same"> | "all";

const OBJECT_LABELS: Record<SchemaObjectType, I18nKey> = {
  table: "schemaDiff.objectTable",
  view: "schemaDiff.objectView",
  column: "schemaDiff.objectColumn",
  index: "schemaDiff.objectIndex",
  foreignKey: "schemaDiff.objectForeignKey",
};

const STATUS_LABELS: Record<Exclude<SchemaDiffStatus, "same">, I18nKey> = {
  added: "schemaDiff.statusAdded",
  missing: "schemaDiff.statusMissing",
  changed: "schemaDiff.statusChanged",
};

function connectionName(connection: ConnectionProfile) {
  return connection.name || connection.database || connection.host;
}

function baselineStorageKey(groupKey: string) {
  return `dopedb.schemaDiffBaseline.${groupKey}`;
}

function statusSymbol(status: Exclude<SchemaDiffStatus, "same">) {
  if (status === "added") return "+";
  if (status === "missing") return "−";
  return "~";
}

export default function SchemaDiff({
  group,
  onClose,
}: {
  group: SchemaConnectionGroup;
  onClose: () => void;
}) {
  const { t } = useI18n();
  const queryClient = useQueryClient();
  const connectionIds = useMemo(
    () => group.connections.map((connection) => connection.id),
    [group.connections],
  );
  const queryResults = useQueries({
    queries: connectionIds.map((connectionId) => catalogQuery(connectionId)),
  });
  const [baselineId, setBaselineId] = useState("");
  const [targetId, setTargetId] = useState("");
  const [statusFilter, setStatusFilter] = useState<StatusFilter>("all");
  const [search, setSearch] = useState("");
  const [refreshing, setRefreshing] = useState(false);
  const [refreshErrors, setRefreshErrors] = useState<Record<string, string>>({});

  const queryById = useMemo(
    () =>
      new Map(
        connectionIds.map((connectionId, index) => [connectionId, queryResults[index]]),
      ),
    [connectionIds, queryResults],
  );

  useEffect(() => {
    const stored = localStorage.getItem(baselineStorageKey(group.key));
    const saved = group.connections.find((connection) => connection.id === stored);
    const next = saved ?? defaultSchemaBaseline(group);
    setBaselineId(next?.id ?? "");
  }, [group]);

  const baseline =
    group.connections.find((connection) => connection.id === baselineId) ??
    defaultSchemaBaseline(group);
  const targets = useMemo(
    () => group.connections.filter((connection) => connection.id !== baseline?.id),
    [baseline?.id, group.connections],
  );

  useEffect(() => {
    if (!targets.some((connection) => connection.id === targetId)) {
      setTargetId(targets[0]?.id ?? "");
    }
  }, [targetId, targets]);

  const catalogs = useMemo(() => {
    const map = new Map<string, Catalog>();
    for (const connectionId of connectionIds) {
      const catalog = queryById.get(connectionId)?.data;
      if (catalog) map.set(connectionId, catalog);
    }
    return map;
  }, [connectionIds, queryById]);

  const baselineCatalog = baseline ? catalogs.get(baseline.id) : undefined;
  const comparisons = useMemo(() => {
    const map = new Map<string, ReturnType<typeof compareCatalogs>>();
    if (!baselineCatalog) return map;
    for (const target of targets) {
      const catalog = catalogs.get(target.id);
      if (catalog) map.set(target.id, compareCatalogs(catalog, baselineCatalog));
    }
    return map;
  }, [baselineCatalog, catalogs, targets]);

  const selectedTarget = targets.find((connection) => connection.id === targetId) ?? null;
  const selectedDiff = selectedTarget ? comparisons.get(selectedTarget.id) : undefined;
  const selectedLoadError = (() => {
    if (baseline) {
      const error = refreshErrors[baseline.id] ?? queryById.get(baseline.id)?.error;
      if (error) return { connection: baseline, error: errMessage(error) };
    }
    if (selectedTarget) {
      const error = refreshErrors[selectedTarget.id] ?? queryById.get(selectedTarget.id)?.error;
      if (error) return { connection: selectedTarget, error: errMessage(error) };
    }
    return null;
  })();
  const visibleObjects = useMemo(() => {
    const normalizedSearch = search.trim().toLocaleLowerCase();
    return (selectedDiff?.objects ?? []).filter((object) => {
      if (statusFilter !== "all" && object.status !== statusFilter) return false;
      if (!normalizedSearch) return true;
      return `${object.path} ${object.objectType} ${object.baselineValue} ${object.targetValue}`
        .toLocaleLowerCase()
        .includes(normalizedSearch);
    });
  }, [search, selectedDiff, statusFilter]);

  function changeBaseline(nextId: string) {
    setBaselineId(nextId);
    setTargetId("");
    localStorage.setItem(baselineStorageKey(group.key), nextId);
  }

  async function refreshAll() {
    setRefreshing(true);
    setRefreshErrors({});
    const results = await Promise.allSettled(
      group.connections.map(async (connection) => {
        const catalog = await fetchFreshCatalog(connection.id);
        queryClient.setQueryData(qk.catalog(connection.id), catalog);
        return connection.id;
      }),
    );
    const errors: Record<string, string> = {};
    results.forEach((result, index) => {
      if (result.status === "rejected") {
        errors[group.connections[index].id] = errMessage(result.reason);
      }
    });
    setRefreshErrors(errors);
    setRefreshing(false);
  }

  if (!schemaGroupIsCompatible(group)) {
    return (
      <section className="main-view schema-diff-screen">
        <SchemaDiffHeader group={group} onClose={onClose} />
        <div className="schema-diff-empty ds-panel ds-tone-risk">
          <Icon name="alert" />
          <div>
            <strong>{t("schemaDiff.incompatibleTitle")}</strong>
            <p>{t("schemaDiff.incompatibleBody")}</p>
          </div>
        </div>
      </section>
    );
  }

  return (
    <section className="main-view schema-diff-screen">
      <SchemaDiffHeader group={group} onClose={onClose} />

      <div className="schema-diff-toolbar ds-toolbar">
        <label className="schema-diff-select">
          <span>{t("schemaDiff.baseline")}</span>
          <select value={baseline?.id ?? ""} onChange={(event) => changeBaseline(event.target.value)}>
            {group.connections.map((connection) => (
              <option key={connection.id} value={connection.id}>
                {connectionName(connection)}{connection.env ? ` · ${connection.env}` : ""}
              </option>
            ))}
          </select>
        </label>
        <label className="schema-diff-select">
          <span>{t("schemaDiff.target")}</span>
          <select value={targetId} onChange={(event) => setTargetId(event.target.value)} disabled={targets.length === 0}>
            {targets.map((connection) => (
              <option key={connection.id} value={connection.id}>
                {connectionName(connection)}{connection.env ? ` · ${connection.env}` : ""}
              </option>
            ))}
          </select>
        </label>
        <span className="ds-toolbar-spacer" />
        <button className="btn small" disabled={refreshing} onClick={() => void refreshAll()}>
          <Icon name="refresh" />
          {refreshing ? t("schemaDiff.refreshing") : t("schemaDiff.refreshAll")}
        </button>
      </div>

      {targets.length === 0 ? (
        <div className="schema-diff-empty ds-panel">
          <Icon name="database" />
          <div>
            <strong>{t("schemaDiff.needTargetTitle")}</strong>
            <p>{t("schemaDiff.needTargetBody")}</p>
          </div>
        </div>
      ) : (
        <>
          <div className="schema-diff-overview" aria-label={t("schemaDiff.groupOverview")}>
            {targets.map((target) => {
              const result = queryById.get(target.id);
              const error = refreshErrors[target.id] ?? (result?.error ? errMessage(result.error) : null);
              const diff = comparisons.get(target.id);
              const counts = diff ? diffCounts(diff) : null;
              const selected = target.id === targetId;
              return (
                <button
                  key={target.id}
                  type="button"
                  className={`schema-diff-target-card${selected ? " selected" : ""}`}
                  aria-pressed={selected}
                  onClick={() => setTargetId(target.id)}
                >
                  <span className="schema-diff-target-head">
                    <span className="schema-diff-target-name">{connectionName(target)}</span>
                    {target.env && <span className={`env-chip env-${target.env}`}>{target.env}</span>}
                  </span>
                  {error ? (
                    <span className="schema-diff-target-error">{t("schemaDiff.loadFailed")}</span>
                  ) : !diff ? (
                    <span className="muted">{t("common.loading")}</span>
                  ) : diff.total === 0 ? (
                    <span className="schema-diff-in-sync"><Icon name="check" /> {t("schemaDiff.inSync")}</span>
                  ) : (
                    <span className="schema-diff-counts">
                      <span className="diff-added">+{counts?.added ?? 0}</span>
                      <span className="diff-missing">−{counts?.missing ?? 0}</span>
                      <span className="diff-changed">~{counts?.changed ?? 0}</span>
                    </span>
                  )}
                </button>
              );
            })}
          </div>

          {[...Object.entries(refreshErrors)].map(([connectionId, error]) => {
            const connection = group.connections.find((candidate) => candidate.id === connectionId);
            return (
              <div className="error schema-diff-error" key={connectionId}>
                {t("schemaDiff.connectionError", { connection: connection ? connectionName(connection) : connectionId, error })}
              </div>
            );
          })}

          <div className="schema-diff-detail ds-surface">
            <div className="schema-diff-detail-toolbar ds-data-toolbar">
              <div className="schema-diff-filter-group" role="group" aria-label={t("schemaDiff.filterStatus")}>
                {(["all", "added", "missing", "changed"] as const).map((status) => (
                  <button
                    key={status}
                    type="button"
                    className={`schema-diff-filter${statusFilter === status ? " active" : ""}`}
                    aria-pressed={statusFilter === status}
                    onClick={() => setStatusFilter(status)}
                  >
                    {status === "all" ? t("schemaDiff.statusAll") : t(STATUS_LABELS[status])}
                  </button>
                ))}
              </div>
              <label className="schema-diff-search">
                <Icon name="search" />
                <input
                  value={search}
                  onChange={(event) => setSearch(event.target.value)}
                  placeholder={t("schemaDiff.searchPlaceholder")}
                  aria-label={t("schemaDiff.searchPlaceholder")}
                />
              </label>
            </div>

            {selectedLoadError ? (
              <div className="schema-diff-empty compact ds-tone-risk">
                <Icon name="alert" />
                <div>
                  <strong>{t("schemaDiff.loadFailed")}</strong>
                  <p>
                    {t("schemaDiff.connectionError", {
                      connection: connectionName(selectedLoadError.connection),
                      error: selectedLoadError.error,
                    })}
                  </p>
                </div>
              </div>
            ) : !baselineCatalog || (selectedTarget && !catalogs.has(selectedTarget.id)) ? (
              <Skeleton lines={7} className="schema-diff-skeleton" />
            ) : selectedDiff?.total === 0 ? (
              <div className="schema-diff-empty compact">
                <Icon name="check" />
                <div>
                  <strong>{t("schemaDiff.inSync")}</strong>
                  <p>{t("schemaDiff.inSyncBody")}</p>
                </div>
              </div>
            ) : visibleObjects.length === 0 ? (
              <div className="schema-diff-empty compact muted">{t("schemaDiff.noMatches")}</div>
            ) : (
              <SchemaDiffGrid
                objects={visibleObjects}
                baseline={baseline}
                target={selectedTarget}
              />
            )}
          </div>
        </>
      )}
    </section>
  );
}

function SchemaDiffHeader({
  group,
  onClose,
}: {
  group: SchemaConnectionGroup;
  onClose: () => void;
}) {
  const { t } = useI18n();
  const engine = group.connections[0]?.engine;
  return (
    <header className="schema-diff-head ds-workbench-head">
      <div className="ds-workbench-title">
        <div className="ds-title-line">
          {engine && <EngineMark engine={engine} />}
          <h2>{group.label}</h2>
          <span className="ds-context-badge">{t("schemaDiff.groupBadge", { count: group.connections.length })}</span>
        </div>
        <p className="muted">{t("schemaDiff.subtitle")}</p>
      </div>
      <button className="btn small" onClick={onClose}>
        <Icon name="close" />
        {t("common.close")}
      </button>
    </header>
  );
}

function SchemaDiffGrid({
  objects,
  baseline,
  target,
}: {
  objects: SchemaObjectDiff[];
  baseline: ConnectionProfile | null;
  target: ConnectionProfile | null;
}) {
  const { t } = useI18n();
  return (
    <div className="schema-diff-grid" role="table" aria-label={t("schemaDiff.detailTitle")}>
      <div className="schema-diff-grid-head" role="row">
        <span role="columnheader">{t("schemaDiff.object")}</span>
        <span role="columnheader">{t("schemaDiff.change")}</span>
        <span role="columnheader">{baseline ? connectionName(baseline) : t("schemaDiff.baseline")}</span>
        <span role="columnheader">{target ? connectionName(target) : t("schemaDiff.target")}</span>
      </div>
      <div className="schema-diff-grid-body">
        {objects.map((object) => (
          <div className={`schema-diff-row status-${object.status}`} role="row" key={object.id}>
            <span className="schema-diff-object" role="cell">
              <span className="schema-diff-object-kind">{t(OBJECT_LABELS[object.objectType])}</span>
              <code>{object.path}</code>
            </span>
            <span className={`schema-diff-status status-${object.status}`} role="cell">
              <span aria-hidden="true">{statusSymbol(object.status)}</span>
              {t(STATUS_LABELS[object.status])}
            </span>
            <code className="schema-diff-value baseline-value" role="cell">{object.baselineValue}</code>
            <code className="schema-diff-value target-value" role="cell">{object.targetValue}</code>
          </div>
        ))}
      </div>
    </div>
  );
}
