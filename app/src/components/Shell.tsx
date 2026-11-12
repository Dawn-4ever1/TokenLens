import { LayoutDashboard, MessagesSquare, MonitorSmartphone, Settings as SettingsIcon, TrendingUp, type LucideIcon } from "lucide-react";
import { lazy, Suspense, type ReactNode } from "react";
import { relativeTime } from "@/lib/format";
import { PAGES, useNav, type Page } from "@/lib/nav";
import { useStatus } from "@/lib/status";
import type { AppStatus } from "@/lib/types";
import { AppMark } from "./AppMark";
import { ActiveFilters, FilterMenu, RangePicker, RefreshStatus } from "./FilterBar";
import { SessionDrawer } from "./SessionDrawer";
import { UpdateDialog, UpdateNotice } from "./UpdateNotice";
import { StatusDot, Tip, cn } from "./ui";
import { Overview } from "@/pages/Overview";

const Trends = lazy(() => import("@/pages/Trends").then((m) => ({ default: m.Trends })));
const Sessions = lazy(() => import("@/pages/Sessions").then((m) => ({ default: m.Sessions })));
const Devices = lazy(() => import("@/pages/Devices").then((m) => ({ default: m.Devices })));
const Settings = lazy(() => import("@/pages/Settings").then((m) => ({ default: m.Settings })));

const ICONS: Record<Page, LucideIcon> = {
  overview: LayoutDashboard,
  trends: TrendingUp,
  sessions: MessagesSquare,
  devices: MonitorSmartphone,
  settings: SettingsIcon,
};

/** Pages that use the global time range and filters. */
const FILTERED: Page[] = ["overview", "trends", "sessions", "devices"];

export function Shell() {
  const { page, sessionKey, openSession } = useNav();
  const { status } = useStatus();
  const mac = status?.platform === "macos";
  const title = PAGES.find((item) => item.key === page)?.label ?? "";
  return (
    <div className="flex h-full">
      <Sidebar mac={mac} status={status} />
      <div className="flex min-w-0 flex-1 flex-col">
        <header
          data-tauri-drag-region
          className={cn("flex shrink-0 flex-wrap items-center gap-x-3 gap-y-2 border-b border-line/70 px-6 pb-3", mac ? "pt-3.5" : "pt-3")}
        >
          <h1 data-tauri-drag-region className="display mr-auto text-xl text-ink">
            {title}
          </h1>
          {FILTERED.includes(page) && (
            <div className="flex flex-wrap items-center gap-2">
              <RangePicker />
              <FilterMenu />
              <RefreshStatus />
            </div>
          )}
        </header>
        {FILTERED.includes(page) && <ActiveFilters />}
        <main className="min-h-0 flex-1 overflow-y-auto overflow-x-hidden">
          <div className="mx-auto max-w-[1440px] px-6 pb-10 pt-5">
            <Suspense fallback={null}>
              <PageView page={page} />
            </Suspense>
          </div>
        </main>
      </div>
      <SessionDrawer sessionKey={sessionKey} onClose={() => openSession(null)} />
      <UpdateDialog />
    </div>
  );
}

function PageView({ page }: { page: Page }): ReactNode {
  switch (page) {
    case "overview":
      return <Overview />;
    case "trends":
      return <Trends />;
    case "sessions":
      return <Sessions />;
    case "devices":
      return <Devices />;
    case "settings":
      return <Settings />;
  }
}

function Sidebar({ mac, status }: { mac: boolean; status: AppStatus | null }) {
  const { page, go } = useNav();
  return (
    <nav aria-label="主导航" className="flex w-16 shrink-0 flex-col border-r border-line/70 bg-surface-2/50 min-[1100px]:w-52">
      <div
        data-tauri-drag-region
        className={cn("flex shrink-0 items-end justify-center gap-2 px-4 pb-3 min-[1100px]:justify-start", mac ? "h-[52px]" : "h-14")}
      >
        <AppMark className="pointer-events-none h-5 w-5" />
        <span data-tauri-drag-region className="display hidden truncate text-[13px] leading-5 text-ink-2 min-[1100px]:inline">
          TokenLens
        </span>
      </div>
      <ul className="flex flex-1 flex-col gap-0.5 px-2.5">
        {PAGES.map((item, index) => {
          const Icon = ICONS[item.key];
          const active = item.key === page;
          return (
            <li key={item.key} className={cn(index === PAGES.length - 1 && "mt-auto")}>
              <Tip content={item.label} side="right">
                <button
                  type="button"
                  aria-label={item.label}
                  aria-current={active ? "page" : undefined}
                  onClick={() => go(item.key)}
                  className={cn(
                    "flex h-9 w-full items-center justify-center gap-2.5 rounded-lg px-2.5 text-[13px] font-medium transition-colors min-[1100px]:justify-start",
                    active ? "bg-surface text-ink shadow-sm" : "text-ink-2 hover:bg-surface/60 hover:text-ink",
                  )}
                >
                  <Icon className={cn("h-[18px] w-[18px] shrink-0", active && "text-primary")} aria-hidden />
                  <span className="hidden min-[1100px]:inline">{item.label}</span>
                </button>
              </Tip>
            </li>
          );
        })}
      </ul>
      <CollectorStatus status={status} />
    </nav>
  );
}

export function collectorState(status: AppStatus | null): { tone: "good" | "neutral" | "warn" | "bad"; label: string } {
  if (!status) return { tone: "neutral", label: "读取中" };
  const c = status.collector;
  if (c.paused) return { tone: "neutral", label: "采集已暂停" };
  if (c.lastError) return { tone: "bad", label: "采集出错" };
  if (!c.running) return { tone: "neutral", label: "采集未运行" };
  const upload = c.uploadState ?? "ok";
  if (upload === "pairing_required") return { tone: "bad", label: "凭据失效，需重新配对" };
  if (upload === "upgrade_required") return { tone: "warn", label: "需要升级应用" };
  const local = status.mode === "local";
  if (upload.startsWith("blocked:")) return { tone: "bad", label: local ? "本机数据库拒绝写入" : "上传被服务器拒绝" };
  if (upload.startsWith("backoff:")) return { tone: "warn", label: local ? "等待重试写入" : "等待重试上传" };
  return { tone: "good", label: "后台采集中" };
}

function CollectorStatus({ status }: { status: AppStatus | null }) {
  const state = collectorState(status);
  const lastSync = status?.collector.lastUploadAt ?? status?.collector.lastHeartbeatAt ?? null;
  const backlog = status?.collector.backlog ?? 0;
  const pending = status?.mode === "local" ? `待写入 ${backlog} 条` : `待上传 ${backlog} 条`;
  const detail = (
    <div className="space-y-0.5">
      <div>{state.label}</div>
      <div className="text-ink-3">最后同步：{relativeTime(lastSync)}</div>
      {backlog > 0 && <div className="text-ink-3">{pending}</div>}
      {status?.collector.lastError && <div className="text-bad">{status.collector.lastError}</div>}
    </div>
  );
  return (
    <div className="border-t border-line/70 px-2.5 py-3">
      <UpdateNotice />
      <Tip content={detail} side="right">
        <div className="flex items-center justify-center gap-2 rounded-lg px-2.5 py-1.5 min-[1100px]:justify-start">
          <StatusDot tone={state.tone} />
          <div className="hidden min-w-0 min-[1100px]:block">
            <div className="truncate text-xs font-medium text-ink-2">{state.label}</div>
            <div className="truncate text-[11px] text-ink-3">
              {backlog > 0 ? pending : `同步于${relativeTime(lastSync)}`}
            </div>
          </div>
        </div>
      </Tip>
    </div>
  );
}
