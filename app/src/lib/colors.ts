import { useMemo } from "react";
import { useSettings } from "./settings";

let probe: HTMLSpanElement | null = null;
let canvas: CanvasRenderingContext2D | null | undefined;

/**
 * Canvas charts cannot read CSS variables, oklch() or color-mix() on every
 * WebView, so colours are resolved to rgba() through the DOM and a 1px canvas.
 */
export function resolveColor(css: string): string {
  if (typeof document === "undefined") return css;
  if (!probe) {
    probe = document.createElement("span");
    probe.style.display = "none";
    document.body.appendChild(probe);
  }
  probe.style.color = "";
  probe.style.color = css;
  const computed = getComputedStyle(probe).color || css;
  if (canvas === undefined) {
    try {
      canvas = document.createElement("canvas").getContext("2d", { willReadFrequently: true });
    } catch {
      canvas = null;
    }
  }
  if (!canvas) return computed;
  canvas.clearRect(0, 0, 1, 1);
  canvas.fillStyle = "#000";
  canvas.fillStyle = computed;
  canvas.fillRect(0, 0, 1, 1);
  const [r, g, b, a] = canvas.getImageData(0, 0, 1, 1).data;
  return `rgba(${r}, ${g}, ${b}, ${Math.round(((a ?? 255) / 255) * 1000) / 1000})`;
}

export interface Palette {
  ink: string;
  ink2: string;
  ink3: string;
  surface: string;
  border: string;
  grid: string;
  primary: string;
  primarySoft: string;
  accent: string;
  bad: string;
  resolve: (css: string) => string;
}

export function usePalette(): Palette {
  const { paletteVersion } = useSettings();
  return useMemo(() => {
    const cache = new Map<string, string>();
    const resolve = (css: string) => {
      let found = cache.get(css);
      if (!found) cache.set(css, (found = resolveColor(css)));
      return found;
    };
    return {
      ink: resolve("var(--ink)"),
      ink2: resolve("var(--ink-2)"),
      ink3: resolve("var(--ink-3)"),
      surface: resolve("var(--surface)"),
      border: resolve("var(--border)"),
      grid: resolve("var(--chart-grid)"),
      primary: resolve("var(--primary)"),
      primarySoft: resolve("var(--primary-soft)"),
      accent: resolve("var(--accent)"),
      bad: resolve("var(--bad)"),
      resolve,
    };
    // paletteVersion is the dependency: the theme attributes changed.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [paletteVersion]);
}
