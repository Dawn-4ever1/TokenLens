const exactFormat = new Intl.NumberFormat("en-US");

export function exact(value: number): string {
  return exactFormat.format(value);
}

/** K/M/B with about three significant digits; exact value belongs in a tooltip. */
export function compact(value: number): string {
  const abs = Math.abs(value);
  if (abs < 1e4) return exact(Math.round(value));
  const units: [number, string][] = [
    [1e3, "K"],
    [1e6, "M"],
    [1e9, "B"],
    [1e12, "T"],
  ];
  let index = abs >= 1e12 ? 3 : abs >= 1e9 ? 2 : abs >= 1e6 ? 1 : 0;
  for (;;) {
    const [divisor, suffix] = units[index]!;
    const scaled = abs / divisor;
    const digits = scaled >= 100 ? 0 : scaled >= 10 ? 1 : 2;
    const text = scaled.toFixed(digits);
    if (Number(text) >= 1000 && index < units.length - 1) {
      index += 1;
      continue;
    }
    const trimmed = text.includes(".") ? text.replace(/0+$/, "").replace(/\.$/, "") : text;
    return `${value < 0 ? "-" : ""}${trimmed}${suffix}`;
  }
}

export function percent(rate: number | null | undefined, digits = 1): string {
  if (rate === null || rate === undefined || !Number.isFinite(rate)) return "—";
  return `${(rate * 100).toFixed(digits)}%`;
}

export function share(part: number, whole: number): number | null {
  return whole > 0 ? part / whole : null;
}

export function usd(value: number | null | undefined): string {
  if (value === null || value === undefined) return "—";
  if (value > 0 && value < 0.01) return "<$0.01";
  if (value >= 1000) return `$${compact(value)}`;
  return `$${value.toFixed(2)}`;
}

export function credits(value: number | null | undefined): string {
  if (value === null || value === undefined) return "—";
  return value >= 1000 ? compact(value) : value.toFixed(value >= 100 ? 0 : 1);
}

export function toMs(value: string | number | null | undefined): number | null {
  if (value === null || value === undefined) return null;
  if (typeof value === "number") return value;
  const ms = Date.parse(value);
  return Number.isNaN(ms) ? null : ms;
}

export function relativeTime(value: string | number | null | undefined, now = Date.now()): string {
  const ms = toMs(value);
  if (ms === null) return "从未";
  const seconds = Math.max(0, Math.round((now - ms) / 1000));
  if (seconds < 60) return "刚刚";
  const minutes = Math.round(seconds / 60);
  if (minutes < 60) return `${minutes} 分钟前`;
  const hours = Math.round(minutes / 60);
  if (hours < 48) return `${hours} 小时前`;
  const days = Math.round(hours / 24);
  if (days < 60) return `${days} 天前`;
  return dateTime(ms, undefined, false);
}

const formatters = new Map<string, Intl.DateTimeFormat>();
function formatter(timeZone: string | undefined, withTime: boolean, withYear: boolean) {
  const key = `${timeZone}|${withTime}|${withYear}`;
  let found = formatters.get(key);
  if (!found) {
    found = new Intl.DateTimeFormat("zh-CN", {
      timeZone,
      year: withYear ? "numeric" : undefined,
      month: "2-digit",
      day: "2-digit",
      hour: withTime ? "2-digit" : undefined,
      minute: withTime ? "2-digit" : undefined,
      hourCycle: "h23",
    });
    formatters.set(key, found);
  }
  return found;
}

export function dateTime(
  value: string | number | null | undefined,
  timeZone?: string,
  withTime = true,
  withYear = false,
): string {
  const ms = toMs(value);
  if (ms === null) return "—";
  return formatter(timeZone, withTime, withYear).format(ms).replace(/\//g, "-");
}

export function duration(ms: number | null | undefined): string {
  if (ms === null || ms === undefined) return "—";
  if (ms < 1000) return `${Math.round(ms)} 毫秒`;
  const seconds = Math.round(ms / 1000);
  if (seconds < 60) return `${seconds} 秒`;
  const minutes = Math.floor(seconds / 60);
  if (minutes < 60) return seconds % 60 ? `${minutes} 分 ${seconds % 60} 秒` : `${minutes} 分钟`;
  const hours = Math.floor(minutes / 60);
  return minutes % 60 ? `${hours} 小时 ${minutes % 60} 分` : `${hours} 小时`;
}

export function shortId(id: string, length = 8): string {
  return id.length > length + 2 ? `${id.slice(0, length)}…` : id;
}

/** "YYYY-MM-DD" → "11月10日" (or with year). */
export function dayLabel(date: string, withYear = false): string {
  const [y, m, d] = date.split("-");
  if (!m || !d) return date;
  const core = `${Number(m)}月${Number(d)}日`;
  return withYear ? `${y}年${core}` : core;
}

export function bucketLabel(bucket: string, kind: "hour" | "day" | "week" | "month"): string {
  switch (kind) {
    case "hour":
      return `${dayLabel(bucket.slice(0, 10))} ${bucket.slice(11, 13)}时`;
    case "day":
      return dayLabel(bucket);
    case "week":
      return `${dayLabel(bucket)}起一周`;
    case "month": {
      const [y, m] = bucket.split("-");
      return `${y}年${Number(m)}月`;
    }
  }
}
