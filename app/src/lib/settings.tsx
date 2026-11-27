import { createContext, useContext, useEffect, useLayoutEffect, useMemo, useState, type ReactNode } from "react";
import { usePersistent } from "./store";

export type ThemeMode = "light" | "dark" | "system";
export type Direction = "editorial" | "graphite" | "paper";

export const DIRECTIONS: { key: Direction; label: string; description: string }[] = [
  { key: "editorial", label: "舒适", description: "大数字、暖色构成条与暖色圆角柱（推荐）" },
  { key: "graphite", label: "终端", description: "等宽数字、格线分区与规整柱状图" },
  { key: "paper", label: "书卷", description: "报纸式标题、自动结论句与纤细墨色柱" },
];

export interface Settings {
  theme: ThemeMode;
  direction: Direction;
  refreshSeconds: number;
  /** Token counts show every digit instead of K/M/B. */
  exactNumbers: boolean;
}

const DEFAULTS: Settings = { theme: "system", direction: "editorial", refreshSeconds: 60, exactNumbers: false };
const REFRESH_CHOICES = new Set([0, 30, 60, 300]);

export function normalizeSettings(value: unknown): Settings | null {
  if (typeof value !== "object" || value === null || Array.isArray(value)) return null;
  const c = value as Partial<Settings>;
  const theme: ThemeMode = c.theme === "light" || c.theme === "dark" || c.theme === "system" ? c.theme : DEFAULTS.theme;
  const direction: Direction = c.direction === "editorial" || c.direction === "graphite" || c.direction === "paper" ? c.direction : DEFAULTS.direction;
  const refreshSeconds = typeof c.refreshSeconds === "number" && Number.isFinite(c.refreshSeconds) && REFRESH_CHOICES.has(c.refreshSeconds)
    ? c.refreshSeconds
    : DEFAULTS.refreshSeconds;
  const exactNumbers = typeof c.exactNumbers === "boolean" ? c.exactNumbers : DEFAULTS.exactNumbers;
  return { theme, direction, refreshSeconds, exactNumbers };
}

interface SettingsContext {
  settings: Settings;
  update: (patch: Partial<Settings>) => void;
  dark: boolean;
  /** Changes whenever colours or chart geometry change (after DOM attributes update). */
  paletteVersion: string;
  appliedDirection: Direction;
}

const Context = createContext<SettingsContext | null>(null);

function systemDark(): boolean {
  return typeof matchMedia !== "undefined" && matchMedia("(prefers-color-scheme: dark)").matches;
}

function applyRootTheme(direction: Direction, dark: boolean): { direction: Direction; version: string } {
  if (typeof document !== "undefined") {
    const root = document.documentElement;
    root.classList.toggle("dark", dark);
    root.dataset.direction = direction;
    root.style.colorScheme = dark ? "dark" : "light";
  }
  return { direction, version: `${direction}-${dark ? "dark" : "light"}` };
}

export function SettingsProvider({ children }: { children: ReactNode }) {
  const [settings, setSettings] = usePersistent<Settings>("tokenlens.settings.v1", DEFAULTS, undefined, normalizeSettings);
  const [prefersDark, setPrefersDark] = useState(systemDark);
  useEffect(() => {
    if (typeof matchMedia === "undefined") return;
    const query = matchMedia("(prefers-color-scheme: dark)");
    const listener = () => setPrefersDark(query.matches);
    query.addEventListener("change", listener);
    return () => query.removeEventListener("change", listener);
  }, []);
  const dark = settings.theme === "dark" || (settings.theme === "system" && prefersDark);
  const [applied, setApplied] = useState(() => applyRootTheme(settings.direction, dark));
  useLayoutEffect(() => {
    const next = applyRootTheme(settings.direction, dark);
    setApplied((previous) =>
      previous.version === next.version && previous.direction === next.direction ? previous : next,
    );
  }, [dark, settings.direction]);
  const value = useMemo<SettingsContext>(
    () => ({
      settings,
      update: (patch) => setSettings((previous) => normalizeSettings({ ...previous, ...patch }) ?? DEFAULTS),
      dark,
      paletteVersion: applied.version,
      appliedDirection: applied.direction,
    }),
    [settings, setSettings, dark, applied],
  );
  return <Context.Provider value={value}>{children}</Context.Provider>;
}

export function useSettings(): SettingsContext {
  const context = useContext(Context);
  if (!context) throw new Error("SettingsProvider missing");
  return context;
}

/** Whether token counts are shown in full, and a toggle shared by every count on screen. */
export function useExactNumbers(): [boolean, () => void] {
  const { settings, update } = useSettings();
  const exact = settings.exactNumbers === true;
  return [exact, () => update({ exactNumbers: !exact })];
}
