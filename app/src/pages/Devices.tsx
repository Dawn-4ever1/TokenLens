import { Check, Copy, KeyRound, Laptop, MoreHorizontal, Pencil, Plus, ShieldOff } from "lucide-react";
import { useEffect, useMemo, useState } from "react";
import { CaveatList, ClientName, LoadError, Num, ShareBar } from "@/components/common";
import { Badge, Button, Card, Dialog, EmptyState, Field, Popover, PopoverContent, PopoverTrigger, Segmented, Skeleton, StatusDot, Tip, cn, inputClass } from "@/components/ui";
import { app, errorMessage } from "@/lib/api";
import { copyText } from "@/lib/clipboard";
import { useData, useDashboard } from "@/lib/data";
import { compact, dateTime, duration, exact, relativeTime, shortId } from "@/lib/format";
import { clientColor, coverageColumns, formFactorLabel, platformLabel } from "@/lib/labels";
import { useStatus } from "@/lib/status";
import type { DeviceStatus, DeviceUsage, Devices as DevicesData, FormFactor } from "@/lib/types";
import { deviceTone } from "./Overview";
import { ConnectServer } from "./Pairing";

export function Devices() {
  const { query, timeZone } = useData();
  const { status } = useStatus();
  const connected = Boolean(status?.server);
  const devices = useDashboard<DevicesData>("devices", query());
  const [adding, setAdding] = useState(false);
  const data = devices.data;
  const max = Math.max(0, ...(data?.usage ?? []).map((usage) => usage.totalTokens));
  return (
    <div className="space-y-5">
      <div className="flex flex-wrap items-center gap-3">
        <p className="min-w-[200px] flex-1 text-xs leading-5 text-ink-3">
          {connected
            ? "这里汇总本机与同一空间的其他设备，本机记录和服务器同步会继续运行。"
            : "正在记录这台电脑的用量。想合并查看其他电脑，点击「添加设备」连接服务器，本机记录会继续运行。"}
        </p>
        <Button variant="primary" onClick={() => setAdding(true)}>
          <Plus className="h-4 w-4" aria-hidden /> 添加设备
        </Button>
      </div>
      {devices.error ? <LoadError error={devices.error} onRetry={devices.reload} compact /> : null}
      {!data ? (
        !devices.error && <div className="grid grid-cols-1 gap-4 min-[1100px]:grid-cols-2">
          {[0, 1].map((i) => <Skeleton key={i} className="h-56 rounded-[var(--radius-card)]" />)}
        </div>
      ) : (
        <>
          <CaveatList caveats={data.caveats} />
          {data.devices.length === 0 ? (
            <Card><EmptyState title="还没有设备" hint="本机数据正在准备；连接服务器后还可以添加其他电脑。" /></Card>
          ) : (
            <div className="grid grid-cols-1 gap-4 min-[1100px]:grid-cols-2">
              {data.devices.map((device) => (
                <DeviceCard
                  key={device.deviceId}
                  device={device}
                  usage={data.usage.find((item) => item.deviceId === device.deviceId)}
                  max={max}
                  minCollectorVersion={data.minCollectorVersion}
                  timeZone={timeZone}
                  connected={connected}
                  onChanged={devices.reload}
                />
              ))}
            </div>
          )}
          <Coverage data={data} timeZone={timeZone} />
        </>
      )}
      {connected
        ? <AddDevice open={adding} onOpenChange={setAdding} onDone={devices.reload} />
        : <ConnectServer open={adding} onOpenChange={setAdding} />}
    </div>
  );
}

