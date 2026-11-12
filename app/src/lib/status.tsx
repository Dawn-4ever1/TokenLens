import { createContext, useCallback, useContext, useEffect, useMemo, useState, type ReactNode } from "react";
import { app, isTauri } from "./api";
import type { AppStatus } from "./types";

interface StatusContext {
  status: AppStatus | null;
  error: unknown;
  refresh: () => Promise<void>;
  set: (status: AppStatus) => void;
}

const Context = createContext<StatusContext | null>(null);

/** Local app state (pairing, collector); reading it never touches the network. */
export function StatusProvider({ children }: { children: ReactNode }) {
  const [status, setStatus] = useState<AppStatus | null>(null);
  const [error, setError] = useState<unknown>(null);
  const refresh = useCallback(async () => {
    try {
      setStatus(await app.status());
      setError(null);
    } catch (next) {
      setError(next);
    }
  }, []);
  useEffect(() => {
    void refresh();
    const id = setInterval(() => {
      if (document.visibilityState === "visible") void refresh();
    }, 5000);
    let unlisten: (() => void) | undefined;
    if (isTauri()) {
      void import("@tauri-apps/api/event").then(({ listen }) =>
        listen("status-changed", () => void refresh()).then((stop) => {
          unlisten = stop;
        }),
      );
    }
    return () => {
      clearInterval(id);
      unlisten?.();
    };
  }, [refresh]);
  const value = useMemo(() => ({ status, error, refresh, set: setStatus }), [status, error, refresh]);
  return <Context.Provider value={value}>{children}</Context.Provider>;
}

export function useStatus(): StatusContext {
  const context = useContext(Context);
  if (!context) throw new Error("StatusProvider missing");
  return context;
}
