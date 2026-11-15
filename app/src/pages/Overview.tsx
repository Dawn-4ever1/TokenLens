import { ChevronRight } from "lucide-react";
import { useCallback, useMemo, useState, type ReactNode } from "react";
import { Chart, baseOption, type ChartOption } from "@/components/Chart";
import { Composition } from "@/components/Composition";
import { ClientName, LoadError, ModelName, Num, PrecisionBadge, ShareBar } from "@/components/common";
import { Badge, Button, Card, EmptyState, Segmented, Skeleton, StatusDot, Tip, cn } from "@/components/ui";
import type { Palette } from "@/lib/colors";
import { useData, useDashboard } from "@/lib/data";
import { bucketLabel, compact, exact, percent, relativeTime, share, usd } from "@/lib/format";
import { clientColor, formFactorLabel, OTHER_KEY, platformLabel } from "@/lib/labels";
import { useNav } from "@/lib/nav";
import { daysBetween, resolveRange, todayIn, type DateSpan } from "@/lib/range";
import type { Bucket, DeviceStatus, Devices, Point, SessionPage, Summary, Timeseries } from "@/lib/types";

export function autoBucket(span: DateSpan, firstEventDate: string | undefined, today: string): Bucket {
  const from = span.from ?? firstEventDate ?? today;
  const to = span.to ?? today;
  const days = daysBetween(from, to) + 1;
  if (days <= 2) return "hour";
  if (days <= 120) return "day";
  if (days <= 800) return "week";
  return "month";
}

export function Overview() {
  const { filters, timeZone, query, options } = useData();
  const span = resolveRange(filters, timeZone);
  const q = query();
  const summary = useDashboard<Summary>("summary", q);
  const bucket = autoBucket(span, options?.firstEventDate, todayIn(timeZone));
  const trend = useDashboard<Timeseries>("timeseries", { ...q, bucket, groupBy: "none" });
  const recent = useDashboard<SessionPage>("sessions", { ...q, sort: "recent", limit: "5" });
  const devices = useDashboard<Devices>("devices", q);

  if (!summary.data) {
    if (summary.error) return <LoadError error={summary.error} onRetry={summary.reload} />;
    return <OverviewSkeleton />;
  }
  const s = summary.data;
  const empty = s.messageCount === 0;

  return (
    <div className="space-y-5">
      {summary.error ? <LoadError error={summary.error} onRetry={summary.reload} compact /> : null}
      <div data-testid="overview-kpis" className="grid grid-cols-2 gap-4 min-[1000px]:grid-cols-4">
        <Kpi label="Tokens" value={<Num value={s.totalTokens} />} />
        <Kpi label="成本" value={<span className="num">{usd(s.costUsd)}</span>} />
        <Kpi label="缓存命中率" value={<span className="num">{percent(s.cacheHitRate)}</span>} />
        <Kpi label="会话" value={<span className="num">{exact(s.sessionCount)}</span>} />
      </div>

      {empty ? (
        <Card>
          <EmptyState title="这个时间范围内没有用量" hint="可以换一个时间范围，或清除筛选条件。如果刚配对，第一次上传可能需要几分钟。" />
        </Card>
      ) : (
        <>
          <div className="grid grid-cols-1 gap-4 min-[1100px]:grid-cols-2">
            <ToolCard summary={s} />
            <ModelCard summary={s} />
          </div>
          <TrendCard data={trend.data} error={trend.error} reload={trend.reload} bucket={bucket} />
        </>
      )}
      <Card title="Token 构成" action={<PrecisionBadge precision={s.precision} />}>
        <Composition value={s} />
      </Card>
      <div className="grid grid-cols-1 gap-4 min-[1100px]:grid-cols-2">
        <RecentCard page={recent.data} error={recent.error} reload={recent.reload} />
        <DeviceStrip data={devices.data} error={devices.error} reload={devices.reload} />
      </div>
    </div>
  );
}

function Kpi({ label, value }: { label: string; value: ReactNode }) {
  return (
    <section className="card min-w-0 px-5 py-5">
      <h2 className="truncate text-sm font-semibold text-ink">{label}</h2>
      <div className="display num-fit mt-3 text-[32px] leading-10 text-ink min-[1300px]:text-[36px]">{value}</div>
    </section>
  );
}

type TrendMode = "total" | "nonCache" | "cost";

