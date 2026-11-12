import { useCallback, useState } from "react";
import { compact, credits, dateTime, duration, exact, percent, shortId, toMs, usd } from "@/lib/format";
import { useData, useDashboard } from "@/lib/data";
import { clientLabel, TOKEN_PARTS } from "@/lib/labels";
import type { Palette } from "@/lib/colors";
import type { SessionDetail, SessionTurn } from "@/lib/types";
import { Chart, baseOption, type ChartOption } from "./Chart";
import { Composition } from "./Composition";
import { ClientName, LoadError, ModelName, Num, PrecisionBadge, Stat } from "./common";
import { Badge, Sheet, Skeleton, Tip } from "./ui";

const TURN_LIMIT = 300;

export function SessionDrawer({ sessionKey, onClose }: { sessionKey: string | null; onClose: () => void }) {
  const { data, error, loading, reload } = useDashboard<SessionDetail>(sessionKey ? `sessions/${sessionKey}` : null);
  const detail = data && data.session.sessionKey === sessionKey ? data : null;
  const s = detail?.session;
  return (
    <Sheet
      open={sessionKey !== null}
      onOpenChange={(open) => !open && onClose()}
      title={s ? <ClientName id={s.client} label={clientLabel(s.client)} /> : "会话详情"}
      description={s ? <span className="num">{s.deviceName} · 会话 {shortId(s.sessionId, 12)}{s.projectLabel ? ` · ${s.projectLabel}` : ""}</span> : undefined}
    >
      {!detail && error ? (
        <LoadError error={error} onRetry={reload} />
      ) : !detail || (loading && !detail) ? (
        <div className="space-y-4">
          <Skeleton className="h-16" />
          <Skeleton className="h-24" />
          <Skeleton className="h-64" />
        </div>
      ) : (
        <SessionBody detail={detail} />
      )}
    </Sheet>
  );
}

function SessionBody({ detail }: { detail: SessionDetail }) {
  const { timeZone } = useData();
  const s = detail.session;
  const [showAll, setShowAll] = useState(false);
  const span = (toMs(s.lastUsedAt) ?? 0) - (toMs(s.startedAt) ?? 0);
  const turns = showAll ? detail.turns : detail.turns.slice(0, TURN_LIMIT);
  return (
    <div className="space-y-6">
      <div className="grid grid-cols-3 gap-4 min-[700px]:grid-cols-6">
        <Stat label="总 tokens">
          <Num value={s.totalTokens} />
        </Stat>
        <Stat label="成本（估算）" hint={s.unpricedTokens > 0 ? `${compact(s.unpricedTokens)} 未定价` : undefined}>
          {usd(s.costUsd)}
        </Stat>
        <Stat label="轮次">{exact(s.turnCount)}</Stat>
        <Stat label="子代理">{exact(s.subagentCount)}</Stat>
        <Stat label="跨度">{duration(span)}</Stat>
        <Stat label="缓存命中率">{percent(s.cacheHitRate)}</Stat>
      </div>
      <div className="flex flex-wrap items-center gap-2 text-xs text-ink-3">
        <span className="num">
          {dateTime(s.startedAt, timeZone, true, true)} → {dateTime(s.lastUsedAt, timeZone, true, true)}
        </span>
        <PrecisionBadge precision={s.precision} always />
        {s.credits !== null && <Badge tone="accent">Qoder credits {credits(s.credits)}</Badge>}
      </div>
      <section>
        <h3 className="mb-2 text-[13px] font-medium text-ink">Token 构成</h3>
        <Composition value={s} dense />
      </section>
      <section>
        <h3 className="mb-1 text-[13px] font-medium text-ink">模型</h3>
        <div className="flex flex-wrap gap-1.5">
          {s.models.map((model) => (
            <Tip key={model} content={model}>
              <span>
              <Badge><ModelName id={model} /></Badge>
              </span>
            </Tip>
          ))}
        </div>
      </section>
      {detail.turns.length > 1 && (
        <section>
          <h3 className="mb-1 text-[13px] font-medium text-ink">每轮用量</h3>
          <TurnChart turns={detail.turns} />
        </section>
      )}
      <section>
        <h3 className="mb-2 text-[13px] font-medium text-ink">
          按轮次明细 <span className="num font-normal text-ink-3">（{detail.turns.length} 轮{detail.truncated ? "，已截断" : ""}）</span>
        </h3>
        <TurnTable turns={turns} timeZone={timeZone} />
        {!showAll && detail.turns.length > TURN_LIMIT && (
          <button type="button" className="mt-2 text-xs text-primary hover:underline" onClick={() => setShowAll(true)}>
            显示全部 {detail.turns.length} 轮
          </button>
        )}
      </section>
      {detail.subagents.length > 0 && (
        <section>
          <h3 className="mb-2 text-[13px] font-medium text-ink">子代理（{detail.subagents.length}）</h3>
          <div className="overflow-hidden rounded-lg border border-line">
            <table className="w-full text-left text-xs">
              <thead className="bg-surface-2 text-ink-3">
                <tr>
                  <th className="px-3 py-2 font-medium">子代理</th>
                  <th className="px-3 py-2 font-medium">模型</th>
                  <th className="px-3 py-2 text-right font-medium">tokens</th>
                  <th className="px-3 py-2 text-right font-medium">轮次</th>
                  <th className="px-3 py-2 text-right font-medium">开始</th>
                </tr>
              </thead>
              <tbody className="num">
                {detail.subagents.map((sub) => (
                  <tr key={sub.sessionId} className="border-t border-line">
                    <td className="px-3 py-1.5 text-ink-2">{shortId(sub.sessionId, 10)}</td>
                    <td className="px-3 py-1.5 text-ink-2">
                      <div className="flex min-w-0 flex-wrap gap-1.5">
                        {sub.models.map((model) => <ModelName key={model} id={model} />)}
                      </div>
                    </td>
                    <td className="px-3 py-1.5 text-right">
                      <Num value={sub.totalTokens} />
                    </td>
                    <td className="px-3 py-1.5 text-right">{sub.turnCount}</td>
                    <td className="px-3 py-1.5 text-right text-ink-3">{dateTime(sub.startedAt, timeZone)}</td>
                  </tr>
                ))}
              </tbody>
            </table>
          </div>
        </section>
      )}
      <p className="text-xs leading-5 text-ink-3">这里只有数字。采集端从不上传提示词、回答、代码或会话标题。</p>
    </div>
  );
}

