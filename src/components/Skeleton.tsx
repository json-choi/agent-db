// Placeholder bars for a first load that has no cached data to show yet. Revisits paint
// from the query cache, so this should only ever appear on a genuinely cold surface.
// The 200ms reveal delay lives in CSS: a fast response unmounts this before it is visible.
import { useI18n } from "../lib/i18n";
import "./Skeleton.css";

export default function Skeleton({
  lines = 3,
  className = "",
}: {
  lines?: number;
  className?: string;
}) {
  const { t } = useI18n();
  return (
    <div
      className={`skeleton ${className}`.trim()}
      role="status"
      aria-busy="true"
      aria-label={t("common.loading")}
    >
      {Array.from({ length: lines }, (_, i) => (
        <span key={i} className="skeleton-bar" aria-hidden="true" />
      ))}
    </div>
  );
}
