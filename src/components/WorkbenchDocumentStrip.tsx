import type { ConnectionProfile } from "../ipc/types";
import type { WorkbenchDocument } from "../lib/workbenchDocuments";
import { tableLabel } from "../lib/tableRef";
import { useI18n } from "../lib/i18n";
import { Icon } from "./Icon";

export default function WorkbenchDocumentStrip({
  documents,
  activeId,
  engine,
  supportsSql,
  onActivate,
  onClose,
  onNewQuery,
  onOpenActivity,
}: {
  documents: WorkbenchDocument[];
  activeId: string | null;
  engine: ConnectionProfile["engine"];
  supportsSql: boolean;
  onActivate: (id: string) => void;
  onClose: (id: string) => void;
  onNewQuery: () => void;
  onOpenActivity: () => void;
}) {
  const { t } = useI18n();
  const visibleDocuments = documents.filter((document) => document.kind !== "schema");
  const hasVisibleActiveDocument = visibleDocuments.some(
    (document) => document.id === activeId,
  );
  const keyboardFallbackId = visibleDocuments[0]?.id ?? null;

  function label(document: WorkbenchDocument, index: number) {
    if (document.kind === "data") return tableLabel(engine, document.table);
    if (document.kind === "schema") return t("tabs.schema");
    if (document.kind === "activity") return t("tabs.activity");
    if (document.kind === "documents") return `${t("tabs.documents")} ${index + 1}`;
    return `${t("tabs.sql")} ${index + 1}`;
  }

  let queryIndex = 0;
  return (
    <div className="workbench-document-strip">
      <div
        className="workbench-document-tabs ds-control-row"
        role="tablist"
        aria-label={t("app.workbenchNavigation")}
        onKeyDown={(event) => {
          if (event.key !== "ArrowLeft" && event.key !== "ArrowRight") return;
          const tabs = [
            ...event.currentTarget.querySelectorAll<HTMLButtonElement>('[role="tab"]'),
          ];
          const current = tabs.indexOf(event.target as HTMLButtonElement);
          if (current < 0) return;
          event.preventDefault();
          const direction = event.key === "ArrowRight" ? 1 : -1;
          tabs[(current + direction + tabs.length) % tabs.length]?.focus();
        }}
      >
        {visibleDocuments.map((document) => {
          const index =
            document.kind === "sql" || document.kind === "documents" ? queryIndex++ : 0;
          const title = label(document, index);
          const active = activeId === document.id;
          return (
            <div
              className={`workbench-document-tab${active ? " active" : ""}`}
              key={document.id}
            >
              <button
                type="button"
                className="workbench-document-select"
                role="tab"
                aria-selected={active}
                tabIndex={
                  active || (!hasVisibleActiveDocument && document.id === keyboardFallbackId)
                    ? 0
                    : -1
                }
                onClick={() => onActivate(document.id)}
                title={title}
              >
                <Icon
                  name={
                    document.kind === "data"
                      ? "table"
                      : document.kind === "schema"
                        ? "dashboard"
                        : document.kind === "activity"
                          ? "chart"
                          : document.kind === "documents"
                            ? "list"
                            : "play"
                  }
                />
                <span>{title}</span>
              </button>
              <button
                type="button"
                className="workbench-document-close"
                onClick={() => onClose(document.id)}
                title={t("common.close")}
                aria-label={`${t("common.close")}: ${title}`}
              >
                <Icon name="close" />
              </button>
            </div>
          );
        })}
      </div>
      <div className="workbench-document-actions ds-control-row">
        <button
          type="button"
          className="btn small"
          onClick={onOpenActivity}
          title={t("tabs.activity")}
          aria-label={t("tabs.activity")}
        >
          <Icon name="chart" />
        </button>
        <button
          type="button"
          className="btn small"
          onClick={onNewQuery}
          title={supportsSql ? t("tabs.sql") : t("tabs.documents")}
          aria-label={supportsSql ? t("tabs.sql") : t("tabs.documents")}
        >
          <Icon name="plus" />
        </button>
      </div>
    </div>
  );
}
