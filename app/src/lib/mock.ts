// Browser-only development backend with synthetic data. Never loaded in the
// Tauri app (api.ts only imports it when not running inside Tauri, in dev).
import type {
  AppStatus, Breakdown, Caveat, DeviceStatus, Devices, FilterOptions, Health, Heatmap,
  Measures, Point, Precision, SessionDetail, SessionPage, SessionSummary, SessionTurn,
  Slice, Subagent, Timeseries, UpdateStatus,
} from "./types";
import { ApiError } from "./types";
import { addDays, todayIn } from "./range";

const TZ = "Asia/Shanghai";
const OFFSET_MS = 8 * 3_600_000;
const HOUR = 3_600_000;
const DAY = 24 * HOUR;

interface Ev {
  ts: number;
  device: string;
  client: string;
  model: string;
  session: string;
  root: string;
  sub: boolean;
  turnStart: boolean;
  input: number;
  output: number;
  cr: number;
  cw: number;
  reasoning: number;
  cost: number | null;
  credits: number | null;
  precision: Precision;
  project?: string;
}

function rng(seed: number) {
  return () => {
    seed = (seed + 0x6d2b79f5) | 0;
    let t = Math.imul(seed ^ (seed >>> 15), 1 | seed);
    t = (t + Math.imul(t ^ (t >>> 7), 61 | t)) ^ t;
    return ((t ^ (t >>> 14)) >>> 0) / 4294967296;
  };
}

const DEVICES = [
  { id: "dev_mock_mbp", name: "MacBook Pro", platform: "macos", form: "laptop", weight: 0.45, seen: 2 * 60_000 },
  { id: "dev_mock_mini", name: "Mac mini", platform: "macos", form: "desktop", weight: 0.3, seen: 7 * 60_000 },
  { id: "dev_mock_win", name: "Windows 台式机", platform: "windows", form: "desktop", weight: 0.2, seen: 5 * HOUR },
  { id: "dev_mock_old", name: "旧笔记本", platform: "windows", form: "laptop", weight: 0.05, seen: 5 * DAY },
];

const CLIENTS: { id: string; weight: number; models: string[]; priced: boolean; precision: Precision }[] = [
  { id: "claude", weight: 0.38, models: ["claude-sonnet-4-5-20250929", "claude-opus-4-1-20250805", "claude-haiku-4-5-20251001"], priced: true, precision: "measured" },
  { id: "codex", weight: 0.24, models: ["gpt-5-codex", "gpt-5"], priced: true, precision: "measured" },
  { id: "qoder-intl", weight: 0.16, models: ["qoder-auto", "qoder-performance"], priced: false, precision: "estimated" },
  { id: "dsh", weight: 0.09, models: ["deepseek-v3.2"], priced: false, precision: "measured" },
  { id: "zcode", weight: 0.08, models: ["glm-4.6"], priced: false, precision: "measured" },
  { id: "openclaw", weight: 0.05, models: ["kimi-k2-0905"], priced: false, precision: "measured" },
];

const PROJECTS = ["prj_a1f3", "prj_7c2e", "prj_d940", undefined, undefined];

function pick<T extends { weight: number }>(items: T[], r: number): T {
  let acc = 0;
  for (const item of items) {
    acc += item.weight;
    if (r <= acc) return item;
  }
  return items[items.length - 1]!;
}

let cache: { events: Ev[]; now: number } | null = null;