function DeviceCard({
  device,
  usage,
  max,
  minCollectorVersion,
  timeZone,
  connected,
  onChanged,
}: {
  device: DeviceStatus;
  usage: DeviceUsage | undefined;
  max: number;
  minCollectorVersion: string;
  timeZone: string;
  /** Controls cloud credentials; never selects which dashboard is shown. */
  connected: boolean;
  onChanged: () => void;
}) {
  const tone = deviceTone(device);
  const [dialog, setDialog] = useState<"rename" | "form-factor" | "rotate" | "revoke" | null>(null);
  const kind = [platformLabel(device.platform), formFactorLabel(device.formFactor)].filter(Boolean).join(" ");
  const os = device.osName ? `${device.osName}${device.osVersion ? ` ${device.osVersion}` : ""}` : null;
  const clients = (usage?.byClient ?? []).filter((slice) => slice.totalTokens > 0);
  const total = usage?.totalTokens ?? 0;
  return (
    <Card
      title={
        <span className="flex min-w-0 items-center gap-2">
          <StatusDot tone={tone.tone} />
          <span className="truncate">{device.displayName}</span>
          {device.isCurrent && <Badge tone="primary">本机</Badge>}
          {device.outdated && (
            <Tip content={`采集端版本低于服务器要求的 ${minCollectorVersion}，请更新这台电脑上的应用`}>
              <span>
                <Badge tone="warn">需要升级</Badge>
              </span>
            </Tip>
          )}
        </span>
      }
      subtitle={[kind, os, tone.label].filter(Boolean).join(" · ")}
      action={<DeviceMenu device={device} connected={connected} onPick={setDialog} />}
    >
      <div className="flex items-end justify-between gap-3">
        <div>
          <div className="text-xs text-ink-3">这个范围内</div>
          <Num value={total} className="display text-2xl text-ink" />
          <span className="ml-1 text-xs text-ink-3">tokens</span>
        </div>
        <span className={cn("text-xs", device.backfillComplete ? "text-ink-3" : "text-[color-mix(in_oklch,var(--warn)_60%,var(--ink))]")}>
          {device.backfillComplete
            ? "历史已补齐"
            : `${!connected && device.isCurrent ? "历史整理中" : "历史上传中"}${device.outboxBacklog ? `，还剩 ${compact(device.outboxBacklog)} 条` : ""}`}
        </span>
      </div>
      <ShareBar value={total} max={max} color="var(--primary)" className="mt-2" />
      {clients.length > 0 && (
        <ul className="mt-3 space-y-1.5">
          {clients.slice(0, 5).map((slice) => (
            <li key={slice.key} className="grid grid-cols-[minmax(0,1fr)_auto] items-center gap-x-3 text-[13px]">
              <ClientName id={slice.key} label={slice.label} className="text-ink-2" />
              <Num value={slice.totalTokens} className="text-ink" />
              <ShareBar value={slice.totalTokens} max={total} color={clientColor(slice.key)} className="col-span-2 h-1" />
            </li>
          ))}
        </ul>
      )}
      <dl className="mt-4 grid grid-cols-2 gap-x-4 gap-y-2 border-t border-line/70 pt-3 text-xs min-[1300px]:grid-cols-3">
        <Info label="最后在线" value={relativeTime(device.lastSeenAt)} title={dateTime(device.lastSeenAt, timeZone)} />
        <Info label={!connected && device.isCurrent ? "最后写入" : "最后上传"} value={relativeTime(device.lastUploadAt)} title={dateTime(device.lastUploadAt, timeZone)} />
        <Info label="最后扫描" value={relativeTime(device.lastScanAt)} title={device.lastScanDurationMs !== null ? `用时 ${duration(device.lastScanDurationMs)}` : undefined} />
        <Info label="采集端版本" value={device.collectorVersion ?? "—"} />
        <Info label="解析库版本" value={device.tokscaleRev ? device.tokscaleRev.slice(0, 8) : "—"} title={device.tokscaleRev ?? undefined} mono />
        <Info label="配对于" value={dateTime(device.createdAt, timeZone, false, true)} />
      </dl>
      <RenameDialog open={dialog === "rename"} device={device} onClose={() => setDialog(null)} onDone={onChanged} />
      <FormFactorDialog open={dialog === "form-factor"} device={device} onClose={() => setDialog(null)} onDone={onChanged} />
      <RotateDialog open={dialog === "rotate"} onClose={() => setDialog(null)} onDone={onChanged} />
      {!connected && device.isCurrent ? (
        <LocalDeviceDialog open={dialog === "revoke"} onClose={() => setDialog(null)} onChanged={onChanged} />
      ) : (
        <RevokeDialog open={dialog === "revoke"} device={device} onClose={() => setDialog(null)} onDone={onChanged} />
      )}
    </Card>
  );
}

function Info({ label, value, title, mono }: { label: string; value: string; title?: string; mono?: boolean }) {
  return (
    <div className="min-w-0">
      <dt className="text-ink-3">{label}</dt>
      <Tip content={title}>
        <dd className={cn("mt-0.5 truncate text-ink-2", mono && "font-mono")}>{value}</dd>
      </Tip>
    </div>
  );
}

