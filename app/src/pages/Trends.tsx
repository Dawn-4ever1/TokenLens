import { useCallback, useMemo, useState } from "react";
import { Chart, baseOption, type ChartOption } from "@/components/Chart";
import { CaveatList, Delta, LoadError, Num } from "@/components/common";
import { Card, EmptyState, Segmented, Skeleton, Tip } from "@/components/ui";
import type { Palette } from "@/lib/colors";
import { useData, useDashboard } from "@/lib/data";
import { bucketLabel, compact, dayLabel, escapeHtml, exact, usd, weekdayLabel } from "@/lib/format";
import { clientLabel, modelLabel, NO_PROJECT_KEY, OTHER_KEY, seriesColor } from "@/lib/labels";
import { useNav } from "@/lib/nav";
import { addDays, daysBetween, resolveRange, todayIn, type DateSpan, type Filters, type RangePreset } from "@/lib/range";
import type { Bucket, GroupBy, Heatmap, Summary, Timeseries } from "@/lib/types";
import { autoBucket, isCurrentWeekDaily, weekCalendarDates, weeklyXAxisLabel } from "./Overview";

const BUCKETS: { key: Bucket; label: string }[] = [
  { key: "hour", label: "小时" },
  { key: "day", label: "日" },
  { key: "week", label: "周" },
  { key: "month", label: "月" },
];

const GROUPS: { key: GroupBy; label: string }[] = [
  { key: "none", label: "总量" },
  { key: "client", label: "工具" },
  { key: "model", label: "模型" },
  { key: "device", label: "设备" },
  { key: "project", label: "项目" },
];

type Metric = "tokens" | "cost";

export function seriesName(groupBy: GroupBy, key: string, label: string): string {
  if (key === OTHER_KEY) return "其他";
  switch (groupBy) {
    case "client":
      return clientLabel(key, label);
    case "model":
      return modelLabel(key);
    case "project":
      return key === NO_PROJECT_KEY ? "未标记项目" : label;
    case "none":
      return "总量";
    default:
      return label;
  }
}

/** Date span covered by one bucket label, for drilling into Sessions. */
export function bucketSpan(label: string, bucket: Bucket): DateSpan {
  switch (bucket) {
    case "hour":
    case "day":
      return { from: label.slice(0, 10), to: label.slice(0, 10) };
    case "week":
      return { from: label, to: addDays(label, 6) };
    case "month": {
      const [y, m] = label.split("-").map(Number);
      const last = new Date(Date.UTC(y!, m!, 0)).getUTCDate();
      return { from: `${label}-01`, to: `${label}-${String(last).padStart(2, "0")}` };
    }
  }
}

function spanDays(span: DateSpan, first: string | undefined, today: string): number {
  if (!span.from && !first) return 90;
  return Math.max(1, daysBetween(span.from ?? first ?? today, span.to ?? today) + 1);
}

