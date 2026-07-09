import type { Engine } from "../ipc/types";

function EngineGlyph({ engine }: { engine: Engine }) {
  if (engine === "postgres") {
    return (
      <svg viewBox="0 0 24 24" aria-hidden="true" focusable="false">
        <path d="M7.4 8.5c0-2.5 1.9-4.4 4.6-4.4s4.6 1.9 4.6 4.4v4.3" />
        <path d="M16.6 11.5c2.1.4 3.3 1.6 3.3 3.1 0 2.8-3.1 4.8-7.5 4.8" />
        <path d="M12 11.2v6.8c0 1.1-.8 1.9-1.9 1.9S8.2 19 8.2 18" />
        <path d="M7.4 12.8c-2 0-3.3-1-3.3-2.7 0-1.2.7-2.2 1.9-2.7" />
        <path d="M9.3 8.5h.01M14.7 8.5h.01" />
      </svg>
    );
  }

  if (engine === "mysql") {
    return (
      <svg viewBox="0 0 24 24" aria-hidden="true" focusable="false">
        <path d="M4 15.4c3.5-4 7.3-5.8 11.4-5.4" />
        <path d="M14.5 7.5c2.4-.7 4.4-.4 5.5.8-1.1.2-2 .7-2.7 1.5" />
        <path d="M5.2 16.8c2.3 1.8 5.2 2.4 8.1 1.3 2.6-1 4.2-3 4.7-5.5" />
        <path d="M7.8 13.4 5 10.7M10.8 11.5 8 8.8" />
      </svg>
    );
  }

  return (
    <svg viewBox="0 0 24 24" aria-hidden="true" focusable="false">
      <path d="M6 5.8c0-1.7 2.7-3 6-3s6 1.3 6 3-2.7 3-6 3-6-1.3-6-3Z" />
      <path d="M18 5.8v12.4c0 1.7-2.7 3-6 3s-6-1.3-6-3V5.8" />
      <path d="M18 11.9c0 1.7-2.7 3-6 3s-6-1.3-6-3" />
    </svg>
  );
}

export default function EngineMark({ engine }: { engine: Engine }) {
  return (
    <span className={`ds-engine-mark engine-${engine}`} title={engine} aria-label={engine}>
      <EngineGlyph engine={engine} />
    </span>
  );
}
