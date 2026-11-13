import { createContext, useCallback, useContext, useEffect, useMemo, useRef, useState, type ReactNode } from "react";
import { dashboardGet } from "./api";
import { DEFAULT_FILTERS, DEFAULT_TIME_ZONE, filterQuery, type Filters, type Query } from "./range";
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

function isFilters(value: unknown): value is Filters {
  return Boolean(value) && typeof value === "object" && typeof (value as Filters).preset === "string";
}

/** Source IDs are non-secret. Keep dates, but never carry device/project filters between sources. */
function initialFilters(sourceKey: string): Filters {
  try {
    if (localStorage.getItem("tokenlens.source.v1") === sourceKey) return DEFAULT_FILTERS;
    const previous: unknown = JSON.parse(localStorage.getItem("tokenlens.filters.v1") ?? "null");
    if (isFilters(previous)) return { ...previous, devices: [], clients: [], models: [], projects: [] };
  } catch {
    // Storage is optional.
  }
  return DEFAULT_FILTERS;
}

export function DataProvider({ children, enabled, sourceKey }: { children: ReactNode; enabled: boolean; sourceKey: string }) {
  const { settings } = useSettings();
  const [initial] = useState(() => initialFilters(sourceKey));
  const [filters, setFilters] = usePersistent<Filters>("tokenlens.filters.v1", initial, (value): value is Filters => {
    try {
      return localStorage.getItem("tokenlens.source.v1") === sourceKey && isFilters(value);
    } catch {
      return false;
    }
  });
  useEffect(() => {
    try {
      localStorage.setItem("tokenlens.source.v1", sourceKey);
    } catch {
      // Storage is optional.
    }
  }, [sourceKey]);
  const [options, setOptions] = useState<FilterOptions | null>(null);
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
    dashboardGet<FilterOptions>("filters")
      .then((next) => alive && setOptions(next))
      .catch(() => undefined);
    return () => {
      alive = false;
    };
  }, [enabled, tick]);
  const timeZone = options?.timeZone ?? DEFAULT_TIME_ZONE;
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
  useEffect(() => {
    if (path === null || key === null) return;
    let alive = true;
    setState((previous) => ({ ...previous, loading: true }));
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
