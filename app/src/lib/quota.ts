import type { QuotaWindow } from "./types";

const HOUR_MINUTES = 60;
const DAY_MINUTES = 24 * HOUR_MINUTES;
const WEEK_MINUTES = 7 * DAY_MINUTES;

/** "5 小时", "本周", "本周 · Opus": the names the agents' own /usage screens use. */
export function windowLabel(window: Pick<QuotaWindow, "minutes" | "scope">): string {
  const { minutes, scope } = window;
  let base: string;
  if (minutes === null) base = "";
  else if (minutes === WEEK_MINUTES) base = "本周";
  else if (minutes === DAY_MINUTES) base = "今天";
  else if (minutes % DAY_MINUTES === 0) base = `${minutes / DAY_MINUTES} 天`;
  else if (minutes % HOUR_MINUTES === 0) base = `${minutes / HOUR_MINUTES} 小时`;
  else base = `${minutes} 分钟`;
  if (base && scope) return `${base} · ${scope}`;
  return base || scope || "其他额度";
}

/** True once the window renewed after the agent's last reading. */
export function hasReset(window: Pick<QuotaWindow, "resetsAt">, now = Date.now()): boolean {
  if (!window.resetsAt) return false;
  const at = Date.parse(window.resetsAt);
  return Number.isFinite(at) && at <= now;
}

/** "2 小时 25 分后重置"; `null` without a reset time or once it has passed. */
export function resetCountdown(resetsAt: string | null, now = Date.now()): string | null {
  if (!resetsAt) return null;
  const at = Date.parse(resetsAt);
  if (!Number.isFinite(at) || at <= now) return null;
  const minutes = Math.max(1, Math.ceil((at - now) / 60_000));
  if (minutes < HOUR_MINUTES) return `${minutes} 分钟后重置`;
  const days = Math.floor(minutes / DAY_MINUTES);
  const hours = Math.floor((minutes % DAY_MINUTES) / HOUR_MINUTES);
  if (days > 0) return hours > 0 ? `${days} 天 ${hours} 小时后重置` : `${days} 天后重置`;
  const rest = minutes % HOUR_MINUTES;
  return rest > 0 ? `${hours} 小时 ${rest} 分后重置` : `${hours} 小时后重置`;
}

export type QuotaTone = "normal" | "warn" | "bad";

/** Darker from 80%, red once used up. */
export function quotaTone(usedPercent: number): QuotaTone {
  if (usedPercent >= 100) return "bad";
  if (usedPercent >= 80) return "warn";
  return "normal";
}

/** "已用 34%"; whole numbers, as the agents show them. */
export function usedText(usedPercent: number): string {
  if (usedPercent >= 100) return "已用完";
  return `已用 ${Math.round(Math.min(100, Math.max(0, usedPercent)))}%`;
}