function pointValue(point: Point, mode: TrendMode): number {
  switch (mode) {
    case "total":
      return point.totalTokens;
    case "nonCache":
      return point.inputTokens + point.outputTokens + point.reasoningTokens;
    case "cost":
      return point.costUsd;
  }
}

function TrendCard({ data, error, reload, bucket }: { data: Timeseries | null; error: unknown; reload: () => void; bucket: Bucket }) {
  const [mode, setMode] = useState<TrendMode>("total");
  const values = useMemo(() => {
    if (!data) return [];
    return data.buckets.map((_, index) => data.series.reduce((sum, series) => sum + (series.points[index] ? pointValue(series.points[index], mode) : 0), 0));
  }, [data, mode]);
  const option = useCallback(
    (palette: Palette): ChartOption => {
      const format = mode === "cost" ? (v: number) => usd(v) : (v: number) => compact(v);
      const formatExact = mode === "cost" ? (v: number) => usd(v) : (v: number) => exact(v);
      const kind = data?.bucket ?? bucket;
      return {
        ...baseOption(palette),
        grid: { left: 4, right: 12, top: 12, bottom: 4, containLabel: true },
        xAxis: {
          type: "category",
          boundaryGap: false,
          data: data?.buckets ?? [],
          axisLine: { lineStyle: { color: palette.border } },
          axisTick: { show: false },
          axisLabel: { color: palette.ink3, hideOverlap: true, formatter: (value: string) => (kind === "hour" ? `${value.slice(11, 13)}时` : bucketLabel(value, kind).replace("起一周", "")) },
        },
        yAxis: { type: "value", splitLine: { lineStyle: { color: palette.grid } }, axisLabel: { color: palette.ink3, formatter: format } },
        tooltip: {
          ...(baseOption(palette).tooltip as object),
          trigger: "axis",
          formatter: (params: { dataIndex: number; value: number }[]) => {
            const first = params[0];
            if (!first || !data) return "";
            const name = bucketLabel(data.buckets[first.dataIndex] ?? "", kind);
            return `${name}<br/><b>${formatExact(first.value)}</b>`;
          },
        },
        series: [
          {
            type: "line",
            data: values,
            smooth: 0.25,
            smoothMonotone: "x",
            symbol: "none",
            lineStyle: { width: 2, color: palette.primary },
            itemStyle: { color: palette.primary },
            areaStyle: {
              color: {
                type: "linear", x: 0, y: 0, x2: 0, y2: 1,
                colorStops: [
                  { offset: 0, color: palette.resolve("color-mix(in oklch, var(--primary) 28%, transparent)") },
                  { offset: 1, color: palette.resolve("color-mix(in oklch, var(--primary) 2%, transparent)") },
                ],
              },
            },
          },
        ],
      };
    },
    [data, values, mode, bucket],
  );
  const unit = { hour: "每小时", day: "每天", week: "每周", month: "每月" }[data?.bucket ?? bucket];
  return (
    <Card
      bodyClassName="flex flex-col"
      title="趋势"
      subtitle={`${unit}${mode === "cost" ? "成本（API 等价估算）" : mode === "nonCache" ? "非缓存 tokens（输入 + 输出 + 推理）" : "总 tokens"}`}
      action={
        <Segmented
          size="sm"
          label="趋势指标"
          value={mode}
          onChange={setMode}
          options={[
            { key: "total", label: "总量" },
            { key: "nonCache", label: "非缓存" },
            { key: "cost", label: "成本" },
          ]}
        />
      }
    >
      {!data && error ? (
        <LoadError error={error} onRetry={reload} compact />
      ) : !data ? (
        <Skeleton className="h-[260px]" />
      ) : (
        <div className="relative min-h-[260px] flex-1">
          <Chart option={option} height="100%" className="absolute inset-0" label="用量趋势面积图" />
        </div>
      )}
    </Card>
  );
}

