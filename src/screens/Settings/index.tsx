// Settings menu — houses everything that isn't a data view: the MCP server config and
// per-connection safety. Moved out of the top tab bar so tabs stay data-focused.
import { useState } from "react";
import type { ConnectionProfile } from "../../ipc/types";
import Mcp from "../Mcp";
import Safety from "../Safety";

type Section = "mcp" | "safety";

export default function Settings({
  connection,
  onClose,
  refreshSafety,
  initialSection,
}: {
  connection: ConnectionProfile | null;
  onClose: () => void;
  // Re-loads the App's per-connection safety so Safety edits apply without reselecting.
  refreshSafety: () => void;
  initialSection?: Section;
}) {
  const [section, setSection] = useState<Section>(
    initialSection ?? (connection ? "safety" : "mcp"),
  );

  // Safety may have changed while this menu was open — refresh App's copy on the way out.
  function close() {
    refreshSafety();
    onClose();
  }

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
      </aside>

      <div className="settings-body">
        {section === "mcp" && <Mcp />}
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
