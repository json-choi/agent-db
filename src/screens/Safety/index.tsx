// Per-connection SafetySettings editor. Loads via get_safety, saves via set_safety.
import { useEffect, useState } from "react";
import { getSafety, setSafety } from "../../ipc/commands";
import type { SafetySettings } from "../../ipc/types";
import { errMessage } from "../../ipc/types";
import { useToast } from "../../components/Toast";

const TOGGLES: { key: keyof SafetySettings; label: string; hint: string }[] = [
  { key: "requireApproval", label: "Require approval", hint: "Gate every statement behind the approval card." },
  { key: "allowWrites", label: "Allow writes", hint: "Master gate for the write path (off by default)." },
  { key: "autoRunReads", label: "Auto-run reads", hint: "Run read-only SELECTs without a manual approve." },
  { key: "wrapWritesInTx", label: "Wrap writes in a transaction", hint: "BEGIN … writes … so they can be rolled back." },
  { key: "explainPreview", label: "EXPLAIN preview", hint: "Show the plan / row estimate before running." },
];

const NUMBERS: { key: keyof SafetySettings; label: string; hint: string }[] = [
  { key: "maxRows", label: "Max rows", hint: "Row cap applied to read result sets." },
  { key: "execPreviewRowLimit", label: "Exec-preview row limit", hint: "Skip execute-preview above this estimate (L3 gate)." },
];

export default function Safety({ connectionId }: { connectionId: string }) {
  const [settings, setSettings] = useState<SafetySettings | null>(null);
  const [msg, setMsg] = useState<string | null>(null); // load-failure only; save feedback goes through toast
  const [busy, setBusy] = useState(false);
  const toast = useToast();

  useEffect(() => {
    let alive = true;
    setSettings(null);
    setMsg(null);
    getSafety(connectionId)
      .then((s) => {
        if (alive) setSettings(s);
      })
      .catch((e) => {
        if (alive) setMsg(errMessage(e));
      });
    return () => {
      alive = false;
    };
  }, [connectionId]);

  if (!settings) {
    return <div className={msg ? "screen muted" : "screen muted loading"}>{msg ?? "Loading safety settings…"}</div>;
  }

  function set<K extends keyof SafetySettings>(key: K, value: SafetySettings[K]) {
    setSettings((s) => (s ? { ...s, [key]: value } : s));
  }

  async function save() {
    if (!settings) return;
    setBusy(true);
    try {
      await setSafety(connectionId, settings);
      toast("Safety settings saved");
    } catch (e) {
      toast(errMessage(e), "error");
    } finally {
      setBusy(false);
    }
  }

  return (
    <div className="screen safety form">
      <h2>Safety settings</h2>
      {TOGGLES.map((t) => (
        <label key={t.key} className="check">
          <input
            type="checkbox"
            checked={settings[t.key] as boolean}
            onChange={(e) => set(t.key, e.target.checked as never)}
          />
          <span>
            {t.label}
            <small className="muted"> — {t.hint}</small>
          </span>
        </label>
      ))}
      {NUMBERS.map((n) => (
        <label key={n.key}>
          {n.label}
          <input
            type="number"
            min={n.key === "maxRows" ? 1 : 0}
            step={1}
            value={settings[n.key] as number}
            onChange={(e) => {
              // Clamp to backend-enforced bounds; guard NaN from an empty field.
              const raw = Math.floor(Number(e.target.value));
              const v =
                n.key === "maxRows"
                  ? Math.min(100000, Math.max(1, raw || 1))
                  : Math.min(1000000, Math.max(0, raw || 0));
              set(n.key, v as never);
            }}
          />
          <small className="muted">{n.hint}</small>
        </label>
      ))}
      <div className="form-actions">
        <button className="btn primary" disabled={busy} onClick={save}>
          Save
        </button>
      </div>
    </div>
  );
}
