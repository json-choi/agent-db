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

export type ToastFn = (msg: string, variant?: Variant) => void;

const Ctx = createContext<ToastFn>(() => {});

export function useToast(): ToastFn {
  return useContext(Ctx);
}

export function ToastProvider({ children }: { children: ReactNode }) {
  const [toasts, setToasts] = useState<ToastItem[]>([]);
  const idRef = useRef(0);

  const toast = useCallback<ToastFn>((msg, variant = "success") => {
    const id = idRef.current++;
    setToasts((t) => [...t, { id, msg, variant }]);
    window.setTimeout(() => setToasts((t) => t.filter((x) => x.id !== id)), 3000);
  }, []);

  return (
    <Ctx.Provider value={toast}>
      {children}
      <div className="toast-wrap">
        {toasts.map((t) => (
          <div key={t.id} className={`toast ${t.variant}`}>
            {t.msg}
          </div>
        ))}
      </div>
    </Ctx.Provider>
  );
}
