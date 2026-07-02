// Inline two-step confirm: one click arms it ("Really delete? Yes / No"), auto-reverts
// after 3s if untouched. No window.confirm. Styling reuses .btn; inline flex, no css file.
import { useEffect, useRef, useState, type ReactNode } from "react";

export default function ConfirmButton({
  children,
  onConfirm,
  className = "btn",
  disabled,
  confirmLabel = "Really delete?",
}: {
  children: ReactNode;
  onConfirm: () => void;
  className?: string;
  disabled?: boolean;
  confirmLabel?: string;
}) {
  const [armed, setArmed] = useState(false);
  const timer = useRef<number>();

  useEffect(() => () => window.clearTimeout(timer.current), []);

  if (!armed) {
    return (
      <button
        className={className}
        disabled={disabled}
        onClick={() => {
          setArmed(true);
          timer.current = window.setTimeout(() => setArmed(false), 3000);
        }}
      >
        {children}
      </button>
    );
  }

  return (
    <span style={{ display: "inline-flex", alignItems: "center", gap: 6 }}>
      <span className="muted">{confirmLabel}</span>
      <button
        className="btn danger small"
        disabled={disabled}
        onClick={() => {
          window.clearTimeout(timer.current);
          setArmed(false);
          onConfirm();
        }}
      >
        Yes
      </button>
      <button
        className="btn small"
        disabled={disabled}
        onClick={() => {
          window.clearTimeout(timer.current);
          setArmed(false);
        }}
      >
        No
      </button>
    </span>
  );
}
