// App-wide query cache plus the single place backend events are translated into cache
// invalidations. Keeping the listeners here (rather than in each screen) means a screen
// that is not currently mounted still shows fresh data the next time it is opened.
import { useEffect, useState, type ReactNode } from "react";
import { QueryClient, QueryClientProvider, useQueryClient } from "@tanstack/react-query";
import { listen } from "@tauri-apps/api/event";
import type { Dashboard } from "../ipc/types";
import { qk } from "./queries";

function createQueryClient() {
  return new QueryClient({
    defaultOptions: {
      queries: {
        // Desktop app: the window regaining focus is not a signal that database state
        // changed, and a blanket refetch would re-run every open table query.
        refetchOnWindowFocus: false,
        // Failures here are deterministic (bad credentials, invalid SQL, dropped table),
        // so a retry only doubles the wait — and a retried runSql would double-write the
        // query history. Screens surface the error and offer an explicit refresh instead.
        retry: false,
        // Long enough that a cached tab survives a detour through the rest of the app.
        gcTime: 30 * 60_000,
      },
    },
  });
}

// Backend events name the connection they concern, so each one invalidates exactly that
// connection's logs. Prefix keys let `audit` cover both the verdict and the row snapshot.
function CacheInvalidation({ children }: { children: ReactNode }) {
  const queryClient = useQueryClient();

  useEffect(() => {
    const pending = [
      listen<{ connectionId?: unknown }>("agent:result", (event) => {
        const connectionId = event.payload.connectionId;
        if (typeof connectionId !== "string") return;
        void queryClient.invalidateQueries({ queryKey: qk.history(connectionId) });
        void queryClient.invalidateQueries({ queryKey: qk.audit(connectionId) });
      }),
      listen<Dashboard>("dashboard:created", (event) => {
        void queryClient.invalidateQueries({
          queryKey: qk.dashboards(event.payload.connectionId),
        });
      }),
    ];
    return () => {
      for (const p of pending) void p.then((unlisten) => unlisten()).catch(() => {});
    };
  }, [queryClient]);

  return <>{children}</>;
}

export function QueryProvider({ children }: { children: ReactNode }) {
  const [client] = useState(createQueryClient);
  return (
    <QueryClientProvider client={client}>
      <CacheInvalidation>{children}</CacheInvalidation>
    </QueryClientProvider>
  );
}
