import { Check, Monitor, Moon, RefreshCw, Sun } from "lucide-react";
import { useState, type ReactNode } from "react";
import { LoadError } from "@/components/common";
import { collectorState } from "@/components/Shell";
import { Badge, Button, Card, Dialog, Segmented, StatusDot, Switch, cn } from "@/components/ui";
import { app, errorMessage } from "@/lib/api";
import { useData, useDashboard } from "@/lib/data";
import { dateTime, duration, exact, relativeTime, shortId } from "@/lib/format";
import { DIRECTIONS, useSettings, type Direction, type ThemeMode } from "@/lib/settings";
import { useStatus } from "@/lib/status";
import type { AppStatus, Devices, Health } from "@/lib/types";

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
      <Collection />
      <Connection />
      <Definitions />
    </div>
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
  const [error, setError] = useState<string | null>(null);
  if (!status) return null;
  const thresholds = devices.data?.thresholds;
  const local = status.mode === "local";
  const unpair = () => {
    setError(null);
    app
      .unpair()
      .then(set)
      .catch((reason: unknown) => setError(errorMessage(reason)));
  };
  return (
    <Card title="连接" bodyClassName="pt-1">
      {local ? (
        <Row title="数据位置" hint="本机模式：不连服务器，数据只保存在这台电脑上">
          <span className="text-xs text-ink-2">只在这台电脑上</span>
        </Row>
      ) : (
        <Row title="服务器">
          <span className="selectable font-mono text-xs text-ink-2">{status.server ?? "—"}</span>
        </Row>
      )}
      <Row title="这台电脑的设备 ID" hint={local ? "看板页面拿不到读取数据用的凭据" : "凭据保存在系统钥匙串里，看板页面拿不到"}>
        <span className="selectable font-mono text-xs text-ink-2">{status.deviceId ? shortId(status.deviceId, 16) : "—"}</span>
      </Row>
      <Row
        title="服务状态"
        hint={
          health.data
            ? `服务端 ${health.data.serverVersion} · ${exact(health.data.eventCount)} 条用量 · ${health.data.deviceCount} 台设备 · 最后接收 ${relativeTime(health.data.lastIngestAt)} · 时区 ${health.data.timeZone}`
            : undefined
        }
      >
        {health.data ? (
          <Badge tone={health.data.status === "ok" ? "good" : "warn"}>{health.data.status === "ok" ? "正常" : health.data.status}</Badge>
        ) : health.error ? (
          <LoadError error={health.error} onRetry={health.reload} compact />
        ) : (
          <span className="text-xs text-ink-3">读取中…</span>
        )}
      </Row>
      {thresholds && !local && (
        <Row title="在线判定" hint="由服务器决定，所有设备一致">
          <span className="text-xs text-ink-2">
            {duration(thresholds.onlineMs)} 内有心跳算在线，超过 {duration(thresholds.longOfflineMs)} 算长期未上线
          </span>
        </Row>
      )}
      {local ? (
        <Row title="改为连接服务器" hint="想把几台电脑合在一起看时用；这台电脑上已有的本机数据会保留">
          <Button size="sm" onClick={() => setConfirm(true)}>
            改为连接服务器
          </Button>
        </Row>
      ) : (
        <Row title="取消配对" hint="删除这台电脑保存的凭据并停止采集；服务器上的历史数据不受影响">
          <Button variant="danger" size="sm" onClick={() => setConfirm(true)}>
            取消配对
          </Button>
        </Row>
      )}
      {error && <p className="pb-2 text-xs text-bad">{error}</p>}
      <Dialog open={confirm} onOpenChange={setConfirm} title={local ? "改为连接服务器？" : "取消这台电脑的配对？"}>
        <p className="text-[13px] leading-6 text-ink-2">
          {local
            ? "会停止本机模式，回到开始页面，在那里填服务器地址和配对码。配对成功后，这台电脑的历史用量会全部上传到服务器。已有的本机数据会保留，以后回到本机模式还能看到。"
            : "这台电脑会停止采集和上传，回到配对页面。服务器上的设备记录和历史用量会保留；如果想彻底移除，请在「设备」页面选择「移除设备」。"}
        </p>
        <div className="mt-5 flex justify-end gap-2">
          <Button onClick={() => setConfirm(false)}>取消</Button>
          <Button
            variant={local ? "primary" : "danger"}
            onClick={() => {
              setConfirm(false);
              unpair();
            }}
          >
            {local ? "继续" : "取消配对"}
          </Button>
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
        <Definition term="成本">按公开 API 价格折算的「API 等价成本」，不是订阅或中转站账单。价格来自 LiteLLM、OpenRouter、models.dev 三份公开价格表，每天更新一次（本机模式由这台电脑下载，连接服务器时由服务器下载），价格变了历史会一起重算。没有价格的模型不计入，会单独提示；Qoder 只显示 credits，不折算美元。</Definition>
        <Definition term="Qoder 估算">Qoder 本地大多只记 credits，这里按校准汇率折算成 token，标为「估算」，原始 credits 会一起显示。</Definition>
        <Definition term="日期">按 {timeZone} 的自然日统计；跨设备的时间都换算到这个时区。</Definition>
        <Definition term="隐私">
          {status?.mode === "local"
            ? "本机模式下数据只保存在这台电脑上，只会每天下载一次公开价格表。"
            : "只上传数字统计；提示词、回答、代码、文件路径和会话标题都不会离开这台电脑。"}
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