function ToolCard({ summary }: { summary: Summary }) {
  const { go } = useNav();
  const rows = summary.byClient.slice(0, 8);
  const max = Math.max(0, ...rows.map((row) => row.totalTokens));
  return (
    <Card title="工具" subtitle="点击查看该工具的会话">
      {rows.length === 0 ? (
        <EmptyState title="没有工具数据" />
      ) : (
        <ul className="-mx-2 grid grid-cols-1 gap-x-6 gap-y-0.5 min-[700px]:grid-cols-2">
          {rows.map((row) => {
            const clickable = row.key !== OTHER_KEY;
            return (
              <li key={row.key}>
                <button
                  type="button"
                  disabled={!clickable}
                  onClick={() => go("sessions", { clients: [row.key] })}
                  className="group w-full rounded-lg px-2 py-1.5 text-left hover:bg-surface-2 disabled:hover:bg-transparent"
                >
                  <div className="flex items-center justify-between gap-2 text-[13px]">
                    <ClientName id={row.key} label={row.label} className="text-ink" />
                    <span className="flex shrink-0 items-center gap-2">
                      <PrecisionBadge precision={row.precision} />
                      <Num value={row.totalTokens} plain className="font-medium text-ink" />
                    </span>
                  </div>
                  <div className="mt-1.5 flex items-center gap-2">
                    <ShareBar value={row.totalTokens} max={max} color={clientColor(row.key)} />
                    <span className="num w-12 shrink-0 text-right text-[11px] text-ink-3">{percent(share(row.totalTokens, summary.totalTokens), 0)}</span>
                    <span className="num w-14 shrink-0 text-right text-[11px] text-ink-3">{usd(row.costUsd)}</span>
                  </div>
                </button>
              </li>
            );
          })}
        </ul>
      )}
    </Card>
  );
}

function ModelCard({ summary }: { summary: Summary }) {
  const rows = summary.byModel.slice(0, 8);
  const max = Math.max(0, ...rows.map((row) => row.totalTokens));
  return (
    <Card title="模型" subtitle={`前 ${rows.length} 个模型`} bodyClassName="px-0 pb-2">
      {rows.length === 0 ? (
        <EmptyState title="没有模型数据" />
      ) : (
        <table className="w-full table-fixed text-left text-[13px]">
          <thead className="text-xs text-ink-3">
            <tr>
              <th className="w-[38%] px-5 pb-2 font-medium">模型</th>
              <th className="px-2 pb-2 font-medium">Tokens</th>
              <th className="w-[16%] px-2 pb-2 text-right font-medium">成本</th>
              <th className="w-[18%] px-5 pb-2 text-right font-medium">缓存命中</th>
            </tr>
          </thead>
          <tbody>
            {rows.map((row) => (
              <tr key={row.key} className="border-t border-line/70">
                <td className="px-5 py-2">
                  <div className="flex min-w-0 items-center gap-1.5">
                    <Tip content={row.key}>
                      <ModelName id={row.key} className="text-ink" />
                    </Tip>
                    <PrecisionBadge precision={row.precision} />
                  </div>
                </td>
                <td className="px-2 py-2">
                  <div className="flex items-center gap-2">
                    <Num value={row.totalTokens} className="min-w-12 shrink-0 text-left text-ink" />
                    <ShareBar value={row.totalTokens} max={max} color="var(--primary)" />
                  </div>
                </td>
                <td className="num px-2 py-2 text-right text-ink-2">{usd(row.costUsd)}</td>
                <td className="num px-5 py-2 text-right text-ink-2">{percent(row.cacheHitRate)}</td>
              </tr>
            ))}
          </tbody>
        </table>
      )}
    </Card>
  );
}

function RecentCard({ page, error, reload }: { page: SessionPage | null; error: unknown; reload: () => void }) {
  const { go, openSession } = useNav();
  return (
    <Card
      title="最近会话"
      action={
        <Button variant="ghost" size="sm" onClick={() => go("sessions")}>
          全部 <ChevronRight className="h-3.5 w-3.5" aria-hidden />
        </Button>
      }
      bodyClassName="px-2 pb-2"
    >
      {!page && error ? (
        <LoadError error={error} onRetry={reload} compact />
      ) : !page ? (
        <div className="space-y-2 px-3 pb-3">
          {[0, 1, 2, 3, 4].map((i) => (
            <Skeleton key={i} className="h-10" />
          ))}
        </div>
      ) : page.sessions.length === 0 ? (
        <EmptyState title="没有会话" hint="当前时间范围和筛选条件下没有会话。" />
      ) : (
        <ul>
          {page.sessions.map((session) => (
            <li key={session.sessionKey}>
              <button type="button" onClick={() => openSession(session.sessionKey)} className="w-full rounded-lg px-3 py-2 text-left hover:bg-surface-2">
                <div className="flex items-center justify-between gap-3 text-[13px]">
                  <ClientName id={session.client} className="text-ink" />
                  <Num value={session.totalTokens} plain className="shrink-0 font-medium text-ink" />
                </div>
                <div className="mt-0.5 flex items-center justify-between gap-3 text-xs text-ink-3">
                  <span className="flex min-w-0 items-center gap-1 truncate">
                    {session.models[0] ? <ModelName id={session.models[0]} /> : "—"}
                    {session.models.length > 1 ? ` +${session.models.length - 1}` : ""}
                    {session.projectLabel ? ` · ${session.projectLabel}` : ""}
                    {session.subagentCount > 0 ? ` · ${session.subagentCount} 个子代理` : ""}
                  </span>
                  <span className="shrink-0">{relativeTime(session.lastUsedAt)}</span>
                </div>
              </button>
            </li>
          ))}
        </ul>
      )}
    </Card>
  );
}