function rawEvents(): Ev[] {
  const now = Date.now();
  if (cache && now - cache.now < 60_000) return cache.events;
  const rand = rng(20261110);
  const out: Ev[] = [];
  const today = Math.floor((now + OFFSET_MS) / DAY) * DAY - OFFSET_MS;
  let n = 0;
  for (let day = 150; day >= 0; day--) {
    const start = today - day * DAY;
    const weekday = new Date(start + OFFSET_MS).getUTCDay();
    const busy = weekday === 0 || weekday === 6 ? 0.45 : 1;
    const growth = 0.55 + (150 - day) / 220;
    const spike = day === 23 || day === 61 ? 3 : 1;
    const sessions = Math.round((2 + rand() * 4) * busy * growth * spike);
    for (let s = 0; s < sessions; s++) {
      const device = pick(DEVICES, rand());
      const client = pick(CLIENTS, rand());
      const hour = [10, 11, 14, 15, 16, 17, 20, 21, 22, 23][Math.floor(rand() * 10)]!;
      let ts = start + hour * HOUR + Math.floor(rand() * HOUR);
      if (ts > now - 5 * 60_000) continue;
      if (device.id === "dev_mock_old" && ts > now - 5 * DAY) continue;
      if (device.id === "dev_mock_win" && ts > now - 5 * HOUR) continue;
      const root = `${client.id === "codex" ? "rollout" : "sess"}-${(n++).toString(36).padStart(6, "0")}`;
      const project = PROJECTS[Math.floor(rand() * PROJECTS.length)];
      const model = client.models[Math.floor(rand() * client.models.length)]!;
      const turns = 1 + Math.floor(rand() * rand() * 22);
      const streams: { session: string; sub: boolean }[] = [{ session: root, sub: false }];
      if (client.id === "claude" && rand() < 0.3) {
        for (let k = 0; k < 1 + Math.floor(rand() * 3); k++) streams.push({ session: `${root}-agent-${k}`, sub: true });
      }
      for (let t = 0; t < turns; t++) {
        const stream = t > 1 && rand() < 0.25 ? streams[Math.floor(rand() * streams.length)]! : streams[0]!;
        const messages = 1 + Math.floor(rand() * 5);
        for (let m = 0; m < messages; m++) {
          ts += 4_000 + Math.floor(rand() * 40_000);
          if (ts > now) break;
          const cr = Math.floor((15_000 + rand() * 120_000) * (1 + t / 12));
          const cw = rand() < 0.4 ? Math.floor(rand() * 9_000) : 0;
          const input = Math.floor(80 + rand() * 2_500);
          const output = Math.floor(150 + rand() * 3_200);
          const reasoning = client.id === "codex" ? Math.floor(rand() * 1_800) : 0;
          const tokens = input + output + cr + cw + reasoning;
          out.push({
            ts, device: device.id, client: client.id, model, session: stream.session, root, sub: stream.sub,
            turnStart: m === 0, input, output, cr, cw, reasoning,
            cost: client.priced ? (input * 3 + output * 15 + cr * 0.3 + cw * 3.75 + reasoning * 15) / 1e6 : null,
            credits: client.precision === "estimated" ? Math.round(tokens / 9_000 * 10) / 10 : null,
            precision: client.precision,
            project,
          });
        }
        ts += 30_000 + Math.floor(rand() * 8 * 60_000);
      }
    }
  }
  out.sort((a, b) => a.ts - b.ts);
  cache = { events: out, now };
  return out;
}

/** The local store contains this computer only; server views share the tenant's devices. */
function events(): Ev[] {
  const all = rawEvents();
  return state.mode === "local" ? all.filter((event) => event.device === "dev_mock_mbp") : all;
}

const localDate = (ts: number) => new Date(ts + OFFSET_MS).toISOString().slice(0, 10);
const iso = (ts: number) => new Date(ts).toISOString().replace(/\.\d{3}Z$/, "Z");
const total = (e: Ev) => e.input + e.output + e.cr + e.cw + e.reasoning;
const list = (raw?: string) => (raw ? raw.split(",").filter(Boolean) : []);

function precisionOf(items: Ev[]): Precision {
  const set = new Set(items.map((e) => e.precision));
  if (set.size === 0) return "measured";
  if (set.size === 1) return [...set][0]!;
  return "mixed";
}

function breakdown(items: Ev[]): Breakdown {
  const b = { totalTokens: 0, inputTokens: 0, outputTokens: 0, cacheReadTokens: 0, cacheWriteTokens: 0, reasoningTokens: 0 };
  for (const e of items) {
    b.inputTokens += e.input;
    b.outputTokens += e.output;
    b.cacheReadTokens += e.cr;
    b.cacheWriteTokens += e.cw;
    b.reasoningTokens += e.reasoning;
  }
  b.totalTokens = b.inputTokens + b.outputTokens + b.cacheReadTokens + b.cacheWriteTokens + b.reasoningTokens;
  return b;
}

function measures(items: Ev[]): Measures {
  const b = breakdown(items);
  const priced = items.filter((e) => e.cost !== null);
  const credited = items.filter((e) => e.credits !== null);
  const denominator = b.inputTokens + b.cacheReadTokens + b.cacheWriteTokens;
  return {
    ...b,
    costUsd: priced.length ? priced.reduce((sum, e) => sum + e.cost!, 0) : null,
    unpricedTokens: items.filter((e) => e.cost === null && e.credits === null).reduce((sum, e) => sum + total(e), 0),
    credits: credited.length ? credited.reduce((sum, e) => sum + e.credits!, 0) : null,
    messageCount: items.length,
    cacheHitRate: denominator ? b.cacheReadTokens / denominator : null,
    precision: precisionOf(items),
  };
}

