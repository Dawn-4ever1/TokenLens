/** Official marks (sources in `src/assets/brands/README.md`). */
export type BrandName =
  | "claude"
  | "openai"
  | "copilot"
  | "qoder"
  | "workbuddy"
  | "antigravity"
  | "deepseek"
  | "zcode"
  | "zai"
  | "openclaw"
  | "gemini"
  | "cursor"
  | "opencode"
  | "amp"
  | "kilo"
  | "roocode"
  | "factory"
  | "kimi"
  | "qwen"
  | "mistral"
  | "xai"
  | "meta"
  | "cohere";

/** A brand mark, or a generic mark for unknown agents, unknown models and "other". */
export type IdentityIconName = BrandName | "bot" | "cpu" | "circle";

export function isBrandIcon(icon: IdentityIconName): icon is BrandName {
  return icon !== "bot" && icon !== "cpu" && icon !== "circle";
}

// Fixed tool colours shared by every chart and legend (03 §4.2); other
// auto-discovered clients get a stable colour from the spare palette.
const CLIENTS: Record<string, { label: string; color: string }> = {
  claude: { label: "Claude Code", color: "#D97757" },
  codex: { label: "Codex", color: "#4F6BED" },
  "qoder-intl": { label: "Qoder", color: "#14A38B" },
  dsh: { label: "DSH", color: "#8B5CF6" },
  zcode: { label: "ZCode", color: "#2F9BD8" },
  openclaw: { label: "OpenClaw", color: "#D9468F" },
  gemini: { label: "Gemini CLI", color: "#C08A1E" },
  cursor: { label: "Cursor", color: "#5E6B7A" },
  opencode: { label: "OpenCode", color: "#4D7C0F" },
  amp: { label: "Amp", color: "#B45309" },
  kilocode: { label: "Kilo Code", color: "#0E7490" },
  roocode: { label: "Roo Code", color: "#7C3AED" },
  copilot: { label: "Copilot", color: "#1D4ED8" },
  droid: { label: "Droid", color: "#9A3412" },
  kimi: { label: "Kimi", color: "#334155" },
  qwen: { label: "Qwen Code", color: "#6D28D9" },
  workbuddy: { label: "WorkBuddy", color: "#65A30D" },
  antigravity: { label: "Antigravity", color: "#CA8A04" },
  "antigravity-cli": { label: "Antigravity CLI", color: "#A16207" },
  "antigravity-extension": { label: "Antigravity", color: "#EAB308" },
};

const SPARE = [
  "#A16207", "#0F766E", "#7C3AED", "#BE185D", "#4D7C0F", "#1D4ED8",
  "#9A3412", "#6D28D9", "#047857", "#B91C1C", "#0369A1", "#854D0E",
];

export const OTHER_KEY = "__other__";
export const NO_PROJECT_KEY = "__none__";
const OTHER_COLOR = "#9CA3AF";

const CLIENT_ICONS: Record<string, IdentityIconName> = {
  claude: "claude",
  codex: "openai",
  "qoder-intl": "qoder",
  dsh: "deepseek",
  zcode: "zcode",
  openclaw: "openclaw",
  gemini: "gemini",
  cursor: "cursor",
  opencode: "opencode",
  amp: "amp",
  kilocode: "kilo",
  roocode: "roocode",
  copilot: "copilot",
  droid: "factory",
  kimi: "kimi",
  qwen: "qwen",
  workbuddy: "workbuddy",
  antigravity: "antigravity",
  "antigravity-cli": "antigravity",
  "antigravity-extension": "antigravity",
};

function hash(text: string): number {
  let h = 2166136261;
  for (let i = 0; i < text.length; i++) {
    h ^= text.charCodeAt(i);
    h = Math.imul(h, 16777619);
  }
  return h >>> 0;
}

export function clientColor(id: string): string {
  if (id === OTHER_KEY) return OTHER_COLOR;
  return CLIENTS[id]?.color ?? SPARE[hash(id) % SPARE.length]!;
}

