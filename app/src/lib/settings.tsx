import { createContext, useContext, useEffect, useMemo, useState, type ReactNode } from "react";
import { usePersistent } from "./store";

export type ThemeMode = "light" | "dark" | "system";
export type Direction = "editorial" | "graphite" | "paper";

export const DIRECTIONS: { key: Direction; label: string; description: string }[] = [
  { key: "editorial", label: "暖白", description: "暖米色背景、墨蓝主色，安静耐看（推荐）" },
  { key: "graphite", label: "石墨", description: "冷灰背景、亮蓝主色，偏工具感" },
  { key: "paper", label: "墨纸", description: "纸张底色、宋体标题、细线分隔，偏杂志感" },
];

export interface Settings {
  theme: ThemeMode;
  direction: Direction;
  refreshSeconds: number;
  /** Token counts show every digit instead of K/M/B. */
  exactNumbers: boolean;
}

const DEFAULTS: Settings = { theme: "system", direction: "editorial", refreshSeconds: 60, exactNumbers: false };

interface SettingsContext {
  settings: Settings;
  update: (patch: Partial<Settings>) => void;
  dark: boolean;
  /** Changes whenever resolved colours change (theme or direction). */
  paletteVersion: string;
}

const Context = createContext<SettingsContext | null>(null);

function systemDark(): boolean {
  return typeof matchMedia !== "undefined" && matchMedia("(prefers-color-scheme: dark)").matches;
}

export function SettingsProvider({ children }: { children: ReactNode }) {
  const [settings, setSettings] = usePersistent<Settings>("tokenlens.settings.v1", DEFAULTS);
  const [prefersDark, setPrefersDark] = useState(systemDark);
  useEffect(() => {
    if (typeof matchMedia === "undefined") return;
    const query = matchMedia("(prefers-color-scheme: dark)");
    const listener = () => setPrefersDark(query.matches);
    query.addEventListener("change", listener);
    return () => query.removeEventListener("change", listener);
  }, []);
  const dark = settings.theme === "dark" || (settings.theme === "system" && prefersDark);
  useEffect(() => {
    const root = document.documentElement;
    root.classList.toggle("dark", dark);
    root.dataset.direction = settings.direction;
    root.style.colorScheme = dark ? "dark" : "light";
  }, [dark, settings.direction]);
  const value = useMemo<SettingsContext>(
    () => ({
      settings,
      update: (patch) => setSettings((previous) => ({ ...previous, ...patch })),
      dark,
      paletteVersion: `${settings.direction}-${dark ? "dark" : "light"}`,
    }),
    [settings, setSettings, dark],
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
