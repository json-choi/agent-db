// Shared execute/cancel harness for the Sql and Documents consoles: issues a fresh
// queryId per run and tells a user-initiated cancelQuery apart from a real backend
// error so callers only render one or the other. The branching lives in `runTracked`,
// a plain-object helper kept outside React state so it's unit-testable on its own.
import { useRef, useState } from "react";
import { cancelQuery } from "../ipc/commands";

interface RunTracker {
  queryId: string | null;
  cancelled: boolean;
}

type RunOutcome<T> = { cancelled: true } | { cancelled: false; value: T };

// Runs `fn` with a fresh queryId. If `fn` rejects after cancelTracked() flagged this
// tracker, the rejection is swallowed (cancelled: true); any other rejection rethrows.
export async function runTracked<T>(
  tracker: RunTracker,
  fn: (queryId: string) => Promise<T>,
): Promise<RunOutcome<T>> {
  const id = crypto.randomUUID();
  tracker.queryId = id;
  tracker.cancelled = false;
  try {
    return { cancelled: false, value: await fn(id) };
  } catch (e) {
    if (tracker.cancelled) return { cancelled: true };
    throw e;
  } finally {
    tracker.queryId = null;
  }
}

export function cancelTracked(tracker: RunTracker): void {
  if (tracker.queryId) {
    tracker.cancelled = true;
    void cancelQuery(tracker.queryId);
  }
}

export function useQueryRun() {
  const [running, setRunning] = useState(false);
  const [cancelled, setCancelled] = useState(false);
  const tracker = useRef<RunTracker>({ queryId: null, cancelled: false }).current;

  async function execute<T>(fn: (queryId: string) => Promise<T>): Promise<T | undefined> {
    setRunning(true);
    setCancelled(false);
    try {
      const outcome = await runTracked(tracker, fn);
      if (outcome.cancelled) {
        setCancelled(true);
        return undefined;
      }
      return outcome.value;
    } finally {
      setRunning(false);
    }
  }

  function cancel() {
    cancelTracked(tracker);
  }

  return { running, cancelled, execute, cancel };
}