function DeviceMenu({
  device,
  connected,
  onPick,
}: {
  device: DeviceStatus;
  connected: boolean;
  onPick: (action: "rename" | "form-factor" | "rotate" | "revoke") => void;
}) {
  const [open, setOpen] = useState(false);
  const item = "flex w-full items-center gap-2 rounded-md px-2.5 py-1.5 text-left text-[13px] hover:bg-surface-2";
  const pick = (action: "rename" | "form-factor" | "rotate" | "revoke") => {
    setOpen(false);
    onPick(action);
  };
  return (
    <Popover open={open} onOpenChange={setOpen}>
      <PopoverTrigger asChild>
        <Button variant="ghost" size="icon" aria-label={`${device.displayName} 的操作`}>
          <MoreHorizontal className="h-4 w-4" />
        </Button>
      </PopoverTrigger>
      <PopoverContent className="w-52 p-1.5">
        <button type="button" className={item} onClick={() => pick("rename")}>
          <Pencil className="h-3.5 w-3.5 text-ink-3" aria-hidden /> 重命名
        </button>
        <button type="button" className={item} onClick={() => pick("form-factor")}>
          <Laptop className="h-3.5 w-3.5 text-ink-3" aria-hidden /> 更改设备性质
        </button>
        {device.isCurrent && connected && (
          <button type="button" className={item} onClick={() => pick("rotate")}>
            <KeyRound className="h-3.5 w-3.5 text-ink-3" aria-hidden /> 更换这台电脑的凭据
          </button>
        )}
        <button type="button" className={cn(item, "text-bad")} onClick={() => pick("revoke")}>
          <ShieldOff className="h-3.5 w-3.5" aria-hidden />
          移除设备
        </button>
      </PopoverContent>
    </Popover>
  );
}

function LocalDeviceDialog({ open, onClose, onChanged }: { open: boolean; onClose: () => void; onChanged: () => void }) {
  const { set } = useStatus();
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const confirm = async () => {
    setBusy(true);
    setError(null);
    try {
      set(await app.removeLocalDevice());
      onChanged();
      onClose();
    } catch (reason) {
      setError(errorMessage(reason));
    } finally {
      setBusy(false);
    }
  };
  return (
    <Dialog open={open} onOpenChange={(next) => !busy && !next && onClose()} title="移除本机设备？">
      <div className="space-y-2 text-[13px] leading-6 text-ink-2">
        <p>这台电脑及其记录将从本机看板移除，采集会暂停。</p>
        <p className="rounded-lg bg-bad/10 px-3 py-2 text-bad">重新开始并恢复采集后，会扫描现有日志，历史用量可能再次出现。已同步到其他设备或服务器的记录不受影响。</p>
      </div>
      {error && <p role="alert" className="mt-3 text-xs text-bad">{error}</p>}
      <div className="mt-5 flex justify-end gap-2">
        <Button onClick={onClose} disabled={busy}>取消</Button>
        <Button variant="danger" onClick={() => void confirm()} disabled={busy}>
          {busy ? "移除中…" : "移除"}
        </Button>
      </div>
    </Dialog>
  );
}

// eslint-disable-next-line no-control-regex
const CONTROL = /[\u0000-\u001f\u007f]/;

export function validDisplayName(name: string): boolean {
  const trimmed = name.trim();
  return trimmed.length >= 1 && [...trimmed].length <= 64 && !CONTROL.test(trimmed);
}

function RenameDialog({ open, device, onClose, onDone }: { open: boolean; device: DeviceStatus; onClose: () => void; onDone: () => void }) {
  const [name, setName] = useState(device.displayName);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  useEffect(() => {
    if (open) {
      setName(device.displayName);
      setError(null);
    }
  }, [open, device.displayName]);
  const submit = () => {
    if (!validDisplayName(name)) {
      setError("名称需要 1–64 个字符。");
      return;
    }
    setBusy(true);
    app
      .renameDevice(device.deviceId, name.trim())
      .then(() => {
        onDone();
        onClose();
      })
      .catch((reason: unknown) => setError(errorMessage(reason)))
      .finally(() => setBusy(false));
  };
  return (
    <Dialog open={open} onOpenChange={(next) => !next && onClose()} title="重命名设备">
      <form
        onSubmit={(event) => {
          event.preventDefault();
          submit();
        }}
        className="space-y-4"
      >
        <Field label="显示名称" hint="只用于看板里区分设备">
          <input value={name} onChange={(event) => setName(event.target.value)} maxLength={64} autoFocus className={inputClass} />
        </Field>
        {error && <p className="text-xs text-bad">{error}</p>}
        <div className="flex justify-end gap-2">
          <Button onClick={onClose}>取消</Button>
          <Button type="submit" variant="primary" disabled={busy}>
            保存
          </Button>
        </div>
      </form>
    </Dialog>
  );
}

