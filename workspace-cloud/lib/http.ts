export function clientIp(request: Request): string {
  return (
    request.headers.get("x-vercel-forwarded-for")?.split(",")[0]?.trim() ||
    request.headers.get("x-forwarded-for")?.split(",")[0]?.trim() ||
    "unknown"
  );
}

export function jsonError(message: string, status: number) {
  return Response.json({ error: message }, { status });
}

export function mutationAllowed(request: Request, appOrigin: string) {
  if (request.headers.get("authorization")?.startsWith("Bearer ")) return true;
  return request.headers.get("origin") === appOrigin;
}

export function safeReturnTo(value: string | null, fallback = "/settings"): string {
  return value?.startsWith("/") && !value.startsWith("//") ? value : fallback;
}
