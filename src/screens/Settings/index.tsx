// Settings menu — houses everything that isn't a data view: the MCP server config and
// per-connection safety. Moved out of the top tab bar so tabs stay data-focused.
import { useEffect, useRef, useState } from "react";
import type { ConnectionProfile } from "../../ipc/types";
import Mcp from "../Mcp";
import Safety from "../Safety";
import Updates from "../Updates";

type Section = "mcp" | "safety" | "updates";

export default function Settings({
  connection,
  onClose,
  refreshSafety,
  initialSection,
  onOpenAgent,
}: {
  connection: ConnectionProfile | null;
  onClose: () => void;
  // Re-loads the App's per-connection safety so Safety edits apply without reselecting.
  refreshSafety: () => void;
  initialSection?: Section;
  // Jump from the MCP section to the app-level Agent tab (closes Settings).
  onOpenAgent: () => void;
}) {
  const [section, setSection] = useState<Section>(
    initialSection ?? (connection ? "safety" : "mcp"),
  );

  // Safety may have changed while this menu was open — refresh App's copy on the way out.
  function close() {
    refreshSafety();
    onClose();
  }

  // Esc closes the overlay, matching Migrations/RowEditor. Ref keeps the handler
  // pinned to the latest close() (refreshSafety side-effect) without re-binding.
  const closeRef = useRef(close);
  closeRef.current = close;
  useEffect(() => {
    const h = (e: KeyboardEvent) => {
      if (e.key !== "Escape") return;
      // Don't hijack Escape while a field has focus — close() reloads Safety and would
      // discard unsaved edits. Let the input's own Escape (blur/revert) win instead.
      if ((e.target as HTMLElement)?.closest("input, textarea, select")) return;
      closeRef.current();
    };
    window.addEventListener("keydown", h);
    return () => window.removeEventListener("keydown", h);
  }, []);

  return (
    <div className="settings">
      <aside className="settings-nav">
        <div className="settings-head">
          <strong>Settings</strong>
          <button className="btn small" onClick={close}>
            Done
          </button>
        </div>
        <button
          className={section === "mcp" ? "snav active" : "snav"}
          onClick={() => setSection("mcp")}
        >
          MCP server
        </button>
        <button
          className={section === "safety" ? "snav active" : "snav"}
          onClick={() => setSection("safety")}
          disabled={!connection}
          title={connection ? undefined : "Select a connection first"}
        >
          Safety{connection ? ` · ${connection.name || "(unnamed)"}` : ""}
        </button>
        <button
          className={section === "updates" ? "snav active" : "snav"}
          onClick={() => setSection("updates")}
        >
          Updates
        </button>
      </aside>

      <div className="settings-body">
        {section === "mcp" && <Mcp onOpenAgent={onOpenAgent} />}
        {section === "updates" && <Updates />}
        {section === "safety" &&
          (connection ? (
            <Safety connectionId={connection.id} />
          ) : (
            <div className="muted">Select a connection to edit its safety settings.</div>
          ))}
      </div>
    </div>
  );
}
