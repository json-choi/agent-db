"use client";

import { track } from "@vercel/analytics";
import type { AnchorHTMLAttributes, ReactNode } from "react";

type TrackedLinkProps = AnchorHTMLAttributes<HTMLAnchorElement> & {
  children: ReactNode;
  event: string;
  properties?: Record<string, string | number | boolean>;
};

export function TrackedLink({
  children,
  event,
  properties,
  onClick,
  ...props
}: TrackedLinkProps) {
  return (
    <a
      {...props}
      onClick={(clickEvent) => {
        track(event, properties);
        onClick?.(clickEvent);
      }}
    >
      {children}
    </a>
  );
}
