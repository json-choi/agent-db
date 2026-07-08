import { lazy, Suspense, type ReactNode } from "react";
import type { SqlViewerProps } from "./SqlViewer";

const SqlViewer = lazy(() => import("./SqlViewer"));

export default function LazySqlViewer({
  fallback = <div className="muted small-pad">Loading editor...</div>,
  ...props
}: SqlViewerProps & { fallback?: ReactNode }) {
  return (
    <Suspense fallback={fallback}>
      <SqlViewer {...props} />
    </Suspense>
  );
}
