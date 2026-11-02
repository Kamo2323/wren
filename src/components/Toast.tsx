// In-window toasts: a dark pill at the bottom centre of the content, one at a time, 2.4 s.
import { createContext, useCallback, useContext, useEffect, useRef, useState, type ReactNode } from "react";

export type ToastTone = "default" | "success" | "danger";

interface ToastItem {
  id: number;
  text: string;
  tone: ToastTone;
  icon?: ReactNode;
}

interface ToastApi {
  show: (text: string, opts?: { tone?: ToastTone; icon?: ReactNode; ms?: number }) => void;
}

const ToastContext = createContext<ToastApi>({ show: () => {} });

export function useToast(): ToastApi {
  return useContext(ToastContext);
}

export function ToastProvider({ children }: { children: ReactNode }) {
  const [item, setItem] = useState<ToastItem | null>(null);
  const [leaving, setLeaving] = useState(false);
  const timers = useRef<number[]>([]);
  const nextId = useRef(1);

  const clearTimers = () => {
    timers.current.forEach((t) => window.clearTimeout(t));
    timers.current = [];
  };

  const show = useCallback<ToastApi["show"]>((text, opts) => {
    clearTimers();
    setLeaving(false);
    setItem({ id: nextId.current++, text, tone: opts?.tone ?? "default", icon: opts?.icon });
    const ms = opts?.ms ?? 2400;
    timers.current.push(window.setTimeout(() => setLeaving(true), ms));
    timers.current.push(window.setTimeout(() => setItem(null), ms + 200));
  }, []);

  useEffect(() => clearTimers, []);

  return (
    <ToastContext.Provider value={{ show }}>
      {children}
      <div className="toast-layer" aria-live="polite">
        {item && (
          <div key={item.id} className={`toast toast--${item.tone} ${leaving ? "is-leaving" : ""}`}>
            {item.icon && <span className="toast__icon">{item.icon}</span>}
            {item.text}
          </div>
        )}
      </div>
    </ToastContext.Provider>
  );
}
