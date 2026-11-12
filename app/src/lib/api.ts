import type { AppStatus, PairCode } from "./types";
import { ApiError, type ApiErrorKind } from "./types";
import type { Query } from "./range";

export const isTauri = (): boolean =>
  typeof window !== "undefined" && "__TAURI_INTERNALS__" in window;

const KINDS: ApiErrorKind[] = [
  "not_paired", "unauthorized", "network", "bad_request", "not_found",
  "forbidden", "rate_limited", "server", "keychain", "unavailable",
];

function toApiError(error: unknown): ApiError {
  if (error instanceof ApiError) return error;
  if (error && typeof error === "object" && "kind" in error) {
    const kind = (error as { kind: string }).kind as ApiErrorKind;
    const status = (error as { status?: number }).status;
    return new ApiError(KINDS.includes(kind) ? kind : "server", status);
  }
  return new ApiError("server");
}

function clean(query: Query): Record<string, string> {
  const out: Record<string, string> = {};
  for (const [key, value] of Object.entries(query)) {
    if (value !== undefined && value !== "") out[key] = value;
  }
  return out;
}

async function call<T>(command: string, args: Record<string, unknown> = {}): Promise<T> {
  if (isTauri()) {
    const { invoke } = await import("@tauri-apps/api/core");
    try {
      return await invoke<T>(command, args);
    } catch (error) {
      throw toApiError(error);
    }
  }
  if (import.meta.env.DEV) {
    const mock = await import("./mock");
    try {
      return (await mock.command(command, args)) as T;
    } catch (error) {
      throw toApiError(error);
    }
  }
  throw new ApiError("unavailable");
}

/** GET /api/dashboard/{path}; the Rust side adds the read token. */
export function dashboardGet<T>(path: string, query: Query = {}): Promise<T> {
  return call<T>("dashboard_get", { path, query: clean(query) });
}

export const app = {
  status: () => call<AppStatus>("app_status"),
  pair: (input: { server: string; code: string; formFactor: "laptop" | "desktop"; name: string; autostart: boolean }) =>
    call<AppStatus>("pair_device", input),
  useLocal: (input: { formFactor: "laptop" | "desktop"; name: string; autostart: boolean }) =>
    call<AppStatus>("use_local_mode", input),
  unpair: () => call<AppStatus>("unpair_device"),
  setPaused: (paused: boolean) => call<AppStatus>("set_paused", { paused }),
  syncNow: () => call<AppStatus>("sync_now"),
  setAutostart: (enabled: boolean) => call<AppStatus>("set_autostart", { enabled }),
  createPairCode: () => call<PairCode>("create_pair_code"),
  renameDevice: (deviceId: string, displayName: string) =>
    call<{ deviceId: string; displayName: string }>("rename_device", { deviceId, displayName }),
  rotateCredentials: () => call<AppStatus>("rotate_credentials"),
  revokeDevice: (deviceId: string) => call<AppStatus>("revoke_device", { deviceId }),
};

export function errorMessage(error: unknown): string {
  const kind = error instanceof ApiError ? error.kind : "server";
  switch (kind) {
    case "not_paired":
      return "这台电脑还没有配对。";
    case "unauthorized":
      return "这台电脑的凭据已失效（可能已被吊销或轮换），需要重新配对。";
    case "network":
      return "连不上服务器。请检查网络，或确认服务器是否在运行。";
    case "rate_limited":
      return "请求太频繁，请稍后再试。";
    case "forbidden":
      return "这个操作只能在那台设备自己的应用里进行。";
    case "not_found":
      return "没有找到这条数据，可能已被删除。";
    case "bad_request":
      return "筛选条件无效，请调整后重试。";
    case "keychain":
      return "无法读取系统钥匙串中的凭据。";
    case "unavailable":
      return "请在 TokenLens 应用中打开此页面。";
    default:
      return "服务器出错了，请稍后重试。";
  }
}