export function Trends() {
  const { filters, timeZone, query, options } = useData();
  const today = todayIn(timeZone);
  const span = resolveRange(filters, timeZone);
  const days = spanDays(span, options?.firstEventDate, today);
  const fallback = autoBucket(span, options?.firstEventDate, today, filters.preset);
  const [chosen, setChosen] = useState<Bucket | null>(null);
  const allowed = (bucket: Bucket) => (bucket === "hour" ? days <= 62 : bucket === "day" ? days <= 4000 : true);
  const bucket = chosen && allowed(chosen) ? chosen : fallback;
  const [groupBy, setGroupBy] = useState<GroupBy>("client");
  const [metric, setMetric] = useState<Metric>("tokens");
  const q = query();
  const summary = useDashboard<Summary>("summary", q);
  const series = useDashboard<Timeseries>("timeseries", { ...q, bucket, groupBy, limit: "8" });
  const dailyAllowed = allowed("day");
  const daily = useDashboard<Timeseries>(dailyAllowed ? "timeseries" : null, { ...q, bucket: "day", groupBy: "none" });

  return (
    <div className="space-y-[var(--stack-space)]">
      {summary.data && <CaveatList caveats={summary.data.caveats.filter((c) => c.code !== "estimated_cost")} />}
      <Card
        title="用量趋势"
        subtitle={metric === "cost" ? "成本为 API 等价估算；未定价的用量不计入" : "按所选分组堆叠；超出前 8 的合并为「其他」"}
        action={
          <div className="flex flex-wrap items-center justify-end gap-1.5">
            <Segmented size="sm" label="指标" value={metric} onChange={setMetric} options={[{ key: "tokens", label: "Tokens" }, { key: "cost", label: "成本" }]} />
            <Segmented size="sm" label="分组" value={groupBy} onChange={setGroupBy} options={GROUPS} />
            <Segmented
              size="sm"
              label="粒度"
              value={bucket}
              onChange={(next) => allowed(next) && setChosen(next)}
              options={BUCKETS.map((b) => {
                const ok = allowed(b.key);
                const tip = b.key === "hour" ? "范围太长，不能按小时显示" : "范围太长，不能按日显示";
                return {
                  key: b.key,
                  disabled: !ok,
                  label: ok ? b.label : <Tip content={tip}><span>{b.label}</span></Tip>,
                };
              })}
            />
          </div>
        }
      >
        {!series.data && series.error ? (
          <LoadError error={series.error} onRetry={series.reload} compact />
        ) : !series.data ? (
          <Skeleton className="h-[320px]" />
        ) : series.data.series.length === 0 ? (
          <EmptyState title="这个范围内没有用量" hint="换一个时间范围，或清除筛选条件。" />
        ) : (
          <StackedChart data={series.data} metric={metric} preset={filters.preset} today={today} />
        )}
      </Card>
      <PeriodCompare />
      <div className="grid grid-cols-12 gap-[var(--grid-gap)]">
        <CalendarCard className="col-span-12 min-[1200px]:col-span-7" span={span} />
        <WeekHourCard className="col-span-12 min-[1200px]:col-span-5" />
      </div>
      <AnomalyCard data={daily.data} error={daily.error} reload={daily.reload} tooLong={!dailyAllowed} />
    </div>
  );
}

