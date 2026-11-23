import { createContext, useCallback, useContext, useEffect, useMemo, useRef, useState, type ReactNode } from "react";
import { app, isTauri } from "./api";
import type { AppStatus } from "./types";

interface StatusContext {
  status: AppStatus | null;
  error: unknown;
  refresh: () => Promise<void>;
  set: (status: AppStatus) => void;
}

const Context = createContext<StatusContext | null>(null);

/** Poll local state; restore the saved account separately once at startup. */
export function StatusProvider({ children }: { children: ReactNode }) {
  const [status, setStatus] = useState<AppStatus | null>(null);
  const [error, setError] = useState<unknown>(null);
  const revision = useRef(0);
  const accountRevision = useRef(0);
  const accountRestore = useRef<Promise<string | null> | null>(null);
  const set = useCallback((next: AppStatus) => {
    revision.current += 1;
    accountRevision.current += 1;
    setStatus(next);
    setError(null);
  }, []);
  const refresh = useCallback(async () => {
    const current = ++revision.current;
    try {
      const next = await app.status();
      if (revision.current !== current) return;
      setStatus((previous) => (previous && JSON.stringify(previous) === JSON.stringify(next) ? previous : next));
      setError(null);
    } catch (next) {
      if (revision.current === current) setError(next);
    }
  }, []);
  useEffect(() => {
    void refresh();
    let active = true;
    const restoringRevision = accountRevision.current;
    // Keep the same promise through StrictMode's effect replay. Local status
    // and the dashboard stay available while this optional request is pending.
    accountRestore.current ??= app.refreshAccount();
    void accountRestore.current.then(() => {
      if (active && accountRevision.current === restoringRevision) void refresh();
    }, () => {
      // Offline account recovery must not turn local collection into an error.
    });
    const id = setInterval(() => {
      if (document.visibilityState === "visible") void refresh();
    }, 5000);
    let unlisten: (() => void) | undefined;
    if (isTauri()) {
      void import("@tauri-apps/api/event")
        .then(({ listen }) =>
          listen("status-changed", () => void refresh()).then((stop) => {
            if (!active) stop();
            else unlisten = stop;
          }),
        )
        .catch(() => undefined);
    }
    return () => {
      active = false;
      clearInterval(id);
      unlisten?.();
    };
  }, [refresh]);
  const value = useMemo(() => ({ status, error, refresh, set }), [status, error, refresh, set]);
  return <Context.Provider value={value}>{children}</Context.Provider>;
}

export function useStatus(): StatusContext {
  const context = useContext(Context);
  if (!context) throw new Error("StatusProvider missing");
  return context;
}