export function deviceTone(device: DeviceStatus): { tone: "good" | "neutral" | "warn" | "bad"; label: string } {
  if (device.clients.some((client) => client.errorCode)) return { tone: "bad", label: "有 agent 读取出错" };
  if (device.connection === "online") return { tone: "good", label: "在线" };
  if (device.connection === "long_offline") return { tone: "warn", label: "长期未上线" };
  return { tone: "neutral", label: "离线" };
}

function DeviceStrip({ data, error, reload }: { data: Devices | null; error: unknown; reload: () => void }) {
  const { go } = useNav();
  return (
    <Card
      title="设备"
      subtitle="台式机关机、笔记本合盖时显示离线是正常的，开机后会自动补传"
      action={
        <Button variant="ghost" size="sm" onClick={() => go("devices")}>
          管理 <ChevronRight className="h-3.5 w-3.5" aria-hidden />
        </Button>
      }
    >
      {!data && error ? (
        <LoadError error={error} onRetry={reload} compact />
      ) : !data ? (
        <div className="grid grid-cols-1 gap-3 min-[650px]:grid-cols-2">
          {[0, 1, 2, 3].map((i) => (
            <Skeleton key={i} className="h-24" />
          ))}
        </div>
      ) : (
        <div className="grid grid-cols-1 gap-3 min-[650px]:grid-cols-2">
          {data.devices.map((device) => {
            const usage = data.usage.find((item) => item.deviceId === device.deviceId);
            const tone = deviceTone(device);
            return (
              <button
                key={device.deviceId}
                type="button"
                onClick={() => go("devices")}
                className="min-w-0 rounded-xl border border-line px-4 py-3 text-left hover:bg-surface-2"
              >
                <div className="flex items-center gap-2">
                  <StatusDot tone={tone.tone} />
                  <span className="truncate text-[13px] font-medium text-ink">{device.displayName}</span>
                  {device.isCurrent && <Badge tone="primary">本机</Badge>}
                </div>
                <div className="mt-1 truncate text-xs text-ink-3">
                  {platformLabel(device.platform)} {formFactorLabel(device.formFactor)} · {tone.label}
                  {device.connection !== "online" ? ` · ${relativeTime(device.lastSeenAt)}` : ""}
                </div>
                <div className="mt-2 flex items-end justify-between gap-2">
                  <Num value={usage?.totalTokens ?? 0} plain className="text-base font-semibold text-ink" />
                  <span className={cn("text-[11px]", device.backfillComplete ? "text-ink-3" : "text-[color-mix(in_oklch,var(--warn)_60%,var(--ink))]")}>
                    {device.backfillComplete
                      ? "历史已补齐"
                      : `历史上传中${device.outboxBacklog ? `（${compact(device.outboxBacklog)}）` : ""}`}
                  </span>
                </div>
              </button>
            );
          })}
        </div>
      )}
    </Card>
  );
}

function OverviewSkeleton() {
  return (
    <div className="space-y-5" aria-busy>
      <div className="grid grid-cols-2 gap-4 min-[1000px]:grid-cols-4">
        {[0, 1, 2, 3].map((i) => (
          <Skeleton key={i} className="h-[110px] rounded-[var(--radius-card)]" />
        ))}
      </div>
      <div className="grid grid-cols-1 gap-4 min-[1100px]:grid-cols-2">
        <Skeleton className="h-[340px] rounded-[var(--radius-card)]" />
        <Skeleton className="h-[340px] rounded-[var(--radius-card)]" />
      </div>
    </div>
  );
}