function StackedChart({ data, metric, preset, today }: { data: Timeseries; metric: Metric; preset: RangePreset; today: string }) {
  const { go } = useNav();
  const weekly = isCurrentWeekDaily(data, preset, today);
  const buckets = useMemo(() => (weekly ? weekCalendarDates(today) : data.buckets), [weekly, today, data.buckets]);
  const bucketIndex = useMemo(() => new Map(data.buckets.map((b, i) => [b, i])), [data.buckets]);
  const bars = weekly || buckets.length <= 45;
  const option = useCallback(
    (palette: Palette): ChartOption => {
      const value = (point: { totalTokens: number; costUsd: number } | undefined) => (point ? (metric === "cost" ? point.costUsd : point.totalTokens) : 0);
      const format = metric === "cost" ? (v: number) => usd(v) : (v: number) => compact(v);
      const formatExact = metric === "cost" ? (v: number) => usd(v) : (v: number) => exact(v);
      return {
        ...baseOption(palette),
        grid: { left: 4, right: 12, top: 36, bottom: 4, containLabel: true },
        legend: { type: "scroll", top: 0, left: 0, icon: "roundRect", itemWidth: 10, itemHeight: 10, textStyle: { color: palette.ink2 }, pageTextStyle: { color: palette.ink3 } },
        xAxis: {
          type: "category",
          boundaryGap: bars,
          data: buckets,
          axisLine: { lineStyle: { color: palette.border } },
          axisTick: { show: false },
          axisLabel: weekly
            ? weeklyXAxisLabel(palette, today)
            : {
                color: palette.ink3,
                hideOverlap: true,
                formatter: (v: string) => (data.bucket === "hour" ? `${dayLabel(v.slice(0, 10))} ${v.slice(11, 13)}时` : bucketLabel(v, data.bucket).replace("起一周", "")),
              },
        },
        yAxis: { type: "value", splitLine: { lineStyle: { color: palette.grid } }, axisLabel: { color: palette.ink3, formatter: format } },
        tooltip: {
          ...(baseOption(palette).tooltip as object),
          trigger: "axis",
          axisPointer: { type: bars ? "shadow" : "line" },
          formatter: (params: { dataIndex: number; value: number | null; seriesName: string; color: string }[]) => {
            const first = params[0];
            if (!first) return "";
            const key = buckets[first.dataIndex] ?? "";
            if (weekly) {
              const heading = `${escapeHtml(dayLabel(key))} ${escapeHtml(weekdayLabel(key))}${key === today ? "（今天）" : ""}`;
              if (!bucketIndex.has(key)) {
                return `${heading}<br/><span style="color:${palette.ink3}">尚未到达</span>`;
              }
              const numeric = params.filter((p): p is { dataIndex: number; value: number; seriesName: string; color: string } => typeof p.value === "number");
              const rows = numeric
                .filter((p) => p.value > 0)
                .sort((a, b) => b.value - a.value)
                .map((p) => `<div style="display:flex;justify-content:space-between;gap:16px"><span><span style="display:inline-block;width:8px;height:8px;border-radius:2px;background:${p.color};margin-right:6px"></span>${escapeHtml(p.seriesName)}</span><b>${formatExact(p.value)}</b></div>`)
                .join("");
              const total = numeric.reduce((sum, p) => sum + p.value, 0);
              return `<div style="min-width:180px">${heading}<div style="margin:4px 0 6px">合计 <b>${formatExact(total)}</b></div>${rows}</div>`;
            }
            const rows = params
              .filter((p): p is { dataIndex: number; value: number; seriesName: string; color: string } => typeof p.value === "number" && p.value > 0)
              .sort((a, b) => b.value - a.value)
              .map((p) => `<div style="display:flex;justify-content:space-between;gap:16px"><span><span style="display:inline-block;width:8px;height:8px;border-radius:2px;background:${p.color};margin-right:6px"></span>${escapeHtml(p.seriesName)}</span><b>${formatExact(p.value)}</b></div>`)
              .join("");
            const total = params.reduce((sum, p) => sum + (typeof p.value === "number" ? p.value : 0), 0);
            const heading = escapeHtml(bucketLabel(key, data.bucket));
            return `<div style="min-width:180px">${heading}<div style="margin:4px 0 6px">合计 <b>${formatExact(total)}</b></div>${rows}</div>`;
          },
        },
        series: data.series.map((s, index) => {
          const color = palette.resolve(seriesColor(data.groupBy, s.key, index));
          return {
            name: seriesName(data.groupBy, s.key, s.label),
            type: bars ? "bar" : "line",
            stack: "total",
            barMaxWidth: palette.barMaxWidth,
            symbol: "none",
            smooth: palette.direction === "paper" ? 0 : 0.2,
            smoothMonotone: "x",
            color,
            lineStyle: { width: palette.chartLineWidth },
            itemStyle: bars ? { borderRadius: palette.direction === "graphite" ? 1 : 0 } : undefined,
            areaStyle: bars ? undefined : { opacity: palette.chartAreaOpacity },
            emphasis: { focus: "series" },
            data: buckets.map((b) => {
              const idx = bucketIndex.get(b);
              return idx === undefined ? null : value(s.points[idx]);
            }),
          };
        }),
      };
    },
    [data, metric, bars, weekly, buckets, bucketIndex, today],
  );
  const onClick = useCallback(
    (params: { dataIndex: number }) => {
      const label = buckets[params.dataIndex];
      if (!label || (weekly && !bucketIndex.has(label))) return;
      const span = bucketSpan(label, data.bucket);
      const patch: Partial<Filters> = { preset: "custom", customFrom: span.from, customTo: span.to };
      go("sessions", patch);
    },
    [buckets, weekly, bucketIndex, data.bucket, go],
  );
  return <Chart option={option} height={320} onClick={onClick} label="按分组堆叠的用量趋势图，点击某个时间段查看当时的会话" />;
}

