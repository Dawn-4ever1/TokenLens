import { createContext, useCallback, useContext, useEffect, useMemo, useRef, useState, type ReactNode } from "react";
import { app, isTauri } from "./api";
import type { UpdateStatus } from "./types";

interface UpdateContext {
  status: UpdateStatus | null;
  busy: boolean;
  dialogOpen: boolean;
  setDialogOpen: (open: boolean) => void;
  check: () => Promise<UpdateStatus>;
  install: () => Promise<UpdateStatus>;
  setAutoCheck: (enabled: boolean) => Promise<UpdateStatus>;
}

const Context = createContext<UpdateContext | null>(null);
const FALLBACK: UpdateStatus = {
  autoCheck: true, phase: "idle", version: null, notes: null,
  downloadedBytes: 0, totalBytes: null, error: null, checkedAt: null,
};

export const updateInProgress = (status: UpdateStatus | null): boolean =>
  status?.phase === "downloading" || status?.phase === "installing";

export function downloadPercent(status: UpdateStatus): number | null {
  if (!status.totalBytes || status.totalBytes <= 0) return null;
  return Math.min(100, Math.max(0, Math.round(status.downloadedBytes / status.totalBytes * 100)));
}

/** Only reads local Rust state. Rust owns the 10-second / 24-hour network schedule. */
export function UpdateProvider({ children }: { children: ReactNode }) {
  const [status, setStatus] = useState<UpdateStatus | null>(null);
  const [busy, setBusy] = useState(false);
  const [dialogOpen, setDialogOpen] = useState(false);
  const mounted = useRef(true);
  const refreshing = useRef(false);
  const snapshot = useRef<UpdateStatus | null>(null);
  const accept = useCallback((next: UpdateStatus) => {
    snapshot.current = next;
    if (mounted.current) setStatus(next);
    return next;
  }, []);
  const failure = useCallback((message: string) => accept({
    ...(snapshot.current ?? FALLBACK), phase: "error", error: message,
  }), [accept]);
  const refresh = useCallback(async () => {
    if (refreshing.current) return;
    refreshing.current = true;
    try { accept(await app.updateStatus()); }
    catch { failure("暂时无法读取更新状态，请稍后再试。"); }
    finally { refreshing.current = false; }
  }, [accept, failure]);
  useEffect(() => {
    mounted.current = true;
    void refresh();
    const timer = setInterval(() => {
      if (document.visibilityState === "visible") void refresh();
    }, 1000);
    let stopped = false;
    let unlisten: (() => void) | undefined;
    if (isTauri()) {
      void import("@tauri-apps/api/event").then(({ listen }) =>
        listen("update-changed", () => void refresh()).then((stop) => {
          if (stopped) stop();
          else unlisten = stop;
        }),
      ).catch(() => { /* Polling remains available if events are unavailable. */ });
    }
    return () => {
      mounted.current = false;
      stopped = true;
      clearInterval(timer);
      unlisten?.();
    };
  }, [refresh]);
  const run = useCallback(async (action: () => Promise<UpdateStatus>, message: string) => {
    setBusy(true);
    try { return accept(await action()); }
    catch { return failure(message); }
    finally { if (mounted.current) setBusy(false); }
  }, [accept, failure]);
  const check = useCallback(() => run(app.checkUpdate, "连不上 GitHub，稍后会再试。"), [run]);
  const install = useCallback(() => run(app.installUpdate, "更新没有完成，原来的版本仍可使用，请稍后重试。"), [run]);
  const setAutoCheck = useCallback((enabled: boolean) => run(
    () => app.setAutoCheckUpdates(enabled), "没能保存自动检查设置，请稍后再试。",
  ), [run]);
  const value = useMemo(() => ({ status, busy, dialogOpen, setDialogOpen, check, install, setAutoCheck }),
    [status, busy, dialogOpen, check, install, setAutoCheck]);
  return <Context.Provider value={value}>{children}</Context.Provider>;
}

export function useUpdates(): UpdateContext {
  const context = useContext(Context);
  if (!context) throw new Error("UpdateProvider missing");
  return context;
}