function point(items: Ev[]): Point {
  const m = measures(items);
  return { ...breakdown(items), costUsd: m.costUsd ?? 0, unpricedTokens: m.unpricedTokens, messageCount: m.messageCount };
}

type Q = Record<string, string>;

function span(all: Ev[], q: Q) {
  const today = todayIn(TZ);
  const from = q.from ?? (all.length ? localDate(all[0]!.ts) : today);
  const lastDay = all.length ? localDate(all[all.length - 1]!.ts) : today;
  const to = q.to ?? (lastDay > today ? lastDay : today);
  if (from > to) throw new ApiError("bad_request", 400);
  return { from, to, timeZone: TZ };
}

function filtered(q: Q, ignoreRange = false) {
  const devices = list(q.devices);
  const clients = list(q.clients);
  const models = list(q.models);
  const projects = list(q.projects);
  const base = events().filter(
    (e) =>
      (!devices.length || devices.includes(e.device)) &&
      (!clients.length || clients.includes(e.client)) &&
      (!models.length || models.includes(e.model)) &&
      (!projects.length || projects.includes(e.project ?? "__none__")),
  );
  const range = span(base, q);
  const items = ignoreRange ? base : base.filter((e) => {
    const d = localDate(e.ts);
    return d >= range.from && d <= range.to;
  });
  return { items, range };
}

function groupKey(e: Ev, by: string): string {
  switch (by) {
    case "client": return e.client;
    case "model": return e.model;
    case "device": return e.device;
    case "project": return e.project ?? "__none__";
    default: return "total";
  }
}

function labelOf(key: string, by: string): string {
  if (by === "device") return DEVICES.find((d) => d.id === key)?.name ?? key;
  if (by === "project" && key === "__none__") return "未标记项目";
  if (key === "__other__") return "其他";
  return key;
}

function slices(items: Ev[], by: string): Slice[] {
  const groups = new Map<string, Ev[]>();
  for (const e of items) {
    const key = groupKey(e, by);
    let group = groups.get(key);
    if (!group) groups.set(key, (group = []));
    group.push(e);
  }
  return [...groups.entries()]
    .map(([key, group]) => ({ key, label: labelOf(key, by), ...measures(group) }))
    .sort((a, b) => b.totalTokens - a.totalTokens);
}

function caveats(items: Ev[], devices: DeviceStatus[] = []): Caveat[] {
  const out: Caveat[] = [];
  const m = measures(items);
  if (items.some((e) => e.precision === "estimated")) {
    out.push({ code: "estimated_qoder", severity: "info", message: "Qoder 的 token 由 credits × 校准汇率折算，不是官方计数" });
  }
  if (items.some((e) => e.precision === "unknown")) {
    out.push({ code: "unknown_precision", severity: "info", message: "部分 token 来自未核查的工具，可能含按字数估算" });
  }
  if (m.costUsd !== null) {
    out.push({ code: "estimated_cost", severity: "info", message: `成本按 ${todayIn(TZ)} 的公开价格估算，是 API 等价金额，不是账单` });
  }
  if (m.unpricedTokens > 0) {
    out.push({ code: "unpriced_tokens", severity: "warning", message: `${m.unpricedTokens.toLocaleString("en-US")} tokens 没有价格，成本偏低` });
  }
  for (const d of devices) {
    if (d.connection === "long_offline") {
      out.push({ code: "device_long_offline", severity: "warning", deviceId: d.deviceId, message: `${d.displayName} 已超过 3 天没有上线，请确认它的采集端是否还在自启动` });
    }
  }
  return out;
}

function bucketOf(ts: number, bucket: string): string {
  const local = new Date(ts + OFFSET_MS);
  const date = local.toISOString().slice(0, 10);
  switch (bucket) {
    case "hour": return `${date}T${local.toISOString().slice(11, 13)}:00`;
    case "week": {
      const weekday = (local.getUTCDay() + 6) % 7;
      return addDays(date, -weekday);
    }
    case "month": return date.slice(0, 7);
    default: return date;
  }
}