function weekStart(date: string): string {
  const [y, m, d] = date.split("-").map(Number);
  const weekday = new Date(Date.UTC(y!, m! - 1, d!)).getUTCDay();
  return addDays(date, -((weekday + 6) % 7));
}

function monthBack(date: string): string {
  const [y, m, d] = date.split("-").map(Number);
  const prevMonth = m === 1 ? 12 : m! - 1;
  const prevYear = m === 1 ? y! - 1 : y!;
  const last = new Date(Date.UTC(prevYear, prevMonth, 0)).getUTCDate();
  return `${prevYear}-${String(prevMonth).padStart(2, "0")}-${String(Math.min(d!, last)).padStart(2, "0")}`;
}

/** This week/month so far versus the same number of days in the previous one. */
export function comparisonSpans(today: string) {
  const week = weekStart(today);
  const elapsed = daysBetween(week, today);
  const month = `${today.slice(0, 8)}01`;
  const lastMonthStart = monthBack(month);
  return {
    thisWeek: { from: week, to: today },
    lastWeek: { from: addDays(week, -7), to: addDays(week, elapsed - 7) },
    thisMonth: { from: month, to: today },
    lastMonth: { from: lastMonthStart, to: monthBack(today) },
  };
}

function PeriodCompare() {
  const { timeZone, query } = useData();
  const spans = comparisonSpans(todayIn(timeZone));
  const base = query({ preset: "all" });
  const get = (span: DateSpan) => ({ ...base, from: span.from, to: span.to });
  const thisWeek = useDashboard<Summary>("summary", get(spans.thisWeek));
  const lastWeek = useDashboard<Summary>("summary", get(spans.lastWeek));
  const thisMonth = useDashboard<Summary>("summary", get(spans.thisMonth));
  const lastMonth = useDashboard<Summary>("summary", get(spans.lastMonth));
  const cell = (
    title: string,
    current: { data: Summary | null; error: unknown; reload: () => void },
    previous: Summary | null,
    hint: string,
  ) => (
    <div className="min-w-0">
      <div className="text-xs text-ink-3">{title}</div>
      {current.data ? (
        <>
          <div className="display num-fit mt-1 text-2xl text-ink">
            <Num value={current.data.totalTokens} />
          </div>
          <div className="mt-1 flex flex-wrap items-center gap-2 text-xs text-ink-3">
            <Delta current={current.data.totalTokens} previous={previous?.totalTokens} label={hint} />
            {previous && (
              <span>
                {hint.replace("相比", "")} <Num value={previous.totalTokens} className="text-ink-2" />
              </span>
            )}
          </div>
        </>
      ) : current.error ? (
        <LoadError error={current.error} onRetry={current.reload} compact />
      ) : (
        <Skeleton className="mt-2 h-12" />
      )}
    </div>
  );
  return (
    <Card title="周期对比" subtitle="和上一周期的同样天数相比，不受上方时间范围影响（其他筛选仍然生效）">
      <div className="grid grid-cols-2 gap-6">
        {cell("本周至今", thisWeek, lastWeek.data, "与上周同期相比")}
        {cell("本月至今", thisMonth, lastMonth.data, "与上月同期相比")}
      </div>
    </Card>
  );
}

