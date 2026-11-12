// Mirrors server/src/dashboard.rs (camelCase JSON); see docs/04 §8.

export type Precision = "measured" | "estimated" | "mixed" | "unknown";

export interface RangeOut {
  from: string;
  to: string;
  timeZone: string;
}

export interface Breakdown {
  totalTokens: number;
  inputTokens: number;
  outputTokens: number;
  cacheReadTokens: number;
  cacheWriteTokens: number;
  reasoningTokens: number;
}

export interface Measures extends Breakdown {
  costUsd: number | null;
  unpricedTokens: number;
  credits: number | null;
  messageCount: number;
  cacheHitRate: number | null;
  precision: Precision;
}

export interface Point extends Breakdown {
  costUsd: number;
  unpricedTokens: number;
  messageCount: number;
}

export interface Slice extends Measures {
  key: string;
  label: string;
}

export type CaveatCode =
  | "estimated_qoder"
  | "unknown_precision"
  | "estimated_cost"
  | "partial_cost"
  | "unpriced_tokens"
  | "device_long_offline"
  | "collector_outdated"
  | "app_outdated"
  | "backfill_incomplete"
  | "client_error"
  | "imported_history";

export interface Caveat {
  code: CaveatCode;
  message: string;
  severity: "info" | "warning" | "error";
  deviceId?: string;
  client?: string;
}

export interface Summary extends Measures {
  range: RangeOut;
  sessionCount: number;
  turnCount: number;
  activeDays: number;
  byClient: Slice[];
  byModel: Slice[];
  byDevice: Slice[];
  byProject: Slice[];
  caveats: Caveat[];
  /** Which public price catalog the cost estimates use (D042); null until the server has one. */
  pricing: PricingInfo | null;
  generatedAt: string;
}

export interface PricingInfo {
  /** Local date of the newest catalog download, YYYY-MM-DD. */
  asOf: string;
  version: string;
}

export type Bucket = "hour" | "day" | "week" | "month";
export type GroupBy = "none" | "client" | "model" | "device" | "project";

export interface Series extends Measures {
  key: string;
  label: string;
  points: Point[];
}

export interface Timeseries {
  range: RangeOut;
  bucket: Bucket;
  groupBy: GroupBy;
  buckets: string[];
  series: Series[];
  generatedAt: string;
}

export interface HeatDay {
  date: string;
  totalTokens: number;
  costUsd: number;
  messageCount: number;
}

export interface HeatCell {
  weekday: number;
  hour: number;
  totalTokens: number;
  messageCount: number;
}

export interface Heatmap {
  range: RangeOut;
  kind: "calendar" | "hour_weekday";
  days?: HeatDay[];
  cells?: HeatCell[];
  maxTotalTokens: number;
  generatedAt: string;
}

export interface SessionSummary extends Measures {
  sessionKey: string;
  sessionId: string;
  deviceId: string;
  deviceName: string;
  client: string;
  models: string[];
  projectLabel?: string;
  startedAt: string;
  lastUsedAt: string;
  turnCount: number;
  subagentCount: number;
}

export type SessionSort = "recent" | "tokens" | "cost" | "turns";

export interface SessionPage {
  range: RangeOut;
  sessions: SessionSummary[];
  total: number;
  nextCursor?: string;
  generatedAt: string;
}

export interface SessionTurn extends Measures {
  index: number;
  startedAt: string;
  endedAt: string;
  durationMs?: number;
  models: string[];
  isSubagent: boolean;
  subagentSessionId?: string;
}

export interface Subagent extends Measures {
  sessionId: string;
  startedAt: string;
  lastUsedAt: string;
  models: string[];
  turnCount: number;
}

export interface SessionDetail {
  session: SessionSummary;
  turns: SessionTurn[];
  subagents: Subagent[];
  truncated: boolean;
  generatedAt: string;
}

export interface ClientCoverage {
  client: string;
  detected: boolean;
  lastEventAt?: string;
  lastScanEvents?: number;
  errorCode?: string;
  updatedAt: string;
}

export type Connection = "online" | "offline" | "long_offline";

export interface DeviceStatus {
  deviceId: string;
  displayName: string;
  platform: string | null;
  formFactor: string | null;
  osName: string | null;
  osVersion: string | null;
  createdAt: string;
  lastSeenAt: string | null;
  lastUploadAt: string | null;
  collectorVersion: string | null;
  tokscaleRev: string | null;
  parserGeneration: string | null;
  backfillComplete: boolean;
  lastScanAt: string | null;
  lastScanDurationMs: number | null;
  outboxBacklog: number | null;
  connection: Connection;
  outdated: boolean;
  isCurrent: boolean;
  clients: ClientCoverage[];
}

export interface DeviceUsage extends Measures {
  deviceId: string;
  byClient: Slice[];
}

export interface Devices {
  range: RangeOut;
  devices: DeviceStatus[];
  usage: DeviceUsage[];
  clients: string[];
  caveats: Caveat[];
  thresholds: { onlineMs: number; longOfflineMs: number };
  minCollectorVersion: string;
  generatedAt: string;
}

export interface FilterOption {
  key: string;
  label: string;
  totalTokens: number;
}

export interface FilterOptions {
  devices: FilterOption[];
  clients: FilterOption[];
  models: FilterOption[];
  projects: FilterOption[];
  firstEventDate?: string;
  timeZone: string;
}

export interface Health {
  status: string;
  serverVersion: string;
  schemaVersion: number;
  /** Stored rows of this device's tenant only. */
  eventCount: number;
  deviceCount: number;
  lastIngestAt?: string;
  timeZone: string;
  minCollectorVersion: string;
  minAppVersion: string;
  generatedAt: string;
}

/** Local app state from the Rust side; never contains a token. */
export interface AppStatus {
  paired: boolean;
  /** "local": the server runs inside the app, data stays on this computer (D044). */
  mode: "local" | "server" | null;
  deviceId: string | null;
  /** Server address; null in local mode. */
  server: string | null;
  appVersion: string;
  collector: {
    running: boolean;
    paused: boolean;
    lastScanAt: number | null;
    lastUploadAt: number | null;
    lastHeartbeatAt: number | null;
    backlog: number | null;
    uploadState: string | null;
    backfillComplete: boolean | null;
    lastError: string | null;
  };
  autostart: boolean | null;
  platform: "macos" | "windows" | "linux";
}

export interface PairCode {
  code: string;
  expiresAt: string;
}

/** Updater state from Rust; checking/downloading never carries user credentials. */
export interface UpdateStatus {
  autoCheck: boolean;
  phase: "idle" | "checking" | "available" | "latest" | "downloading" | "installing" | "error";
  version: string | null;
  notes: string | null;
  downloadedBytes: number;
  totalBytes: number | null;
  error: string | null;
  checkedAt: number | null;
}

export type ApiErrorKind =
  | "not_paired"
  | "unauthorized"
  | "network"
  | "bad_request"
  | "not_found"
  | "forbidden"
  | "rate_limited"
  | "server"
  | "keychain"
  | "unavailable";

export class ApiError extends Error {
  constructor(
    public kind: ApiErrorKind,
    public status?: number,
  ) {
    super(kind);
  }
}
