import { useCallback, useEffect, useState } from "react";

const isRecord = (value: unknown): value is Record<string, unknown> =>
  typeof value === "object" && value !== null && !Array.isArray(value);

/** A small piece of UI state kept in the WebView's localStorage. */
export function usePersistent<T>(key: string, initial: T, validate?: (value: unknown) => value is T) {
  const [value, setValue] = useState<T>(() => {
    try {
      const raw = localStorage.getItem(key);
      if (raw === null) return initial;
      const parsed: unknown = JSON.parse(raw);
      if (validate && !validate(parsed)) return initial;
      if (isRecord(initial) && isRecord(parsed)) return { ...initial, ...parsed } as T;
      return parsed as T;
    } catch {
      return initial;
    }
  });
  useEffect(() => {
    try {
      localStorage.setItem(key, JSON.stringify(value));
    } catch {
      // Storage full or unavailable; the state still works for this window.
    }
  }, [key, value]);
  const update = useCallback((next: T | ((previous: T) => T)) => setValue(next), []);
  return [value, update] as const;
}