function CalendarCard({ span, className }: { span: DateSpan; className?: string }) {
  const { timeZone, query, options } = useData();
  const { go } = useNav();
  const today = todayIn(timeZone);
  const to = span.to && span.to >= "2000-01-01" ? span.to : today;
  const earliest = addDays(to, -364);
  const latest = addDays(to, -181);
  let from = span.from ?? options?.firstEventDate ?? latest;
  if (from < earliest) from = earliest;
  if (from > latest) from = latest;
  if (from < "2000-01-01") from = "2000-01-01";
  if (from > to) from = to;
  const heat = useDashboard<Heatmap>("heatmap", { ...query(), from, to, kind: "calendar" });
  const option = useCallback(
    (palette: Palette): ChartOption => {
      const days = heat.data?.days ?? [];
      return {
        ...baseOption(palette),
        tooltip: {
          ...(baseOption(palette).tooltip as object),
          formatter: (p: { value: [string, number] }) => `${escapeHtml(dayLabel(p.value[0], true))}<br/><b>${exact(p.value[1])}</b> tokens`,
        },
        visualMap: { show: false, min: 0, max: Math.max(1, heat.data?.maxTotalTokens ?? 1), inRange: { color: [palette.primarySoft, palette.primary] } },
        calendar: {
          range: [from, to],
          top: 22,
          left: 28,
          right: 6,
          bottom: 20,
          cellSize: ["auto", 22],
          splitLine: { show: false },
          itemStyle: { color: palette.resolve("var(--surface-2)"), borderColor: palette.surface, borderWidth: 3 },
          dayLabel: { firstDay: 1, nameMap: ["日", "一", "二", "三", "四", "五", "六"], color: palette.ink3, fontSize: 10 },
          monthLabel: { nameMap: ["1月", "2月", "3月", "4月", "5月", "6月", "7月", "8月", "9月", "10月", "11月", "12月"], color: palette.ink3, fontSize: 10 },
          yearLabel: { show: false },
        },
        series: [{ type: "heatmap", coordinateSystem: "calendar", data: days.filter((d) => d.totalTokens > 0).map((d) => [d.date, d.totalTokens]) }],
      };
    },
    [heat.data, from, to],
  );
  return (
    <Card className={className} title="日历" subtitle={`${dayLabel(from, true)} – ${dayLabel(to, true)}，颜色越深用量越大；点击某天查看会话`}>
      {!heat.data && heat.error ? (
        <LoadError error={heat.error} onRetry={heat.reload} compact />
      ) : !heat.data ? (
        <Skeleton className="h-[200px]" />
      ) : (
        <Chart
          option={option}
          height={200}
          label="按天的用量日历热力图"
          onClick={(p) => {
            const value = p.data as [string, number] | undefined;
            if (value) go("sessions", { preset: "custom", customFrom: value[0], customTo: value[0] });
          }}
        />
      )}
    </Card>
  );
}

const WEEKDAYS = ["周一", "周二", "周三", "周四", "周五", "周六", "周日"];

function WeekHourCard({ className }: { className?: string }) {
  const { query } = useData();
  const heat = useDashboard<Heatmap>("heatmap", { ...query(), kind: "hour_weekday" });
  const option = useCallback(
    (palette: Palette): ChartOption => {
      const cells = heat.data?.cells ?? [];
      return {
        ...baseOption(palette),
        grid: { left: 4, right: 4, top: 4, bottom: 4, containLabel: true },
        tooltip: {
          ...(baseOption(palette).tooltip as object),
          formatter: (p: { value: [number, number, number] }) => `${WEEKDAYS[p.value[1]]} ${p.value[0]}:00–${p.value[0] + 1}:00<br/><b>${exact(p.value[2])}</b> tokens`,
        },
        visualMap: { show: false, min: 0, max: Math.max(1, heat.data?.maxTotalTokens ?? 1), inRange: { color: [palette.resolve("var(--surface-2)"), palette.primary] } },
        xAxis: { type: "category", data: Array.from({ length: 24 }, (_, h) => h), splitArea: { show: false }, axisLine: { show: false }, axisTick: { show: false }, axisLabel: { color: palette.ink3, fontSize: 10, interval: 2 } },
        yAxis: { type: "category", inverse: true, data: WEEKDAYS, axisLine: { show: false }, axisTick: { show: false }, axisLabel: { color: palette.ink3, fontSize: 10 } },
        series: [
          {
            type: "heatmap",
            data: cells.map((c) => [c.hour, c.weekday, c.totalTokens]),
            itemStyle: { borderColor: palette.surface, borderWidth: 2, borderRadius: palette.direction === "paper" ? 0 : palette.direction === "graphite" ? 1 : 3 },
            emphasis: { itemStyle: { borderColor: palette.ink2 } },
          },
        ],
      };
    },
    [heat.data],
  );
  return (
    <Card className={className} title="使用习惯" subtitle="星期 × 小时（看板时区）">
      {!heat.data && heat.error ? <LoadError error={heat.error} onRetry={heat.reload} compact /> : !heat.data ? <Skeleton className="h-[200px]" /> : <Chart option={option} height={200} label="星期乘小时的用量热力图" />}
    </Card>
  );
}

