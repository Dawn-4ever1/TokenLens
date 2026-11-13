import { Check, Monitor, Moon, RefreshCw, Sun } from "lucide-react";
import { useState, type ReactNode } from "react";
import { LoadError } from "@/components/common";
import { collectorState } from "@/components/Shell";
import { UpdateProgress } from "@/components/UpdateNotice";
import { Badge, Button, Card, Dialog, Segmented, StatusDot, Switch, cn } from "@/components/ui";
import { app, errorMessage } from "@/lib/api";
import { useData, useDashboard } from "@/lib/data";
import { dateTime, duration, exact, relativeTime, shortId } from "@/lib/format";
import { DIRECTIONS, useSettings, type Direction, type ThemeMode } from "@/lib/settings";
import { useStatus } from "@/lib/status";
import { updateInProgress, useUpdates } from "@/lib/updates";
import type { AppStatus, Devices, Health } from "@/lib/types";
import { ConnectServer } from "./Pairing";

const THEMES: { key: ThemeMode; label: ReactNode }[] = [
  { key: "light", label: <span className="inline-flex items-center gap-1.5"><Sun className="h-3.5 w-3.5" aria-hidden />浅色</span> },
  { key: "dark", label: <span className="inline-flex items-center gap-1.5"><Moon className="h-3.5 w-3.5" aria-hidden />深色</span> },
  { key: "system", label: <span className="inline-flex items-center gap-1.5"><Monitor className="h-3.5 w-3.5" aria-hidden />跟随系统</span> },
];

const REFRESH: { key: string; label: string }[] = [
  { key: "30", label: "30 秒" },
  { key: "60", label: "1 分钟" },
  { key: "300", label: "5 分钟" },
  { key: "0", label: "不自动" },
];

export function Settings() {
  return (
    <div className="mx-auto max-w-3xl space-y-5">
      <Appearance />
      <Updates />
      <Collection />
      <Connection />
      <Definitions />
    </div>
  );
}

function Updates() {
  const { status, busy, check, setAutoCheck, setDialogOpen } = useUpdates();
  const { status: appStatus } = useStatus();
  const progressing = updateInProgress(status);
  return (
    <Card title="应用更新" bodyClassName="pt-1">
      <Row title="自动检查更新" hint="启动后约 10 秒检查，之后每 24 小时一次。只读取 GitHub 的公开版本信息，不发送用量或账号信息；下载和安装前会征求你同意">
        <Switch label="自动检查更新" checked={status?.autoCheck ?? true} disabled={!status || busy || progressing} onChange={(enabled) => void setAutoCheck(enabled)} />
      </Row>
      <Row title={`当前版本 ${appStatus?.appVersion ?? "—"}`} hint="关闭自动检查后，仍可在这里手动检查">
        {status?.version && <Button size="sm" variant="ghost" className="text-primary" onClick={() => setDialogOpen(true)}>有新版本 {status.version}</Button>}
        <Button size="sm" disabled={!status || busy || progressing || status.phase === "checking"} onClick={() => void check()}>
          <RefreshCw className={cn("h-3.5 w-3.5", status?.phase === "checking" && "animate-spin")} aria-hidden />
          {status?.phase === "checking" ? "正在检查…" : "检查更新"}
        </Button>
      </Row>
      {status?.phase === "latest" && <p role="status" className="pt-1 text-xs text-ink-2">已是最新版本</p>}
      {status?.phase === "error" && <p role="status" className="pt-1 text-xs leading-5 text-bad">{status.error || "更新没有完成，请稍后重试。"}</p>}
      {status && progressing && <div className="pt-2"><UpdateProgress status={status} /></div>}
    </Card>
  );
}

function Row({ title, hint, children }: { title: string; hint?: ReactNode; children: ReactNode }) {
  return (
    <div className="flex flex-wrap items-center justify-between gap-x-6 gap-y-2 border-b border-line/60 py-3.5 last:border-0">
      <div className="min-w-0 flex-1 basis-60">
        <div className="text-[13px] font-medium text-ink">{title}</div>
        {hint && <div className="mt-0.5 text-xs leading-5 text-ink-3">{hint}</div>}
      </div>
      <div className="flex shrink-0 items-center gap-2">{children}</div>
    </div>
  );
}

