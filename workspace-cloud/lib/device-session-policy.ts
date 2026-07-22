export interface DeviceSessionIdentity {
  session: { id: string };
}

/** Revoke every listed device session except the one needed to finish sign-out. */
export function sessionsExceptCurrent<T extends DeviceSessionIdentity>(
  sessions: T[],
  currentSessionId: string,
): T[] {
  return sessions.filter((item) => item.session.id !== currentSessionId);
}
