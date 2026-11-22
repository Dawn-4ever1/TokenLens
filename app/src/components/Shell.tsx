import { Cloud as CloudIcon, LayoutDashboard, LogOut, MessagesSquare, MonitorSmartphone, Settings as SettingsIcon, TrendingUp, UserRound, type LucideIcon } from "lucide-react";
import { lazy, Suspense, useEffect, useState, type ReactNode } from "react";
import { relativeTime } from "@/lib/format";
import { PAGES, useNav, type Page } from "@/lib/nav";
import { useStatus } from "@/lib/status";
import type { AppStatus } from "@/lib/types";
import { app, errorMessage } from "@/lib/api";
import { AppMark } from "./AppMark";
import { DeviceScopePicker, FilterMenu, RangePicker, RefreshStatus } from "./FilterBar";
import { SessionDrawer } from "./SessionDrawer";
import { UpdateDialog, UpdateNotice } from "./UpdateNotice";
import { Button, Dialog, Popover, PopoverContent, PopoverTrigger, StatusDot, Tip, cn } from "./ui";
import { Overview } from "@/pages/Overview";
import { ConnectServer } from "@/pages/Pairing";

const Trends = lazy(() => import("@/pages/Trends").then((m) => ({ default: m.Trends })));
const Sessions = lazy(() => import("@/pages/Sessions").then((m) => ({ default: m.Sessions })));
const Devices = lazy(() => import("@/pages/Devices").then((m) => ({ default: m.Devices })));
const Cloud = lazy(() => import("@/pages/Cloud").then((m) => ({ default: m.Cloud })));
const Settings = lazy(() => import("@/pages/Settings").then((m) => ({ default: m.Settings })));

const ICONS: Record<Page, LucideIcon> = {
  overview: LayoutDashboard,
  trends: TrendingUp,
  sessions: MessagesSquare,
  devices: MonitorSmartphone,
  cloud: CloudIcon,
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
          className={cn("flex shrink-0 flex-wrap items-center gap-x-3 gap-y-2 border-b border-line/70 px-[var(--page-px)] pb-3", mac ? "pt-3.5" : "pt-3")}
        >
          <h1 data-tauri-drag-region className="display mr-auto text-[length:var(--page-title-size)] text-ink">
            {title}
          </h1>
          {FILTERED.includes(page) && (
            <div className="flex flex-wrap items-center gap-2">
              <RangePicker />
              <DeviceScopePicker />
              <FilterMenu />
              <RefreshStatus />
            </div>
          )}
        </header>
        <main className="min-h-0 flex-1 overflow-y-auto overflow-x-hidden">
          <div className="mx-auto max-w-[1440px] px-[var(--page-px)] pb-10 pt-5">
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
    case "cloud":
      return <Cloud />;
    case "settings":
      return <Settings />;
  }
}