function bucketList(from: string, to: string, bucket: string): string[] {
  const out: string[] = [];
  if (bucket === "hour") {
    for (let d = from; d <= to; d = addDays(d, 1)) for (let h = 0; h < 24; h++) out.push(`${d}T${String(h).padStart(2, "0")}:00`);
    return out;
  }
  const seen = new Set<string>();
  for (let d = from; d <= to; d = addDays(d, 1)) {
    const key = bucketOf(Date.parse(`${d}T12:00:00Z`) - OFFSET_MS + 0, bucket);
    if (!seen.has(key)) {
      seen.add(key);
      out.push(key);
    }
  }
  return out;
}

function timeseries(q: Q): Timeseries {
  const { items, range } = filtered(q);
  const bucket = q.bucket ?? "day";
  const groupBy = q.groupBy ?? "none";
  const limit = Number(q.limit ?? 8);
  const buckets = bucketList(range.from, range.to, bucket);
  if (bucket === "hour" && buckets.length > 2000) throw new ApiError("bad_request", 400);
  const ranked = slices(items, groupBy);
  const keep = new Set(ranked.slice(0, limit).map((s) => s.key));
  const groups = new Map<string, Ev[]>();
  for (const e of items) {
    const raw = groupKey(e, groupBy);
    const key = keep.has(raw) ? raw : "__other__";
    let group = groups.get(key);
    if (!group) groups.set(key, (group = []));
    group.push(e);
  }
  const order = [...ranked.slice(0, limit).map((s) => s.key), ...(groups.has("__other__") ? ["__other__"] : [])];
  return {
    range,
    bucket: bucket as Timeseries["bucket"],
    groupBy: groupBy as Timeseries["groupBy"],
    buckets,
    series: order.map((key) => {
      const group = groups.get(key) ?? [];
      const byBucket = new Map<string, Ev[]>();
      for (const e of group) {
        const b = bucketOf(e.ts, bucket);
        let list = byBucket.get(b);
        if (!list) byBucket.set(b, (list = []));
        list.push(e);
      }
      return {
        key,
        label: groupBy === "none" ? "全部" : labelOf(key, groupBy),
        ...measures(group),
        points: buckets.map((b) => point(byBucket.get(b) ?? [])),
      };
    }),
    generatedAt: iso(Date.now()),
  };
}

function heatmap(q: Q): Heatmap {
  const { items, range } = filtered(q);
  if (q.kind === "hour_weekday") {
    const cells = Array.from({ length: 168 }, (_, i) => ({ weekday: Math.floor(i / 24), hour: i % 24, totalTokens: 0, messageCount: 0 }));
    for (const e of items) {
      const local = new Date(e.ts + OFFSET_MS);
      const cell = cells[((local.getUTCDay() + 6) % 7) * 24 + local.getUTCHours()]!;
      cell.totalTokens += total(e);
      cell.messageCount += 1;
    }
    return { range, kind: "hour_weekday", cells, maxTotalTokens: Math.max(0, ...cells.map((c) => c.totalTokens)), generatedAt: iso(Date.now()) };
  }
  const byDay = new Map<string, Ev[]>();
  for (const e of items) {
    const d = localDate(e.ts);
    let list = byDay.get(d);
    if (!list) byDay.set(d, (list = []));
    list.push(e);
  }
  const days = [];
  for (let d = range.from; d <= range.to; d = addDays(d, 1)) {
    const p = point(byDay.get(d) ?? []);
    days.push({ date: d, totalTokens: p.totalTokens, costUsd: p.costUsd, messageCount: p.messageCount });
  }
  return { range, kind: "calendar", days, maxTotalTokens: Math.max(0, ...days.map((d) => d.totalTokens)), generatedAt: iso(Date.now()) };
}