function TurnChart({ turns }: { turns: SessionTurn[] }) {
  const option = useCallback(
    (palette: Palette): ChartOption => ({
      ...baseOption(palette),
      grid: { left: 4, right: 4, top: 8, bottom: 4, containLabel: true },
      xAxis: { type: "category", data: turns.map((turn) => turn.index), axisLine: { lineStyle: { color: palette.border } }, axisTick: { show: false }, axisLabel: { color: palette.ink3, fontSize: 10 } },
      yAxis: { type: "value", splitLine: { lineStyle: { color: palette.grid } }, axisLabel: { color: palette.ink3, fontSize: 10, formatter: (v: number) => compact(v) } },
      tooltip: {
        ...(baseOption(palette).tooltip as object),
        trigger: "axis",
        axisPointer: { type: "shadow" },
        valueFormatter: (v: number) => exact(v),
      },
      series: TOKEN_PARTS.map((part) => ({
        name: part.label,
        type: "bar",
        stack: "tokens",
        barMaxWidth: 18,
        color: palette.resolve(part.color),
        data: turns.map((turn) => turn[part.key]),
      })),
    }),
    [turns],
  );
  return <Chart option={option} height={160} label="每一轮的 token 用量柱状图" />;
}

function TurnTable({ turns, timeZone }: { turns: SessionTurn[]; timeZone: string }) {
  return (
    <div className="overflow-x-auto rounded-lg border border-line">
      <table className="w-full min-w-[620px] text-left text-xs">
        <thead className="bg-surface-2 text-ink-3">
          <tr>
            <th className="px-3 py-2 font-medium">#</th>
            <th className="px-3 py-2 font-medium">时间</th>
            <th className="px-3 py-2 font-medium">模型</th>
            <th className="px-3 py-2 text-right font-medium">输入</th>
            <th className="px-3 py-2 text-right font-medium">输出</th>
            <th className="px-3 py-2 text-right font-medium">缓存读</th>
            <th className="px-3 py-2 text-right font-medium">缓存写</th>
            <th className="px-3 py-2 text-right font-medium">耗时</th>
          </tr>
        </thead>
        <tbody className="num">
          {turns.map((turn) => (
            <tr key={turn.index} className="border-t border-line">
              <td className="px-3 py-1.5 text-ink-3">{turn.index}</td>
              <td className="whitespace-nowrap px-3 py-1.5 text-ink-2">
                {dateTime(turn.startedAt, timeZone)}
                {turn.isSubagent && (
                  <Badge tone="primary" className="ml-1.5">
                    子代理
                  </Badge>
                )}
                {turn.precision === "estimated" && (
                  <Badge tone="accent" className="ml-1.5">
                    估算
                  </Badge>
                )}
              </td>
              <td className="max-w-40 truncate px-3 py-1.5 text-ink-2">
                <div className="flex min-w-0 flex-wrap gap-1.5">
                  {turn.models.map((model) => <ModelName key={model} id={model} />)}
                </div>
              </td>
              <td className="px-3 py-1.5 text-right">
                <Num value={turn.inputTokens} />
              </td>
              <td className="px-3 py-1.5 text-right">
                <Num value={turn.outputTokens} />
              </td>
              <td className="px-3 py-1.5 text-right">
                <Num value={turn.cacheReadTokens} />
              </td>
              <td className="px-3 py-1.5 text-right">
                <Num value={turn.cacheWriteTokens} />
              </td>
              <td className="px-3 py-1.5 text-right text-ink-3">{duration(turn.durationMs)}</td>
            </tr>
          ))}
        </tbody>
      </table>
    </div>
  );
}