function Sidebar({ mac, status }: { mac: boolean; status: AppStatus | null }) {
  const { page, go } = useNav();
  const { set } = useStatus();
  const [connecting, setConnecting] = useState(false);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const signedIn = Boolean(status?.accountEmail || status?.accountDisplayName);
  const brand = (
    <>
      <span data-sidebar-brand-mark className="flex h-5 w-5 shrink-0 items-center justify-center">
        <AppMark className="pointer-events-none h-5 w-5" />
      </span>
      <span className="display hidden truncate text-[13px] leading-5 text-ink-2 min-[1100px]:inline">
        TokenLens
      </span>
    </>
  );
  return (
    <nav aria-label="主导航" className="flex w-[var(--sidebar-narrow)] shrink-0 flex-col border-r border-line/70 bg-surface-2/50 min-[1100px]:w-[var(--sidebar-wide)]">
      <div
        data-tauri-drag-region
        data-sidebar-brand
        className={cn("flex shrink-0 justify-center px-2 min-[1100px]:justify-start min-[1100px]:px-3", mac ? "h-[72px] items-end pb-3" : "h-14 items-center")}
      >
        {signedIn ? (
          <div data-tauri-drag-region className="flex items-center gap-2.5">{brand}</div>
        ) : (
          <Tip content="登录 TokenLens" side="right">
            <button
              type="button"
              aria-label="登录 TokenLens"
              onClick={() => { setError(null); setConnecting(true); }}
              className="flex min-h-8 items-center gap-2.5 rounded-lg px-2 text-left transition-colors hover:bg-surface/70"
            >
              {brand}
            </button>
          </Tip>
        )}
      </div>
      <ul className="flex flex-1 flex-col gap-0.5 px-2.5">
        {/* The brand occupies its own row. Windows aligns its mark with the
            page title; the navigation then continues below it in order. */}
        {PAGES.map((item, index) => {
          const Icon = ICONS[item.key];
          const active = item.key === page;
          return (
            <li key={item.key} className={cn(index === PAGES.length - 1 && "mt-auto")}>
              {item.key === "settings" && (
                <div className="mb-2">
                  <AccountEntry status={status} busy={busy} error={error} onSignIn={() => { setError(null); setConnecting(true); }} onSignOut={async () => {
                    setBusy(true);
                    setError(null);
                    try { set(await app.logoutAccount()); } catch (next) { setError(errorMessage(next)); } finally { setBusy(false); }
                  }} />
                  <ConnectServer open={connecting} onOpenChange={setConnecting} />
                </div>
              )}
              <Tip content={item.label} side="right">
                <button
                  type="button"
                  aria-label={item.label}
                  aria-current={active ? "page" : undefined}
                  onClick={() => go(item.key)}
                  className={cn(
                    "flex h-9 w-full items-center justify-center gap-2.5 rounded-[var(--radius-control)] px-2.5 text-[13px] font-medium transition-colors min-[1100px]:justify-start",
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

function AccountEntry({ status, busy, error, onSignIn, onSignOut }: {
  status: AppStatus | null;
  busy: boolean;
  error: string | null;
  onSignIn: () => void;
  onSignOut: () => Promise<void>;
}) {
  const providerName = status?.accountDisplayName;
  // Older servers and email sign-ins have no provider profile. They still
  // have an account session and show the same responsive account entry.
  const name = providerName || status?.accountEmail;
  const provider = status?.accountProvider
    // Older servers report no provider; a display name still means GitHub.
    ?? (providerName ? "github" : null);
  const accountLabel = provider === "google" ? `Google 账号 ${name}`
    : provider === "github" ? `GitHub 账号 ${name}`
    : `账号 ${name}`;
  const [imageFailed, setImageFailed] = useState(false);
  const [confirming, setConfirming] = useState(false);
  const [menuOpen, setMenuOpen] = useState(false);
  useEffect(() => {
    if (!menuOpen) return;
    const onResize = () => {
      if (window.innerWidth >= 1100) setMenuOpen(false);
    };
    window.addEventListener("resize", onResize);
    return () => window.removeEventListener("resize", onResize);
  }, [menuOpen]);
  if (name) {
    return (
      <div className="px-1">
        {/* In the collapsed rail the avatar sits directly above Settings,
            both centred in the sidebar, and opens a compact popover on click
            so sign-out remains reachable without widening the window. */}
        <div
          role="group"
          aria-label={accountLabel}
          className="group flex h-9 w-full items-center justify-center gap-3 rounded-lg px-1 min-[1100px]:h-11 min-[1100px]:justify-start min-[1100px]:px-2"
        >
          <Popover
            open={menuOpen}
            onOpenChange={(next) => {
              if (!next || window.innerWidth < 1100) setMenuOpen(next);
            }}
          >
            <Tip content={accountLabel} side="right">
              <PopoverTrigger asChild>
                <button
                  type="button"
                  aria-label="打开账号菜单"
                  data-account-identity
                  className="flex min-w-0 flex-none items-center justify-center rounded-full p-0 text-left min-[1100px]:flex-1 min-[1100px]:cursor-default min-[1100px]:justify-start min-[1100px]:gap-3 min-[1100px]:rounded-none"
                >
                  {status?.accountAvatarUrl && !imageFailed ? (
                    <img
                      src={status.accountAvatarUrl}
                      alt=""
                      className="h-5 w-5 shrink-0 rounded-full min-[1100px]:h-6 min-[1100px]:w-6"
                      onError={() => setImageFailed(true)}
                    />
                  ) : <UserRound className="h-4 w-4 shrink-0 text-ink-2 min-[1100px]:h-5 min-[1100px]:w-5" aria-hidden />}
                  <span data-account-name className="hidden min-w-0 flex-1 truncate text-xs font-medium text-ink min-[1100px]:inline">{name}</span>
                </button>
              </PopoverTrigger>
            </Tip>
            <PopoverContent side="right" align="end" className="w-52 p-2">
              <div className="truncate px-2 py-1 text-xs font-medium text-ink" title={accountLabel}>{accountLabel}</div>
              <button
                type="button"
                disabled={busy}
                onClick={() => {
                  setMenuOpen(false);
                  setConfirming(true);
                }}
                className="mt-1 flex w-full items-center gap-2 rounded-md px-2 py-1.5 text-left text-[13px] text-bad hover:bg-surface-2 disabled:opacity-50"
              >
                <LogOut className="h-3.5 w-3.5 shrink-0" aria-hidden />
                退出登录
              </button>
            </PopoverContent>
          </Popover>
          <div className="hidden min-[1100px]:contents">
            <Tip content="退出登录" side="right">
              <button
                type="button"
                aria-label="退出登录"
                disabled={busy}
                onClick={() => setConfirming(true)}
                className="-mr-1 ml-auto flex h-7 w-7 shrink-0 items-center justify-center rounded-md text-ink-3 transition-colors hover:bg-surface hover:text-ink disabled:opacity-50"
              >
                <LogOut className="h-3 w-3 min-[1100px]:h-3.5 min-[1100px]:w-3.5" aria-hidden />
              </button>
            </Tip>
          </div>
        </div>
        {error && <p role="alert" className="px-2 pt-1 text-[11px] leading-4 text-bad">{error}</p>}
        <Dialog
          open={confirming}
          onOpenChange={(next) => { if (!busy) setConfirming(next); }}
          title="退出登录"
        >
          <div className="space-y-4">
            <p className="text-sm leading-6 text-ink-2">退出后，这台电脑会继续采集用量，看板只显示本机数据，并停止同步到这个云端账号。云端设备与历史不会删除；重新登录会重新连接云端，显示云端当时的设备与用量。</p>
            <div className="flex justify-end gap-2">
              <Button variant="ghost" onClick={() => setConfirming(false)}>取消</Button>
              <Button
                variant="danger"
                disabled={busy}
                onClick={() => { setConfirming(false); void onSignOut(); }}
              >
                退出登录
              </Button>
            </div>
          </div>
        </Dialog>
      </div>
    );
  }
  return (
    <div className="px-1">
      <Tip content="登录后可在多台电脑之间同步用量" side="right">
        <button type="button" aria-label="登录，同步多台设备" onClick={onSignIn} className="flex min-h-10 w-full items-center justify-center gap-2 rounded-lg bg-primary-soft px-2 text-xs font-medium text-primary hover:bg-primary-soft/80 min-[1100px]:justify-start">
          <UserRound className="h-4 w-4 shrink-0" aria-hidden />
          <span className="hidden min-[1100px]:inline">登录 / 同步多台设备</span>
        </button>
      </Tip>
      {error && <p role="alert" className="px-2 pt-1 text-[11px] leading-4 text-bad">{error}</p>}
    </div>
  );
}

export function collectorState(status: AppStatus | null): { tone: "good" | "neutral" | "warn" | "bad"; label: string } {
  if (!status) return { tone: "neutral", label: "读取中" };
  const c = status.collector;
  if (c.paused) return { tone: "neutral", label: "采集已暂停" };
  if (c.lastError) return { tone: "bad", label: "采集出错" };
  if (!c.running) return { tone: "neutral", label: "采集未运行" };
  const upload = c.uploadState ?? "ok";
  if (upload === "queue_error") return { tone: "bad", label: "服务器同步出错，本机继续记录" };
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