const SEP = "\u001f";
const encodeKey = (device: string, client: string, root: string) =>
  btoa(unescape(encodeURIComponent([device, client, root].join(SEP)))).replace(/\+/g, "-").replace(/\//g, "_").replace(/=+$/, "");

function sessionGroups(items: Ev[]) {
  const groups = new Map<string, Ev[]>();
  for (const e of items) {
    const key = encodeKey(e.device, e.client, e.root);
    let group = groups.get(key);
    if (!group) groups.set(key, (group = []));
    group.push(e);
  }
  return groups;
}

function summarize(key: string, group: Ev[]): SessionSummary {
  const first = group[0]!;
  const parent = group.filter((e) => !e.sub);
  return {
    sessionKey: key,
    sessionId: first.root,
    deviceId: first.device,
    deviceName: DEVICES.find((d) => d.id === first.device)?.name ?? first.device,
    client: first.client,
    models: [...new Set(group.map((e) => e.model))],
    projectLabel: first.project,
    startedAt: iso(group[0]!.ts),
    lastUsedAt: iso(group[group.length - 1]!.ts),
    turnCount: Math.max(1, parent.filter((e) => e.turnStart).length),
    subagentCount: new Set(group.filter((e) => e.sub).map((e) => e.session)).size,
    ...measures(group),
  };
}

function sessions(q: Q): SessionPage {
  const { items, range } = filtered(q);
  let rows = [...sessionGroups(items).entries()].map(([key, group]) => summarize(key, group));
  const needle = q.q?.trim().toLowerCase();
  if (needle) {
    rows = rows.filter((s) =>
      s.sessionId.toLowerCase().includes(needle) || s.models.some((m) => m.toLowerCase().includes(needle)) || (s.projectLabel ?? "").includes(needle),
    );
  }
  const sort = q.sort ?? "recent";
  rows.sort((a, b) =>
    sort === "tokens" ? b.totalTokens - a.totalTokens
      : sort === "cost" ? (b.costUsd ?? -1) - (a.costUsd ?? -1)
        : sort === "turns" ? b.turnCount - a.turnCount
          : b.lastUsedAt.localeCompare(a.lastUsedAt),
  );
  const offset = Number(q.cursor ?? 0);
  const limit = Number(q.limit ?? 50);
  const page = rows.slice(offset, offset + limit);
  return {
    range,
    sessions: page,
    total: rows.length,
    nextCursor: offset + limit < rows.length ? String(offset + limit) : undefined,
    generatedAt: iso(Date.now()),
  };
}

function sessionDetail(key: string): SessionDetail {
  const group = sessionGroups(events()).get(key);
  if (!group) throw new ApiError("not_found", 404);
  const turns: SessionTurn[] = [];
  const streams = new Map<string, Ev[][]>();
  for (const e of group) {
    let stream = streams.get(e.session);
    if (!stream) streams.set(e.session, (stream = []));
    if (e.turnStart || stream.length === 0) stream.push([]);
    stream[stream.length - 1]!.push(e);
  }
  const all: { events: Ev[]; sub: boolean; session: string }[] = [];
  for (const [session, list] of streams) for (const t of list) all.push({ events: t, sub: t[0]!.sub, session });
  all.sort((a, b) => a.events[0]!.ts - b.events[0]!.ts);
  all.forEach((t, index) => {
    const first = t.events[0]!.ts;
    const last = t.events[t.events.length - 1]!.ts;
    turns.push({
      index: index + 1,
      startedAt: iso(first),
      endedAt: iso(last),
      durationMs: last - first || undefined,
      models: [...new Set(t.events.map((e) => e.model))],
      isSubagent: t.sub,
      subagentSessionId: t.sub ? t.session : undefined,
      ...measures(t.events),
    });
  });
  const subagents: Subagent[] = [...streams.entries()]
    .filter(([, list]) => list[0]![0]!.sub)
    .map(([session, list]) => {
      const flat = list.flat();
      return {
        sessionId: session,
        startedAt: iso(flat[0]!.ts),
        lastUsedAt: iso(flat[flat.length - 1]!.ts),
        models: [...new Set(flat.map((e) => e.model))],
        turnCount: list.length,
        ...measures(flat),
      };
    });
  return { session: summarize(key, group), turns, subagents, truncated: false, generatedAt: iso(Date.now()) };
}

const renamed = new Map<string, string>();
const revoked = new Set<string>();

function deviceStatuses(): DeviceStatus[] {
  const all = events();
  const now = Date.now();
  // Like the real collector, every agent tokscale knows is reported, found or not.
  const clients = [
    "amp", "antigravity", "augment", "claude", "cline", "codebuddy", "codex", "copilot", "crush", "cursor", "droid",
    "dsh", "gemini", "goose", "junie", "kilocode", "kimi", "kiro", "openclaw", "opencode", "qoder-intl", "qwen",
    "roocode", "trae", "warp", "workbuddy", "zcode", "zed",
  ];
  return DEVICES.filter((d) => state.mode === "local" ? d.id === "dev_mock_mbp" : !revoked.has(d.id)).map((d) => {
    const mine = all.filter((e) => e.device === d.id);
    const seen = now - d.seen;
    const connection = d.seen <= 15 * 60_000 ? "online" : d.seen <= 3 * DAY ? "offline" : "long_offline";
    return {
      deviceId: d.id,
      displayName: renamed.get(d.id) ?? d.name,
      platform: d.platform,
      formFactor: d.form,
      osName: d.platform === "macos" ? "macOS" : "Windows",
      osVersion: d.platform === "macos" ? "15.6" : "11 23H2",
      createdAt: iso(now - 160 * DAY),
      lastSeenAt: iso(seen),
      lastUploadAt: mine.length ? iso(Math.max(mine[mine.length - 1]!.ts, seen - 60_000)) : null,
      collectorVersion: d.id === "dev_mock_old" ? "0.0.9" : "0.1.0",
      tokscaleRev: "e9cf1041af784bdaa4e217978f89f2168e731b69",
      parserGeneration: "tokscale-e9cf1041.map-1.qoder-1.key-1",
      backfillComplete: d.id !== "dev_mock_win",
      lastScanAt: iso(seen),
      lastScanDurationMs: 820,
      outboxBacklog: d.id === "dev_mock_win" ? 1240 : 0,
      connection,
      outdated: d.id === "dev_mock_old",
      isCurrent: d.id === "dev_mock_mbp",
      clients: clients.map((client) => {
        const used = mine.filter((e) => e.client === client);
        const detected = used.length > 0 || (client === "gemini" && d.platform === "macos");
        return {
          client,
          detected,
          lastEventAt: used.length ? iso(used[used.length - 1]!.ts) : undefined,
          lastScanEvents: detected ? used.length : undefined,
          errorCode: d.id === "dev_mock_win" && client === "zcode" ? "parse-failed" : undefined,
          updatedAt: iso(seen),
        };
      }),
    } satisfies DeviceStatus;
  });
}

function devices(q: Q): Devices {
  const { items, range } = filtered(q);
  const statuses = deviceStatuses();
  const deviceCaveats = caveats([], statuses).filter((c) => c.deviceId);
  for (const d of statuses) {
    if (d.outdated) deviceCaveats.push({ code: "collector_outdated", severity: "warning", deviceId: d.deviceId, message: `${d.displayName} 的采集端版本过旧，请更新应用` });
    if (!d.backfillComplete) deviceCaveats.push({ code: "backfill_incomplete", severity: "info", deviceId: d.deviceId, message: `${d.displayName} 的历史数据仍在上传` });
    for (const c of d.clients) if (c.errorCode) deviceCaveats.push({ code: "client_error", severity: "error", deviceId: d.deviceId, client: c.client, message: `${d.displayName} 读取 ${c.client} 时出错（${c.errorCode}）` });
  }
  return {
    range,
    devices: statuses,
    usage: statuses.map((d) => {
      const mine = items.filter((e) => e.device === d.deviceId);
      return { deviceId: d.deviceId, ...measures(mine), byClient: slices(mine, "client") };
    }),
    clients: [...new Set(statuses.flatMap((d) => d.clients.map((c) => c.client)))].sort(),
    caveats: deviceCaveats,
    thresholds: { onlineMs: 15 * 60_000, longOfflineMs: 3 * DAY },
    minCollectorVersion: "0.1.0",
    generatedAt: iso(Date.now()),
  };
}

function filters(): FilterOptions {
  const all = events();
  const options = (by: string) => slices(all, by).map((s) => ({ key: s.key, label: s.label, totalTokens: s.totalTokens }));
  return {
    devices: options("device"),
    clients: options("client"),
    models: options("model"),
    projects: options("project"),
    firstEventDate: all.length ? localDate(all[0]!.ts) : undefined,
    timeZone: TZ,
  };
}

function health(): Health {
  const all = events();
  return {
    status: "ok", serverVersion: "0.1.0", schemaVersion: 4,
    eventCount: all.length, deviceCount: deviceStatuses().length, lastIngestAt: iso(Date.now() - 90_000),
    timeZone: TZ, minCollectorVersion: "0.1.0", minAppVersion: "0.1.0", generatedAt: iso(Date.now()),
  };
}

function dashboard(path: string, q: Q): unknown {
  if (path.startsWith("sessions/")) return sessionDetail(path.slice("sessions/".length));
  switch (path) {
    case "summary": {
      const { items, range } = filtered(q);
      const sessionsCount = sessionGroups(items).size;
      return {
        range,
        ...measures(items),
        sessionCount: sessionsCount,
        turnCount: items.filter((e) => e.turnStart && !e.sub).length,
        activeDays: new Set(items.map((e) => localDate(e.ts))).size,
        byClient: slices(items, "client"),
        byModel: slices(items, "model"),
        byDevice: slices(items, "device"),
        byProject: slices(items, "project"),
        caveats: caveats(items),
        pricing: { asOf: todayIn(TZ), version: "mock" },
        generatedAt: iso(Date.now()),
      };
    }
    case "timeseries": return timeseries(q);
    case "heatmap": return heatmap(q);
    case "sessions": return sessions(q);
    case "devices": return devices(q);
    case "filters": return filters();
    case "health": return health();
    default: throw new ApiError("not_found", 404);
  }
}

const params = typeof location !== "undefined" ? new URLSearchParams(location.search) : new URLSearchParams();
/** ?update (or =available), =latest, =downloading, =error, =install-error / =async-install-error. */
export function mockUpdateStatus(scenario: string | null): UpdateStatus {
  const phase: UpdateStatus["phase"] = scenario === null ? "idle"
    : scenario === "latest" ? "latest"
      : scenario === "downloading" ? "downloading"
        : scenario === "error" ? "error" : "available";
  const available = ["available", "downloading"].includes(phase);
  return {
    autoCheck: true, phase,
    version: available ? "0.4.1" : null,
    notes: available ? "TokenLens 0.4.1\n\n改善自动更新体验。\n配对、数据和设置保持不变。" : null,
    downloadedBytes: phase === "downloading" ? 4_200_000 : 0,
    totalBytes: phase === "downloading" ? 10_000_000 : null,
    error: phase === "error" ? "连不上 GitHub，稍后会再试。" : null,
    checkedAt: phase === "idle" ? null : Date.now(),
  };
}
const updateState = mockUpdateStatus(params.get("update"));
const startsLocal = !params.has("server") && !params.has("offline");
const state: AppStatus = {
  paired: !params.has("local-error"),
  mode: startsLocal ? "local" : "server",
  deviceId: "dev_mock_mbp",
  server: startsLocal ? null : "https://dash.example.com",
  cloudServer: "https://tokenlens.online",
  accountEmail: params.has("server") || params.has("signed") ? "mock@example.com" : null,
  accountSpace: params.has("server") || params.has("signed") ? "我的空间" : null,
  appVersion: "0.4.0",
  collector: {
    running: true, paused: false, lastScanAt: Date.now() - 40_000, lastUploadAt: Date.now() - 95_000,
    lastHeartbeatAt: Date.now() - 120_000, backlog: 0, uploadState: "ok", backfillComplete: true, lastError: null,
  },
  autostart: true,
  platform: "macos",
};

let pollCount = 0;

const delay = (ms: number) => new Promise((resolve) => setTimeout(resolve, ms));

export async function command(name: string, args: Record<string, unknown>): Promise<unknown> {
  await delay(params.has("slow") ? 1500 : 120);
  if (name === "dashboard_get" && state.mode === "server" && (params.has("offline") || (params.has("devices-error") && args.path === "devices"))) throw new ApiError("network");
  switch (name) {
    case "dashboard_get":
      if (!state.paired) throw new ApiError("not_paired");
      return dashboard(String(args.path), (args.query ?? {}) as Q);
    case "app_status":
      return { ...state, collector: { ...state.collector } };
    case "update_status":
      return { ...updateState };
    case "set_auto_check_updates":
      updateState.autoCheck = Boolean(args.enabled);
      return { ...updateState };
    case "check_update": {
      if (["downloading", "installing"].includes(updateState.phase)) return { ...updateState };
      updateState.phase = "checking";
      await delay(350);
      const next = mockUpdateStatus(params.has("update") ? params.get("update") : "latest");
      Object.assign(updateState, next, { autoCheck: updateState.autoCheck });
      return { ...updateState };
    }
    case "install_update": {
      if (!updateState.version || ["downloading", "installing"].includes(updateState.phase)) return { ...updateState };
      Object.assign(updateState, { phase: "downloading", downloadedBytes: 4_200_000, totalBytes: 10_000_000, error: null });
      if (params.get("update") === "async-install-error") {
        // Mirrors the real command: it returns immediately; the Rust task reports
        // a later failure through update_status / update-changed.
        setTimeout(() => Object.assign(updateState, { phase: "error", error: "下载没有完成，请稍后重试。" }), 1600);
        return { ...updateState };
      }
      await delay(1600);
      if (params.get("update") === "install-error") {
        Object.assign(updateState, { phase: "error", error: "下载没有完成，请稍后重试。" });
      } else {
        Object.assign(updateState, { phase: "installing", downloadedBytes: 10_000_000 });
      }
      return { ...updateState };
    }
    case "pair_device":
      if (params.has("pairfail")) throw new ApiError("network");
      if (String(args.code).replace(/[^A-Z0-9]/gi, "").length < 8) throw new ApiError("unauthorized", 401);
      state.paired = true;
      state.mode = "server";
      state.server = String(args.server);
      state.autostart = Boolean(args.autostart);
      return { ...state, collector: { ...state.collector } };
    case "use_local_mode":
      state.paired = true;
      state.mode = "local";
      state.server = null;
      state.autostart = Boolean(args.autostart);
      return { ...state, collector: { ...state.collector } };
    case "unpair_device":
      state.paired = true;
      state.mode = "local";
      state.server = null;
      return { ...state, collector: { ...state.collector } };
    case "set_dashboard_mode":
      if (!["local", "server"].includes(String(args.mode)) || (args.mode === "server" && !state.server)) throw new ApiError("bad_request", 400);
      state.mode = args.mode as "local" | "server";
      return { ...state, collector: { ...state.collector } };
    case "set_paused":
      state.collector.paused = Boolean(args.paused);
      state.collector.running = !state.collector.paused;
      return { ...state, collector: { ...state.collector } };
    case "sync_now":
      state.collector.lastScanAt = Date.now();
      return { ...state, collector: { ...state.collector } };
    case "set_autostart":
      state.autostart = Boolean(args.enabled);
      return { ...state, collector: { ...state.collector } };
    case "start_github_login":
      if (params.has("githubfail")) throw new ApiError("unavailable");
      return { userCode: "ABCD-1234", verificationUri: "https://github.com/login/device" };
    case "poll_github_login": {
      pollCount += 1;
      if (pollCount % 2 === 1) return { status: "pending" };
      state.accountEmail = "octo@example.com";
      state.accountSpace = "我的空间";
      state.server = "https://tokenlens.online";
      state.mode = "server";
      state.paired = true;
      return { status: "ok", app: { ...state, collector: { ...state.collector } } };
    }
    case "open_github_device_page":
      return null;
    case "request_login_code":
      if (!/^[^\s@]+@[^\s@]+\.[^\s@]+$/.test(String(args.email))) throw new ApiError("bad_email");
      return null;
    case "login_account": {
      if (params.has("pairfail") || params.has("offline")) throw new ApiError("network");
      state.accountEmail = String(args.email);
      state.accountSpace = "我的空间";
      state.server = String(args.server);
      state.mode = "server";
      state.paired = true;
      return { ...state, collector: { ...state.collector } };
    }
    case "refresh_account":
      return state.accountEmail;
    case "account_pair_code":
      if (!state.accountEmail) throw new ApiError("not_signed_in");
      return { code: args.rotate ? "NEWW-CODE-0002" : "K7QM-4XPR", replaceable: true };
    case "logout_account":
      state.accountEmail = null;
      state.accountSpace = null;
      return { ...state, collector: { ...state.collector } };
    case "create_pair_code":
      if (!state.server) throw new ApiError("bad_request", 400);
      return { code: "K7QM-4XPR", expiresAt: iso(Date.now() + 10 * 60_000) };
    case "rename_device":
      renamed.set(String(args.deviceId), String(args.displayName));
      return { deviceId: args.deviceId, displayName: args.displayName };
    case "rotate_credentials":
      return { ...state, collector: { ...state.collector } };
    case "revoke_device":
      if (args.deviceId === state.deviceId) {
        state.paired = true;
        state.mode = "local";
        state.server = null;
      }
      revoked.add(String(args.deviceId));
      return { ...state, collector: { ...state.collector } };
    default:
      throw new ApiError("not_found");
  }
}
