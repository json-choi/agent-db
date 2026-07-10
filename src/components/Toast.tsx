// Tiny toast system: context + hook + a fixed corner stack. 3s auto-dismiss,
// success/error variants. useToast() returns a `toast(msg, variant?)` fn.
import { createContext, useCallback, useContext, useRef, useState, type ReactNode } from "react";
import "./Toast.css";

type Variant = "success" | "error";
interface ToastItem {
  id: number;
  msg: string;
  variant: Variant;
}

type ToastFn = (msg: string, variant?: Variant) => void;

const Ctx = createContext<ToastFn>(() => {});

export function useToast(): ToastFn {
  return useContext(Ctx);
}

export function ToastProvider({ children }: { children: ReactNode }) {
  const [toasts, setToasts] = useState<ToastItem[]>([]);
  const idRef = useRef(0);

  const dismiss = useCallback((id: number) => setToasts((t) => t.filter((x) => x.id !== id)), []);

  const toast = useCallback<ToastFn>((msg, variant = "success") => {
    const id = idRef.current++;
    // cap the stack so a burst can't overflow off-screen
    setToasts((t) => [...t, { id, msg, variant }].slice(-4));
    window.setTimeout(() => dismiss(id), 3000);
  }, [dismiss]);

  return (
    <Ctx.Provider value={toast}>
      {children}
      {/* live region so screen readers announce toasts */}
      <div className="toast-wrap" role="region" aria-label="Notifications">
        {toasts.map((t) => (
          <div
            key={t.id}
            className={`toast ${t.variant}`}
            role={t.variant === "error" ? "alert" : "status"}
            onClick={() => dismiss(t.id)}
          >
            {t.msg}
          </div>
        ))}
      </div>
    </Ctx.Provider>
  );
}