function Appearance() {
  const { settings, update } = useSettings();
  return (
    <Card title="外观" bodyClassName="pt-1">
      <Row title="主题">
        <Segmented label="主题" value={settings.theme} options={THEMES} onChange={(theme) => update({ theme })} />
      </Row>
      <div className="py-3.5">
        <div className="text-[13px] font-medium text-ink">视觉风格</div>
        <div className="mt-0.5 text-xs text-ink-3">三种方向只是配色和字体不同，数字和布局完全一样，随时可以换。</div>
        <div role="radiogroup" aria-label="视觉风格" className="mt-3 grid grid-cols-3 gap-3">
          {DIRECTIONS.map((direction) => (
            <DirectionCard
              key={direction.key}
              direction={direction.key}
              label={direction.label}
              description={direction.description}
              active={settings.direction === direction.key}
              onPick={() => update({ direction: direction.key })}
            />
          ))}
        </div>
      </div>
      <Row title="自动刷新" hint="看板窗口打开时多久重新读取一次数据；窗口关着时不会读取">
        <Segmented
          label="自动刷新"
          size="sm"
          value={String(settings.refreshSeconds)}
          options={REFRESH}
          onChange={(value) => update({ refreshSeconds: Number(value) })}
        />
      </Row>
      <Row title="显示精确数字" hint={`Token 数显示每一位（如 ${exact(3531604092)}），而不是简写（3.53B）。在任意页面点一下 token 数也能切换`}>
        <Switch label="显示精确数字" checked={settings.exactNumbers} onChange={(exactNumbers) => update({ exactNumbers })} />
      </Row>
    </Card>
  );
}

function DirectionCard({ direction, label, description, active, onPick }: { direction: Direction; label: string; description: string; active: boolean; onPick: () => void }) {
  const { dark } = useSettings();
  return (
    <button
      type="button"
      role="radio"
      aria-checked={active}
      onClick={onPick}
      className={cn("min-w-0 rounded-xl border p-2.5 text-left transition-colors", active ? "border-primary ring-1 ring-primary" : "border-line hover:bg-surface-2")}
    >
      <div data-direction={direction} className={cn("overflow-hidden rounded-lg border border-[var(--border)] bg-[var(--bg)] p-2", dark && "dark")}>
        <div className="flex gap-1.5">
          <div className="h-10 w-3 rounded bg-[var(--surface-2)]" />
          <div className="flex-1 space-y-1.5">
            <div className="h-3 w-2/3 rounded-sm bg-[var(--ink)] opacity-80" />
            <div className="flex h-6 items-end gap-0.5 rounded bg-[var(--surface)] p-1">
              {[40, 65, 35, 80, 55, 90].map((h, i) => (
                <div key={i} className="flex-1 rounded-[1px] bg-[var(--primary)]" style={{ height: `${h}%` }} />
              ))}
            </div>
          </div>
        </div>
      </div>
      <div className="mt-2 flex items-center gap-1.5 text-[13px] font-medium text-ink">
        {label}
        {active && <Check className="h-3.5 w-3.5 text-primary" aria-hidden />}
      </div>
      <div className="mt-0.5 text-[11px] leading-4 text-ink-3">{description}</div>
    </button>
  );
}

