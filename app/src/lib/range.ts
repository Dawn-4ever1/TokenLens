export type RangePreset = "today" | "7d" | "30d" | "month" | "all" | "custom";

export const RANGE_PRESETS: { key: RangePreset; label: string }[] = [
  { key: "today", label: "今天" },
  { key: "7d", label: "7 天" },
  { key: "30d", label: "30 天" },
  { key: "month", label: "本月" },
  { key: "all", label: "全部" },
  { key: "custom", label: "自定义" },
];

export interface Filters {
  preset: RangePreset;
  customFrom?: string;
  customTo?: string;
  devices: string[];
  clients: string[];
  models: string[];
  projects: string[];
}

export const DEFAULT_FILTERS: Filters = {
  preset: "30d",
  devices: [],
  clients: [],
  models: [],
  projects: [],
};

export const DEFAULT_TIME_ZONE = "Asia/Shanghai";

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

export function resolveRange(filters: Filters, timeZone: string, now = new Date()): DateSpan {
  const today = todayIn(timeZone, now);
  switch (filters.preset) {
    case "today":
      return { from: today, to: today };
    case "7d":
      return { from: addDays(today, -6), to: today };
    case "30d":
      return { from: addDays(today, -29), to: today };
    case "month":
      return { from: `${today.slice(0, 8)}01`, to: today };
    case "all":
      return {};
    case "custom": {
      const from = filters.customFrom;
      const to = filters.customTo;
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
  if (filters.preset === "all") return "全部时间";
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
    clients: join(merged.clients),
    models: join(merged.models),
    projects: join(merged.projects),
  };
}

export function activeFilterCount(filters: Filters): number {
  return filters.devices.length + filters.clients.length + filters.models.length + filters.projects.length;
}
