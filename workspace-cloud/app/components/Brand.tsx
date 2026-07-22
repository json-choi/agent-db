import Link from "next/link";

export function Brand() {
  return (
    <Link className="brand" href="/settings" aria-label="DopeDB workspace home">
      <span className="brand-mark" aria-hidden="true"><i /><i /><i /></span>
      <span>DopeDB</span>
      <small>Workspace</small>
    </Link>
  );
}
