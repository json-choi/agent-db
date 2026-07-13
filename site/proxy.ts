import type { NextRequest } from "next/server";
import { NextResponse } from "next/server";

export function proxy(request: NextRequest) {
  const requestHeaders = new Headers(request.headers);
  const isKorean =
    request.nextUrl.pathname === "/ko" || request.nextUrl.searchParams.get("lang") === "ko";

  requestHeaders.set("x-site-lang", isKorean ? "ko" : "en");

  return NextResponse.next({
    request: {
      headers: requestHeaders,
    },
  });
}

export const config = {
  matcher: ["/", "/ko"],
};