function FormFactorDialog({ open, device, onClose, onDone }: { open: boolean; device: DeviceStatus; onClose: () => void; onDone: () => void }) {
  const [formFactor, setFormFactor] = useState<FormFactor | "">("");
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  useEffect(() => {
    if (open) {
      setFormFactor(device.formFactor === "laptop" || device.formFactor === "desktop" ? device.formFactor : "");
      setError(null);
    }
  }, [open, device.formFactor]);
  const submit = async () => {
    if (!formFactor || busy) return;
    setBusy(true);
    setError(null);
    try {
      await app.setDeviceFormFactor(device.deviceId, formFactor);
      onDone();
      onClose();
    } catch (reason) {
      setError(errorMessage(reason));
    } finally {
      setBusy(false);
    }
  };
  return (
    <Dialog
      open={open}
      onOpenChange={(next) => {
        if (!next && !busy) onClose();
      }}
      title={`更改「${device.displayName}」的设备性质`}
    >
      <form
        onSubmit={(event) => {
          event.preventDefault();
          void submit();
        }}
        className="space-y-4"
      >
        <div>
          <span className="text-[13px] font-medium text-ink">设备性质</span>
          <div className="mt-1.5">
            <Segmented label="设备性质" value={formFactor} onChange={setFormFactor} options={[{ key: "laptop", label: "笔记本" }, { key: "desktop", label: "台式机" }]} />
          </div>
          <p className="mt-2 text-xs leading-5 text-ink-3">只修改看板里的设备资料，不影响设备名称、凭据和历史用量。</p>
        </div>
        {error && <p role="alert" className="text-xs text-bad">{error}</p>}
        <div className="flex justify-end gap-2">
          <Button onClick={onClose} disabled={busy}>取消</Button>
          <Button type="submit" variant="primary" disabled={busy || !formFactor}>
            {busy ? "保存中…" : "保存"}
          </Button>
        </div>
      </form>
    </Dialog>
  );
}

function RotateDialog({ open, onClose, onDone }: { open: boolean; onClose: () => void; onDone: () => void }) {
  const { set } = useStatus();
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const confirm = () => {
    setBusy(true);
    setError(null);
    app
      .rotateCredentials()
      .then((status) => {
        set(status);
        onDone();
        onClose();
      })
      .catch((reason: unknown) => setError(errorMessage(reason)))
      .finally(() => setBusy(false));
  };
  return (
    <Dialog open={open} onOpenChange={(next) => !next && onClose()} title="更换这台电脑的凭据">
      <p className="text-[13px] leading-6 text-ink-2">
        凭据是这台电脑连接服务器时使用的身份钥匙，不是账号密码，也不是服务器地址。更换后旧钥匙立即失效，新钥匙自动保存；历史用量不变，采集和同步继续。一般只在怀疑钥匙泄露时使用。
      </p>
      {error && <p className="mt-3 text-xs text-bad">{error}</p>}
      <div className="mt-5 flex justify-end gap-2">
        <Button onClick={onClose}>取消</Button>
        <Button variant="primary" onClick={confirm} disabled={busy}>
          {busy ? "更换中…" : "更换"}
        </Button>
      </div>
    </Dialog>
  );
}

