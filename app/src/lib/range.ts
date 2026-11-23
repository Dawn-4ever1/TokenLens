export type RangePreset = "today" | "week" | "month" | "all" | "custom";
export type DeviceScope = "all" | "current";

export const RANGE_PRESETS: { key: RangePreset; label: string }[] = [
  { key: "today", label: "日" },
  { key: "week", label: "周" },
  { key: "month", label: "月" },
  { key: "all", label: "总计" },
  { key: "custom", label: "自定义" },
];

export interface Filters {
  preset: RangePreset;
  customFrom?: string;
  customTo?: string;
  devices: string[];
  deviceScope: DeviceScope;
  clients: string[];
  models: string[];
  projects: string[];
}

export const DEFAULT_FILTERS: Filters = {
  preset: "month",
  devices: [],
  deviceScope: "all",
  clients: [],
  models: [],
  projects: [],
};

export const DEFAULT_TIME_ZONE = "Asia/Shanghai";

export function systemTimeZone(): string {
  try {
    return Intl.DateTimeFormat().resolvedOptions().timeZone || DEFAULT_TIME_ZONE;
  } catch {
    return DEFAULT_TIME_ZONE;
  }
}

export function isValidDate(value: string): boolean {
  if (!/^\d{4}-\d{2}-\d{2}$/.test(value)) return false;
  if (value < "2000-01-01" || value > "2100-12-31") return false;
  const [y, m, d] = value.split("-").map(Number);
  const dt = new Date(Date.UTC(y!, m! - 1, d!));
  return dt.getUTCFullYear() === y && dt.getUTCMonth() === m! - 1 && dt.getUTCDate() === d;
}

const dayFormats = new Map<string, Intl.DateTimeFormat>();

export function todayIn(timeZone: string, now = new Date()): string {
  let format = dayFormats.get(timeZone);
  if (!format) {
    format = new Intl.DateTimeFormat("en-CA", {
      timeZone,
      year: "numeric",
      month: "2-digit",
      day: "2-digit",
    });
    dayFormats.set(timeZone, format);
  }
  return format.format(now);
}

export function addDays(date: string, days: number): string {
  const [y, m, d] = date.split("-").map(Number);
  const ms = Date.UTC(y!, m! - 1, d!) + days * 86_400_000;
  return new Date(ms).toISOString().slice(0, 10);
}

export function daysBetween(from: string, to: string): number {
  return Math.round((Date.parse(`${to}T00:00:00Z`) - Date.parse(`${from}T00:00:00Z`)) / 86_400_000);
}

export interface DateSpan {
  from?: string;
  to?: string;
}

/** A calendar week starts on Monday, including the current day. */
export function weekStart(date: string): string {
  const weekday = new Date(`${date}T00:00:00Z`).getUTCDay();
  return addDays(date, weekday === 0 ? -6 : 1 - weekday);
}

function normalizePreset(preset: string): RangePreset {
  // Preserve the approximate meaning of windows saved by older app versions.
  if (preset === "7d") return "week";
  if (preset === "30d") return "month";
  return RANGE_PRESETS.some(({ key }) => key === preset) ? preset as RangePreset : DEFAULT_FILTERS.preset;
}

/** Older builds accepted any string from storage; never let it silently select all history. */
export function normalizeFilters(value: unknown): Filters | null {
  if (typeof value !== "object" || value === null) return null;
  const candidate = value as Partial<Filters>;
  if (typeof candidate.preset !== "string") return null;
  const filters: Filters = {
    ...DEFAULT_FILTERS,
    preset: normalizePreset(candidate.preset),
    customFrom: typeof candidate.customFrom === "string" && isValidDate(candidate.customFrom) ? candidate.customFrom : undefined,
    customTo: typeof candidate.customTo === "string" && isValidDate(candidate.customTo) ? candidate.customTo : undefined,
    deviceScope: candidate.deviceScope === "all" || candidate.deviceScope === "current"
      ? candidate.deviceScope
      : DEFAULT_FILTERS.deviceScope,
  };
  for (const key of ["devices", "clients", "models", "projects"] as const) {
    if (candidate[key] !== undefined) {
      if (!Array.isArray(candidate[key]) || candidate[key].some((item) => typeof item !== "string")) return null;
      filters[key] = candidate[key];
    }
  }
  return filters;
}

export function resolveRange(filters: Filters, timeZone: string, now = new Date()): DateSpan {
  const today = todayIn(timeZone, now);
  switch (filters.preset) {
    case "today":
      return { from: today, to: today };
    case "week":
      return { from: weekStart(today), to: today };
    case "month":
      return { from: `${today.slice(0, 8)}01`, to: today };
    case "all":
      return {};
    case "custom": {
      const from = filters.customFrom && isValidDate(filters.customFrom) ? filters.customFrom : undefined;
      const to = filters.customTo && isValidDate(filters.customTo) ? filters.customTo : undefined;
      if (from && to && from > to) return { from: to, to: from };
      return { from, to };
    }
  }
}

/** The equally long span right before `span`, for period-over-period deltas. */
export function previousSpan(span: DateSpan): DateSpan | null {
  if (!span.from || !span.to) return null;
  const length = daysBetween(span.from, span.to) + 1;
  return { from: addDays(span.from, -length), to: addDays(span.from, -1) };
}

export function rangeLabel(filters: Filters, span: DateSpan): string {
  if (filters.preset === "all") return "总计";
  if (filters.preset === "custom" && span.from && span.to) return `${span.from} – ${span.to}`;
  return RANGE_PRESETS.find((preset) => preset.key === filters.preset)?.label ?? "";
}

export type Query = Record<string, string | undefined>;

export function filterQuery(filters: Filters, timeZone: string, overrides: Partial<Filters> = {}): Query {
  const merged = { ...filters, ...overrides };
  const span = resolveRange(merged, timeZone);
  const join = (items: string[]) => (items.length ? items.join(",") : undefined);
  return {
    from: span.from,
    to: span.to,
    devices: join(merged.devices),
    deviceScope: merged.deviceScope,
    clients: join(merged.clients),
    models: join(merged.models),
    projects: join(merged.projects),
  };
}

export function activeFilterCount(filters: Filters, local = false): number {
  return filters.devices.length + filters.clients.length + filters.models.length + filters.projects.length
    + (!local && filters.deviceScope !== "all" ? 1 : 0);
}