function Collection() {
  const { status, set } = useStatus();
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  if (!status) return null;
  const c = status.collector;
  const state = collectorState(status);
  const run = (action: () => Promise<AppStatus>) => {
    setBusy(true);
    setError(null);
    action()
      .then(set)
      .catch((reason: unknown) => setError(errorMessage(reason)))
      .finally(() => setBusy(false));
  };
  return (
    <Card title="采集" bodyClassName="pt-1">
      <Row title="开机时自动在后台运行" hint={status.autostart === null ? "开发版不提供开机启动" : "登录后在后台采集，不会弹出窗口"}>
        <Switch
          label="开机启动"
          checked={status.autostart === true}
          disabled={status.autostart === null || busy}
          onChange={(enabled) => run(() => app.setAutostart(enabled))}
        />
      </Row>
      <Row title="暂停采集" hint={status.mode === "local" ? "暂停后不读取日志；恢复后会把暂停期间的用量补上" : "暂停后不读取日志、不上传；恢复后会把暂停期间的用量补上"}>
        <Switch label="暂停采集" checked={c.paused} disabled={busy} onChange={(paused) => run(() => app.setPaused(paused))} />
      </Row>
      <Row
        title="状态"
        hint={
          <>
            最后扫描 {relativeTime(c.lastScanAt)} · {status.mode === "local" ? "最后写入" : "最后上传"} {relativeTime(c.lastUploadAt)} · 最后心跳 {relativeTime(c.lastHeartbeatAt)}
            {c.backlog ? ` · ${status.mode === "local" ? "待写入" : "待上传"} ${exact(c.backlog)} 条` : ""}
            {c.backfillComplete === false ? (status.mode === "local" ? " · 历史数据仍在整理" : " · 历史数据仍在上传") : ""}
          </>
        }
      >
        <span className="inline-flex items-center gap-1.5 text-[13px] text-ink-2">
          <StatusDot tone={state.tone} />
          {state.label}
        </span>
        <Button size="sm" disabled={busy || c.paused} onClick={() => run(app.syncNow)}>
          <RefreshCw className="h-3.5 w-3.5" aria-hidden /> 立即同步
        </Button>
      </Row>
      {c.lastError && <p className="pb-2 text-xs text-bad">最近的错误：{c.lastError}</p>}
      {error && <p className="pb-2 text-xs text-bad">{error}</p>}
    </Card>
  );
}

function Connection() {
  const { status, set } = useStatus();
  const { query } = useData();
  const health = useDashboard<Health>("health");
  const devices = useDashboard<Devices>("devices", query());
  const [confirm, setConfirm] = useState(false);
  const [connecting, setConnecting] = useState(false);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  if (!status) return null;
  const thresholds = devices.data?.thresholds;
  const local = status.mode === "local";
  const connected = Boolean(status.server);
  const signedIn = Boolean(status.accountEmail);
  const signOut = async () => {
    setBusy(true);
    setError(null);
    try {
      set(await app.logoutAccount());
    } catch (next) {
      setError(errorMessage(next));
    } finally {
      setBusy(false);
    }
  };
  const disconnect = async () => {
    setBusy(true);
    setError(null);
    try {
      set(await app.unpair());
      setConfirm(false);
    } catch (next) {
      setError(errorMessage(next));
    } finally {
      setBusy(false);
    }
  };
  return (
    <Card title="连接" bodyClassName="pt-1">
      <Row title="当前查看" hint="本机持续记录，可在「设备」页面切换查看本机或全部设备">
        <span className="text-xs text-ink-2">{local ? "本机数据" : "全部设备"}</span>
      </Row>
      <Row title="服务器" hint={connected ? "本机记录和服务器同步同时运行" : "未连接，数据保存在这台电脑上"}>
        <span className="selectable font-mono text-xs text-ink-2">{status.server ?? "未连接"}</span>
      </Row>
      <Row
        title="账号"
        hint={
          signedIn
            ? "登录后可以在多台电脑之间共用同一个空间，配对码也在这里查看"
            : "用邮箱登录你的空间；第一次登录会自动创建"
        }
      >
        {signedIn ? (
          <span className="flex items-center gap-2">
            <span className="selectable text-xs text-ink-2">{status.accountEmail}</span>
            <Button size="sm" onClick={() => void signOut()} disabled={busy}>
              退出登录
            </Button>
          </span>
        ) : (
          <Button size="sm" onClick={() => setConnecting(true)}>
            登录
          </Button>
        )}
      </Row>
      <Row title="这台电脑的设备 ID" hint="看板页面拿不到读取数据用的凭据">
        <span className="selectable font-mono text-xs text-ink-2">{status.deviceId ? shortId(status.deviceId, 16) : "—"}</span>
      </Row>
      <Row
        title={local ? "本机服务状态" : "服务器状态"}
        hint={health.data ? `${exact(health.data.eventCount)} 条用量 · ${health.data.deviceCount} 台设备 · 最后接收 ${relativeTime(health.data.lastIngestAt)} · 时区 ${health.data.timeZone}` : undefined}
      >
        {health.data ? (
          <Badge tone={health.data.status === "ok" ? "good" : "warn"}>{health.data.status === "ok" ? "正常" : health.data.status}</Badge>
        ) : health.error ? <LoadError error={health.error} onRetry={health.reload} compact /> : <span className="text-xs text-ink-3">读取中…</span>}
      </Row>
      {thresholds && !local && (
        <Row title="在线判定" hint="由服务器决定，所有设备一致">
          <span className="text-xs text-ink-2">{duration(thresholds.onlineMs)} 内有心跳算在线，超过 {duration(thresholds.longOfflineMs)} 算长期未上线</span>
        </Row>
      )}
      {connected ? (
        <Row title="断开服务器" hint="停止服务器同步并查看本机数据；本机采集继续，服务器历史保留">
          <Button variant="danger" size="sm" onClick={() => setConfirm(true)}>断开服务器</Button>
        </Row>
      ) : (
        <Row title="连接服务器" hint="合并查看多台设备，也可以直接在「设备 → 添加设备」连接">
          <Button size="sm" onClick={() => setConnecting(true)}>连接服务器</Button>
        </Row>
      )}
      {error && !confirm && <p role="alert" className="pb-2 text-xs text-bad">{error}</p>}
      <ConnectServer open={connecting} onOpenChange={setConnecting} />
      <Dialog open={confirm} onOpenChange={setConfirm} title="断开服务器？">
        <p className="text-[13px] leading-6 text-ink-2">会停止服务器同步，自动查看本机数据。本机采集和已有数据继续保留，服务器上的历史也不会删除。以后可在「设备 → 添加设备」重新连接。</p>
        {error && <p role="alert" className="mt-3 text-xs text-bad">{error}</p>}
        <div className="mt-5 flex justify-end gap-2">
          <Button onClick={() => setConfirm(false)} disabled={busy}>取消</Button>
          <Button variant="danger" disabled={busy} onClick={() => void disconnect()}>{busy ? "正在断开…" : "断开服务器"}</Button>
        </div>
      </Dialog>
    </Card>
  );
}