function RevokeDialog({ open, device, onClose, onDone }: { open: boolean; device: DeviceStatus; onClose: () => void; onDone: () => void }) {
  const { set } = useStatus();
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const confirm = () => {
    setBusy(true);
    setError(null);
    app
      .revokeDevice(device.deviceId)
      .then((status) => {
        set(status);
        onDone();
        onClose();
      })
      .catch((reason: unknown) => setError(errorMessage(reason)))
      .finally(() => setBusy(false));
  };
  return (
    <Dialog open={open} onOpenChange={(next) => !next && onClose()} title={`移除「${device.displayName}」`}>
      <div className="space-y-2 text-[13px] leading-6 text-ink-2">
        <p>这台设备会停止服务器同步，也不能再查看这个服务器上的看板。本机记录继续运行，已上传的历史用量保留。</p>
        {device.isCurrent ? (
          <p className="rounded-lg bg-bad/10 px-3 py-2 text-bad">这是你现在正在用的电脑。移除后会自动查看本机数据；以后需要新的配对码才能重新连接服务器。</p>
        ) : (
          <p>以后想再加入，需要在那台电脑上用新的配对码重新配对。</p>
        )}
      </div>
      {error && <p className="mt-3 text-xs text-bad">{error}</p>}
      <div className="mt-5 flex justify-end gap-2">
        <Button onClick={onClose}>取消</Button>
        <Button variant="danger" onClick={confirm} disabled={busy}>
          {busy ? "移除中…" : "移除"}
        </Button>
      </div>
    </Dialog>
  );
}

function AddDevice({ open, onOpenChange, onDone }: { open: boolean; onOpenChange: (open: boolean) => void; onDone: () => void }) {
  const { status } = useStatus();
  const signedIn = Boolean(status?.accountEmail);
  const [code, setCode] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const [copied, setCopied] = useState(false);
  const [signingIn, setSigningIn] = useState(false);
  useEffect(() => {
    if (!open) {
      setCode(null);
      setError(null);
      setCopied(false);
      return;
    }
    if (!signedIn) return;
    let alive = true;
    app
      .accountPairCode(false)
      .then((next) => alive && setCode(next.code))
      .catch((reason: unknown) => alive && setError(errorMessage(reason)));
    return () => {
      alive = false;
    };
  }, [open, signedIn]);
  const rotate = async () => {
    setBusy(true);
    setError(null);
    try {
      const next = await app.accountPairCode(true);
      setCode(next.code);
      setCopied(false);
    } catch (reason) {
      setError(errorMessage(reason));
    } finally {
      setBusy(false);
    }
  };
  const copy = async () => {
    if (!code) return;
    try {
      await copyText(code);
      setCopied(true);
      setError(null);
    } catch (reason) {
      setError(errorMessage(reason));
    }
  };
  const close = (next: boolean) => {
    onOpenChange(next);
    if (!next) onDone();
  };
  return (
    <Dialog open={open} onOpenChange={close} title="添加设备">
      <ol className="list-decimal space-y-1 pl-5 text-[13px] leading-6 text-ink-2">
        <li>在另一台电脑上安装并打开 TokenLens。</li>
        <li>选择「TokenLens 云服务」，用同一个邮箱登录。</li>
        <li>也可以在「连接服务器」里填入下面的服务器地址和配对码。</li>
      </ol>
      <div className="mt-4">
        <Field label="服务器地址">
          <input className={cn(inputClass, "selectable font-mono text-xs")} readOnly value={status?.server ?? status?.cloudServer ?? ""} />
        </Field>
      </div>
      {!signedIn ? (
        <div className="mt-4 space-y-3 rounded-xl bg-surface-2 px-4 py-4">
          <p className="text-[13px] leading-6 text-ink-2">这个空间的配对码需要在登录后查看。登录后多台电脑共用同一个看板。</p>
          <Button variant="primary" onClick={() => setSigningIn(true)}>登录查看配对码</Button>
        </div>
      ) : (
        <>
          <div className="mt-4 rounded-xl bg-surface-2 px-4 py-5 text-center">
            {error ? (
              <p className="text-sm text-bad">{error}</p>
            ) : !code ? (
              <Skeleton className="mx-auto h-9 w-48" />
            ) : (
              <>
                <div className="selectable font-mono text-3xl font-semibold tracking-[0.2em] text-ink">{code}</div>
                <div className="mt-2 text-xs text-ink-3">一直有效；每用一次只添加一台电脑。泄露时可以换一个。</div>
              </>
            )}
          </div>
          <div className="mt-3 flex flex-wrap gap-2">
            <Button size="sm" onClick={() => void copy()} disabled={!code}>
              {copied ? <Check className="h-3.5 w-3.5" aria-hidden /> : <Copy className="h-3.5 w-3.5" aria-hidden />}
              {copied ? "已复制" : "复制配对码"}
            </Button>
            <Button size="sm" variant="danger" onClick={() => void rotate()} disabled={busy || !code}>
              {busy ? "正在更换…" : "换一个配对码"}
            </Button>
          </div>
        </>
      )}
      <div className="mt-5 flex justify-end gap-2">
        <Button variant="primary" onClick={() => close(false)}>完成</Button>
      </div>
      <ConnectServer open={signingIn} onOpenChange={(next) => { setSigningIn(next); if (!next) onDone(); }} />
    </Dialog>
  );
}

