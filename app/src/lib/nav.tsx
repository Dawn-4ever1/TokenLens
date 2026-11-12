import { createContext, useCallback, useContext, useMemo, useState, type ReactNode } from "react";
import { useData } from "./data";
import type { Filters } from "./range";
import { usePersistent } from "./store";

export type Page = "overview" | "trends" | "sessions" | "devices" | "settings";

export const PAGES: { key: Page; label: string }[] = [
  { key: "overview", label: "总览" },
  { key: "trends", label: "趋势" },
  { key: "sessions", label: "会话" },
  { key: "devices", label: "设备" },
  { key: "settings", label: "设置" },
];

interface NavContext {
  page: Page;
  /** Switches page; `patch` narrows the global filters first (drill-down). */
  go: (page: Page, patch?: Partial<Filters>) => void;
  sessionKey: string | null;
  openSession: (key: string | null) => void;
}

const Context = createContext<NavContext | null>(null);

const isPage = (value: unknown): value is Page => PAGES.some((page) => page.key === value);

export function NavProvider({ children }: { children: ReactNode }) {
  const { setFilters } = useData();
  const [page, setPage] = usePersistent<Page>("tokenlens.page.v1", "overview", isPage);
  const [sessionKey, setSessionKey] = useState<string | null>(null);
  const go = useCallback(
    (next: Page, patch?: Partial<Filters>) => {
      if (patch) setFilters((previous) => ({ ...previous, ...patch }));
      setPage(next);
    },
    [setFilters, setPage],
  );
  const value = useMemo(() => ({ page, go, sessionKey, openSession: setSessionKey }), [page, go, sessionKey]);
  return <Context.Provider value={value}>{children}</Context.Provider>;
}

export function useNav(): NavContext {
  const context = useContext(Context);
  if (!context) throw new Error("NavProvider missing");
  return context;
}