function Definitions() {
  const { status } = useStatus();
  const { timeZone } = useData();
  return (
    <Card title="口径说明" bodyClassName="pt-1">
      <dl className="divide-y divide-line/60 text-[13px] leading-6">
        <Definition term="总 tokens">输入 + 输出 + 缓存读取 + 缓存写入 + 推理，五类互斥相加。缓存读取通常占九成以上，所以总量不等于计费 token。</Definition>
        <Definition term="缓存命中率">缓存读取 ÷（输入 + 缓存读取 + 缓存写入）。不含输出和推理。</Definition>
        <Definition term="成本">按公开 API 价格折算的「API 等价成本」，不是订阅或中转站账单。价格来自 LiteLLM、OpenRouter、models.dev 三份公开价格表，每天更新一次（本机数据由这台电脑算价，服务器数据由服务器算价），价格变了历史会一起重算。没有价格的模型不计入，会单独提示；Qoder 只显示 credits，不折算美元。</Definition>
        <Definition term="Qoder 估算">Qoder 本地大多只记 credits，这里按校准汇率折算成 token，标为「估算」，原始 credits 会一起显示。</Definition>
        <Definition term="日期">按 {timeZone} 的自然日统计；跨设备的时间都换算到这个时区。</Definition>
        <Definition term="隐私">
          {!status?.server
            ? "未连接服务器时数据只保存在这台电脑上，会每天下载公开价格表，并按「自动检查更新」的设置读取 GitHub 公开版本信息；这两项请求都不携带用量或账号信息。"
            : "本机保留数据，同时向已连接的服务器同步数字统计。即使正在查看本机数据，服务器同步也会继续；提示词、回答、代码、文件路径和会话标题都不会离开这台电脑。"}
        </Definition>
      </dl>
      <p className="mt-3 text-xs text-ink-3">
        应用版本 {status?.appVersion ?? "—"} · 数据更新于 {dateTime(Date.now(), timeZone)}
      </p>
    </Card>
  );
}

function Definition({ term, children }: { term: string; children: ReactNode }) {
  return (
    <div className="grid grid-cols-[96px_minmax(0,1fr)] gap-3 py-2.5">
      <dt className="font-medium text-ink">{term}</dt>
      <dd className="text-ink-2">{children}</dd>
    </div>
  );
}