function Coverage({ data, timeZone }: { data: DevicesData; timeZone: string }) {
  const { used, unused } = useMemo(() => coverageColumns(data), [data]);
  const columns = [...used, ...unused];
  if (!data.devices.length || !columns.length) return null;
  // The first unused column gets a divider so the two groups read apart.
  const divider = (client: string) => used.length > 0 && client === unused[0] && "border-l border-line";
  const sticky = "sticky left-0 z-10 bg-surface";
  return (
    <Card title="Agent 覆盖" subtitle="装过或用过的 agent 排在最前；其余按热度排序。「—」表示这台电脑上没有装或没用过">
      <div className="overflow-x-auto">
        <table className="w-full text-[13px]">
          <thead className="border-b border-line">
            <tr>
              <th rowSpan={2} className={cn(sticky, "py-2 pr-3 text-left align-bottom text-xs font-medium text-ink-3")}>
                设备
              </th>
              {used.length > 0 && (
                <th colSpan={used.length} className="px-2 pt-1 text-left text-[11px] font-normal whitespace-nowrap text-ink-3">
                  已装或用过 · {used.length}
                </th>
              )}
              {unused.length > 0 && (
                <th colSpan={unused.length} className={cn("px-2 pt-1 text-left text-[11px] font-normal whitespace-nowrap text-ink-3", used.length > 0 && "border-l border-line")}>
                  未发现 · 按热度 · {unused.length}
                </th>
              )}
            </tr>
            <tr>
              {columns.map((client) => (
                <th key={client} className={cn("px-2 py-2 text-left text-xs font-medium whitespace-nowrap text-ink-3", divider(client))}>
                  <ClientName id={client} className={used.includes(client) ? "text-ink-2" : "text-ink-3"} />
                </th>
              ))}
            </tr>
          </thead>
          <tbody>
            {data.devices.map((device) => (
              <tr key={device.deviceId} className="border-b border-line/60 last:border-0">
                <td className={cn(sticky, "max-w-[180px] truncate py-2 pr-3 text-ink")}>{device.displayName}</td>
                {columns.map((client) => {
                  const coverage = device.clients.find((item) => item.client === client);
                  return (
                    <td key={client} className={cn("px-2 py-2 whitespace-nowrap", divider(client))}>
                      <CoverageCell coverage={coverage} timeZone={timeZone} />
                    </td>
                  );
                })}
              </tr>
            ))}
          </tbody>
        </table>
      </div>
    </Card>
  );
}

function CoverageCell({ coverage, timeZone }: { coverage: DeviceStatus["clients"][number] | undefined; timeZone: string }) {
  if (!coverage || !coverage.detected) return <span className="text-ink-3">—</span>;
  if (coverage.errorCode) {
    return (
      <Tip content={`读取出错（${coverage.errorCode}），更新于 ${dateTime(coverage.updatedAt, timeZone)}`}>
        <span className="font-mono text-xs text-bad">{shortId(coverage.errorCode, 14)}</span>
      </Tip>
    );
  }
  return (
    <Tip
      content={`最后一条用量 ${dateTime(coverage.lastEventAt, timeZone)}${coverage.lastScanEvents !== undefined ? ` · 最近一次扫描 ${exact(coverage.lastScanEvents)} 条` : ""}`}
    >
      <span className="inline-flex items-center gap-1 text-xs text-ink-2">
        <Check className="h-3.5 w-3.5 text-good" aria-hidden />
        {coverage.lastEventAt ? relativeTime(coverage.lastEventAt) : "已发现"}
      </span>
    </Tip>
  );
}