// Readable names for other agents tokscale can discover (no fixed colour or mark).
const NAMES: Record<string, string> = {
  cline: "Cline",
  trae: "Trae",
  kiro: "Kiro",
  augment: "Augment",
  codebuddy: "CodeBuddy",
  zed: "Zed",
  warp: "Warp",
  goose: "Goose",
  junie: "Junie",
  crush: "Crush",
  grok: "Grok CLI",
  kilo: "Kilo CLI",
  "devin-cli": "Devin CLI",
  "devin-desktop": "Devin",
  lmstudio: "LM Studio",
  cherrystudio: "Cherry Studio",
  codebuff: "Codebuff",
  hermes: "Hermes",
  pi: "Pi",
};

export function clientLabel(id: string, fallback?: string): string {
  if (id === OTHER_KEY) return "其他";
  return CLIENTS[id]?.label ?? NAMES[id] ?? fallback ?? id;
}

/**
 * Agents ordered by how widely they are used, most popular first. Editorial
 * judgement as of 2026-11 (public user base, downloads and GitHub activity),
 * not a live metric; agents missing here sort after these, by name. Only used
 * to order agents nobody on this dashboard has installed or used yet.
 */
export const CLIENT_POPULARITY: readonly string[] = [
  "claude", "codex", "cursor", "copilot", "gemini",
  "openclaw", "cline", "trae", "roocode", "kilocode", "opencode", "antigravity", "qoder-intl", "kiro", "qwen",
  "codebuddy", "workbuddy", "augment", "amp", "droid", "zed", "warp", "kimi", "kilo", "goose", "junie", "crush",
  "grok", "devin-desktop", "devin-cli", "antigravity-cli", "antigravity-extension", "lmstudio", "cherrystudio",
  "hermes", "pi", "codebuff", "zcode", "dsh",
];

const POPULARITY_RANK = new Map(CLIENT_POPULARITY.map((id, index) => [id, index]));

function byPopularity(a: string, b: string): number {
  const unranked = CLIENT_POPULARITY.length;
  const rank = (POPULARITY_RANK.get(a) ?? unranked) - (POPULARITY_RANK.get(b) ?? unranked);
  return rank || clientLabel(a).localeCompare(clientLabel(b), "en");
}

interface CoverageInput {
  clients: string[];
  devices: { clients: { client: string; detected: boolean }[] }[];
  usage: { byClient: { key: string; totalTokens: number }[] }[];
}

/**
 * Column order for the agent coverage table: agents installed or used on any
 * device come first (most tokens in the range first), everything else follows
 * by popularity.
 */
export function coverageColumns(data: CoverageInput): { used: string[]; unused: string[] } {
  const tokens = new Map<string, number>();
  for (const device of data.usage) {
    for (const slice of device.byClient) tokens.set(slice.key, (tokens.get(slice.key) ?? 0) + slice.totalTokens);
  }
  const detected = new Set(data.devices.flatMap((device) => device.clients.filter((item) => item.detected).map((item) => item.client)));
  const all = [...new Set([...data.clients, ...detected, ...tokens.keys()])];
  const isUsed = (id: string) => detected.has(id) || (tokens.get(id) ?? 0) > 0;
  const used = all.filter(isUsed).sort((a, b) => (tokens.get(b) ?? 0) - (tokens.get(a) ?? 0) || byPopularity(a, b));
  const unused = all.filter((id) => !isUsed(id)).sort(byPopularity);
  return { used, unused };
}

export function clientIcon(id: string): IdentityIconName {
  return CLIENT_ICONS[id] ?? "bot";
}

/** Stable colour for any series key (models, devices, projects). */
export function seriesColor(groupBy: string, key: string, index: number): string {
  if (key === OTHER_KEY) return OTHER_COLOR;
  if (groupBy === "client") return clientColor(key);
  const palette = ["var(--primary)", ...SPARE];
  return groupBy === "none" ? "var(--primary)" : palette[index % palette.length]!;
}