export interface Anomaly {
  date: string;
  totalTokens: number;
  ratio: number;
}

/** Days at least 3× the median active day (and 100K above it); needs 7+ active days. */
export function findAnomalies(dates: string[], totals: number[], limit = 5): { median: number; days: Anomaly[] } {
  const active = totals.filter((v) => v > 0).sort((a, b) => a - b);
  if (active.length < 7) return { median: 0, days: [] };
  const mid = Math.floor(active.length / 2);
  const median = active.length % 2 ? active[mid]! : (active[mid - 1]! + active[mid]!) / 2;
  const days = dates
    .map((date, i) => ({ date, totalTokens: totals[i] ?? 0, ratio: median > 0 ? (totals[i] ?? 0) / median : 0 }))
    .filter((d) => d.ratio >= 3 && d.totalTokens - median >= 100_000)
    .sort((a, b) => b.ratio - a.ratio)
    .slice(0, limit);
  return { median, days };
}

function AnomalyCard({
  data,
  error,
  reload,
  tooLong,
}: {
  data: Timeseries | null;
  error: unknown;
  reload: () => void;
  tooLong: boolean;
}) {
  const { go } = useNav();
  const result = useMemo(() => {
    if (!data) return null;
    const totals = data.buckets.map((_, i) => data.series.reduce((sum, s) => sum + (s.points[i]?.totalTokens ?? 0), 0));
    return findAnomalies(data.buckets, totals);
  }, [data]);
  return (
    <Card title="异常日" subtitle="用量至少是平时（有使用的日子的中位数）3 倍的日子">
      {tooLong ? (
        <p className="py-2 text-[13px] text-ink-3">时间范围超过 4000 天，暂不按日计算异常日。</p>
      ) : !data && error ? (
        <LoadError error={error} onRetry={reload} compact />
      ) : !result ? (
        <Skeleton className="h-16" />
      ) : result.days.length === 0 ? (
        <p className="py-2 text-[13px] text-ink-3">{result.median === 0 ? "有使用的天数不足 7 天，暂不判断。" : "这个范围内没有明显偏高的日子。"}</p>
      ) : (
        <ul className="divide-y divide-line/70">
          {result.days.map((day) => (
            <li key={day.date}>
              <button
                type="button"
                onClick={() => go("sessions", { preset: "custom", customFrom: day.date, customTo: day.date })}
                className="flex w-full items-center justify-between gap-3 rounded-md px-1 py-2 text-left text-[13px] hover:bg-surface-2"
              >
                <span className="text-ink">{dayLabel(day.date, true)}</span>
                <span className="flex items-center gap-3">
                  <Num value={day.totalTokens} plain className="font-medium text-ink" />
                  <span className="num w-28 text-right text-xs text-ink-3">约为平时的 {day.ratio.toFixed(1)} 倍</span>
                </span>
              </button>
            </li>
          ))}
        </ul>
      )}
    </Card>
  );
}
