// Workspace identity stays visually stable while the server silently revalidates
// the OS-keychain session. Resource APIs still authorize every sensitive action.
export const WORKSPACE_AUTH_RECHECK_MS = 5 * 60_000;

export function shouldRevalidateWorkspaceAuth(
  authenticated: boolean,
  dataUpdatedAt: number,
  isFetching: boolean,
  now = Date.now(),
): boolean {
  return (
    authenticated &&
    !isFetching &&
    now - dataUpdatedAt >= WORKSPACE_AUTH_RECHECK_MS
  );
}