/** Display name for a model id; the raw id stays available for tooltips. */
export function modelLabel(id: string): string {
  if (id === OTHER_KEY) return "其他";
  let name = id.includes("/") ? id.slice(id.lastIndexOf("/") + 1) : id;
  name = name.replace(/[-@](20\d{6}|\d{8})$/, "");
  return name;
}

export interface ModelBrand {
  label: string;
  icon: IdentityIconName;
  color: string;
}

/** Resolve a model id to the vendor mark used beside model names. */
export function modelBrand(id: string): ModelBrand {
  const value = id.toLowerCase();
  if (value === OTHER_KEY) return { label: "其他", icon: "circle", color: "#9CA3AF" };
  if (value.includes("claude") || value.includes("anthropic")) return { label: "Anthropic", icon: "claude", color: "#D97757" };
  if (/(^|[\W_])(gpt|o1|o3|o4|openai|codex)([\W_]|$)/.test(value) || value.startsWith("gpt-")) {
    return { label: "OpenAI", icon: "openai", color: "#10A37F" };
  }
  if (value.includes("gemini") || value.includes("gemma") || value.includes("google")) return { label: "Google", icon: "gemini", color: "#4285F4" };
  if (value.includes("deepseek")) return { label: "DeepSeek", icon: "deepseek", color: "#4F6BED" };
  if (value.includes("qwen")) return { label: "Qwen", icon: "qwen", color: "#6D28D9" };
  if (value.includes("kimi") || value.includes("moonshot")) return { label: "Moonshot", icon: "kimi", color: "#334155" };
  if (value.includes("mistral") || value.includes("codestral")) return { label: "Mistral", icon: "mistral", color: "#F97316" };
  if (value.includes("grok") || value.includes("xai")) return { label: "xAI", icon: "xai", color: "#111827" };
  if (value.includes("glm") || value.includes("zhipu")) return { label: "智谱", icon: "zai", color: "#2563EB" };
  if (value.includes("llama") || value.includes("meta")) return { label: "Meta", icon: "meta", color: "#0668E1" };
  if (value.includes("cohere")) return { label: "Cohere", icon: "cohere", color: "#7C3AED" };
  if (value.includes("qoder")) return { label: "Qoder", icon: "qoder", color: "#14A38B" };
  return { label: "模型厂商未知", icon: "cpu", color: "#64748B" };
}

export function platformLabel(platform: string | null | undefined): string {
  switch (platform) {
    case "macos":
      return "Mac";
    case "windows":
      return "Windows";
    case "linux":
      return "Linux";
    default:
      return "未知平台";
  }
}

export function formFactorLabel(formFactor: string | null | undefined): string {
  switch (formFactor) {
    case "laptop":
      return "笔记本";
    case "desktop":
      return "台式机";
    default:
      return "";
  }
}

export function precisionLabel(precision: string): string {
  switch (precision) {
    case "measured":
      return "实测";
    case "estimated":
      return "估算";
    case "mixed":
      return "含估算";
    default:
      return "未核查";
  }
}

export const TOKEN_PARTS = [
  { key: "cacheReadTokens", label: "缓存读取", short: "Cache read", color: "var(--primary)" },
  { key: "cacheWriteTokens", label: "缓存写入", short: "Cache write", color: "color-mix(in oklch, var(--primary) 55%, var(--accent))" },
  { key: "inputTokens", label: "输入", short: "Input", color: "var(--accent)" },
  { key: "outputTokens", label: "输出", short: "Output", color: "color-mix(in oklch, var(--accent) 45%, var(--bad))" },
  { key: "reasoningTokens", label: "推理", short: "Reasoning", color: "color-mix(in oklch, var(--ink-3) 70%, var(--primary))" },
] as const;

export type TokenPartKey = (typeof TOKEN_PARTS)[number]["key"];
