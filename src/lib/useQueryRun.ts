// Shared execute/cancel harness for the Sql and Documents consoles: issues a fresh
// queryId per run and tells a user-initiated cancelQuery apart from a real backend
// error so callers only render one or the other. The branching lives in `runTracked`,
// a plain-object helper kept outside React state so it's unit-testable on its own.
import { useRef, useState } from "react";
import { cancelQuery } from "../ipc/commands";
import { isQueryCancellationError } from "../ipc/types";

interface RunTracker {
  queryId: string | null;
  cancelled: boolean;
}

type RunOutcome<T> = { cancelled: true } | { cancelled: false; value: T };

// Runs `fn` with a fresh queryId. A local click is not enough to claim cancellation:
// only the backend's confirmed read-cancellation error is swallowed. An uncertain
// write outcome remains visible and must never look like a harmless cancellation.
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
    if (tracker.cancelled && isQueryCancellationError(e)) return { cancelled: true };
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

  function track(queryId: string) {
    tracker.queryId = queryId;
    if (tracker.cancelled) void cancelQuery(queryId);
  }

  return { running, cancelled, execute, cancel, track };
}
