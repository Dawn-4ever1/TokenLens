import { createContext, useCallback, useContext, useMemo, useRef, useState, type ReactNode } from "react";
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
  /**
   * Switches page. `patch` narrows the global filters first (drill-down);
   * leaving the drilled page puts the earlier filters back, unless the user
   * changed them in the meantime.
   */
  go: (page: Page, patch?: Partial<Filters>) => void;
  sessionKey: string | null;
  openSession: (key: string | null) => void;
}

const Context = createContext<NavContext | null>(null);

const isPage = (value: unknown): value is Page => PAGES.some((page) => page.key === value);

interface Drill {
  page: Page;
  before: Filters;
  applied: Filters;
}

const same = (a: Filters, b: Filters) => JSON.stringify(a) === JSON.stringify(b);

/** What `go` does to the filters; `null` filters mean "leave them alone". */
export function drillStep(
  pending: Drill | null,
  current: Filters,
  next: Page,
  patch?: Partial<Filters>,
): { drill: Drill | null; filters: Filters | null } {
  const untouched = pending !== null && same(pending.applied, current);
  if (patch) {
    const applied = { ...current, ...patch };
    // A drill-down from a drilled page still returns to the original filters.
    return { drill: { page: next, before: untouched ? pending.before : current, applied }, filters: applied };
  }
  if (!pending || next === pending.page) return { drill: pending, filters: null };
  return { drill: null, filters: untouched ? pending.before : null };
}

export function NavProvider({ children }: { children: ReactNode }) {
  const { filters, setFilters } = useData();
  const [page, setPage] = usePersistent<Page>("tokenlens.page.v1", "overview", isPage);
  const [sessionKey, setSessionKey] = useState<string | null>(null);
  const drill = useRef<Drill | null>(null);
  const go = useCallback(
    (next: Page, patch?: Partial<Filters>) => {
      const step = drillStep(drill.current, filters, next, patch);
      drill.current = step.drill;
      if (step.filters) setFilters(step.filters);
      setPage(next);
    },
    [filters, setFilters, setPage],
  );
  const value = useMemo(() => ({ page, go, sessionKey, openSession: setSessionKey }), [page, go, sessionKey]);
  return <Context.Provider value={value}>{children}</Context.Provider>;
}

export function useNav(): NavContext {
  const context = useContext(Context);
  if (!context) throw new Error("NavProvider missing");
  return context;
}
