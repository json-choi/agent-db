// Side panel showing one cell's full value: pretty-printed JSON when the value is (or
// parses to) an object/array, wrapped plain text otherwise. Copy button lifts the
// displayed text to the clipboard.
import "./grid.css";

function render(value: unknown): { text: string; json: boolean } {
  if (value === null || value === undefined) return { text: "NULL", json: false };
  if (typeof value === "object") return { text: JSON.stringify(value, null, 2), json: true };
  const s = String(value);
  if (typeof value === "string") {
    try {
      const p = JSON.parse(s);
      if (p && typeof p === "object") return { text: JSON.stringify(p, null, 2), json: true };
    } catch {
      /* not JSON — fall through to plain text */
    }
  }
  return { text: s, json: false };
}

export default function CellViewer({
  value,
  column,
  onClose,
}: {
  value: unknown;
  column: string;
  onClose: () => void;
}) {
  const { text, json } = render(value);
  return (
    <div className="cell-viewer">
      <div className="panel-head">
        <strong>{column}</strong>
        <div className="panel-head-actions">
          <button className="btn small" onClick={() => void navigator.clipboard.writeText(text)}>
            Copy
          </button>
          <button className="btn small" onClick={onClose}>
            ✕
          </button>
        </div>
      </div>
      <pre className={json ? "cell-body json" : "cell-body"}>{text}</pre>
    </div>
  );
}
