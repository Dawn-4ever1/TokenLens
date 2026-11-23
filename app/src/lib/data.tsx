import { createContext, useCallback, useContext, useEffect, useMemo, useRef, useState, type ReactNode } from "react";
import { dashboardGet } from "./api";
import { DEFAULT_FILTERS, filterQuery, normalizeFilters, systemTimeZone, type Filters, type Query } from "./range";
import { usePersistent } from "./store";
import { useSettings } from "./settings";
import type { FilterOptions } from "./types";

interface DataContext {
  sourceKey: string;
  filters: Filters;
  setFilters: (next: Filters | ((previous: Filters) => Filters)) => void;
  timeZone: string;
  options: FilterOptions | null;
  /** Increments on manual refresh and on the auto-refresh interval. */
  tick: number;
  refresh: () => void;
  lastLoadedAt: number | null;
  markLoaded: () => void;
  query: (overrides?: Partial<Filters>) => Query;
}

const Context = createContext<DataContext | null>(null);

/** Source IDs are non-secret. Keep dates, but never carry device/project filters between sources. */
function initialFilters(sourceKey: string): Filters {
  const local = sourceKey.startsWith("local:");
  try {
    const previous: unknown = JSON.parse(localStorage.getItem("tokenlens.filters.v1") ?? "null");
    const restored = normalizeFilters(previous);
    // Do not carry another computer's device/project filters into this one.
    // The previous provider may still be flushing its storage while unmounting.
    if (local && restored) {
      return { ...restored, devices: [], deviceScope: "current" };
    }
    const savedSource = localStorage.getItem("tokenlens.source.v1");
    if (savedSource === sourceKey && restored) {
      return restored.deviceScope === "all" ? restored : { ...restored, deviceScope: "all" };
    }
    if (restored) {
      return {
        ...restored,
        devices: [],
        deviceScope: local ? "current" : "all",
        clients: [],
        models: [],
        projects: [],
      };
    }
  } catch {
    // Storage is optional.
  }
  return local ? { ...DEFAULT_FILTERS, deviceScope: "current" } : DEFAULT_FILTERS;
}

export function DataProvider({ children, enabled, sourceKey }: { children: ReactNode; enabled: boolean; sourceKey: string }) {
  const { settings } = useSettings();
  const [initial] = useState(() => initialFilters(sourceKey));
  // `initialFilters` has already read the previous source and removed its
  // device/project filters. Reading the same storage again here would undo
  // that reset when switching from all devices back to this computer.
  const [filters, setFilters] = usePersistent<Filters>("tokenlens.filters.v1", initial, undefined, () => initial);
  useEffect(() => {
    try {
      localStorage.setItem("tokenlens.source.v1", sourceKey);
    } catch {
      // Storage is optional.
    }
  }, [sourceKey]);
  const [options, setOptions] = useState<FilterOptions | null>(null);
  const hasOptions = useRef(false);
  hasOptions.current = options !== null;
  const [tick, setTick] = useState(0);
  const [lastLoadedAt, setLastLoadedAt] = useState<number | null>(null);
  const refresh = useCallback(() => setTick((t) => t + 1), []);
  const markLoaded = useCallback(() => setLastLoadedAt(Date.now()), []);
  useEffect(() => {
    if (!enabled || settings.refreshSeconds <= 0) return;
    const id = setInterval(() => {
      if (document.visibilityState === "visible") setTick((t) => t + 1);
    }, settings.refreshSeconds * 1000);
    return () => clearInterval(id);
  }, [enabled, settings.refreshSeconds]);
  useEffect(() => {
    if (!enabled) return;
    let alive = true;
    let retryTimer: ReturnType<typeof setTimeout> | null = null;
    const load = () => {
      dashboardGet<FilterOptions>("filters")
        .then((next) => {
          if (alive) setOptions(next);
        })
        .catch(() => {
          if (alive && !hasOptions.current && !retryTimer) {
            retryTimer = setTimeout(() => {
              retryTimer = null;
              if (alive) load();
            }, 3000);
          }
        });
    };
    load();
    return () => {
      alive = false;
      if (retryTimer) clearTimeout(retryTimer);
    };
  }, [enabled, tick]);
  const fallbackZone = useMemo(() => systemTimeZone(), []);
  const timeZone = options?.timeZone ?? fallbackZone;
  const value = useMemo<DataContext>(
    () => ({
      sourceKey,
      filters,
      setFilters,
      timeZone,
      options,
      tick,
      refresh,
      lastLoadedAt,
      markLoaded,
      query: (overrides) => filterQuery(filters, timeZone, overrides),
    }),
    [sourceKey, filters, setFilters, timeZone, options, tick, refresh, lastLoadedAt, markLoaded],
  );
  return <Context.Provider value={value}>{children}</Context.Provider>;
}

export function useData(): DataContext {
  const context = useContext(Context);
  if (!context) throw new Error("DataProvider missing");
  return context;
}

export interface Loaded<T> {
  data: T | null;
  error: unknown;
  loading: boolean;
  reload: () => void;
}

/** Fetches a dashboard endpoint; keeps the previous data while reloading. */
export function useDashboard<T>(path: string | null, query: Query = {}): Loaded<T> {
  const { tick, markLoaded, sourceKey } = useData();
  const [state, setState] = useState<{ data: T | null; error: unknown; loading: boolean }>({
    data: null,
    error: null,
    loading: path !== null,
  });
  const [local, setLocal] = useState(0);
  const key = path === null ? null : `${sourceKey}/${path}?${JSON.stringify(query)}`;
  const latest = useRef(key);
  latest.current = key;
  const previousKey = useRef<string | null>(key);
  useEffect(() => {
    if (path === null || key === null) {
      previousKey.current = null;
      setState({ data: null, error: null, loading: false });
      return;
    }
    const changed = previousKey.current !== key;
    previousKey.current = key;
    let alive = true;
    setState((previous) => ({ ...previous, error: changed ? null : previous.error, loading: true }));
    dashboardGet<T>(path, query)
      .then((data) => {
        if (!alive || latest.current !== key) return;
        setState({ data, error: null, loading: false });
        markLoaded();
      })
      .catch((error: unknown) => {
        if (!alive || latest.current !== key) return;
        setState((previous) => ({ data: previous.data, error, loading: false }));
      });
    return () => {
      alive = false;
    };
    // `key` covers path and query.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [key, tick, local]);
  const reload = useCallback(() => setLocal((n) => n + 1), []);
  return { ...state, reload };
}
