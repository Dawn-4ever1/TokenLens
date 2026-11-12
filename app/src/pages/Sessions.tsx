import { Search, X } from "lucide-react";
import { useEffect, useMemo, useState } from "react";
import { ClientName, LoadError, ModelName, Num, PrecisionBadge } from "@/components/common";
import { Badge, Button, Card, EmptyState, Segmented, Skeleton, Tip, cn, inputClass } from "@/components/ui";
import { dashboardGet, errorMessage } from "@/lib/api";
import { useData, useDashboard } from "@/lib/data";
import { dateTime, exact, relativeTime, shortId, usd } from "@/lib/format";
import { NO_PROJECT_KEY } from "@/lib/labels";
import { useNav } from "@/lib/nav";
import { activeFilterCount, rangeLabel, resolveRange } from "@/lib/range";
import type { SessionPage, SessionSort, SessionSummary } from "@/lib/types";

const SORTS: { key: SessionSort; label: string }[] = [
  { key: "recent", label: "最近" },
  { key: "tokens", label: "Tokens" },
  { key: "cost", label: "成本" },
  { key: "turns", label: "轮次" },
];

const PAGE_SIZE = 50;

function useDebounced<T>(value: T, ms: number): T {
  const [debounced, setDebounced] = useState(value);
  useEffect(() => {
    const id = setTimeout(() => setDebounced(value), ms);
    return () => clearTimeout(id);
  }, [value, ms]);
  return debounced;
}

export function Sessions() {
  const { filters, setFilters, timeZone, query } = useData();
  const { openSession } = useNav();
  const [text, setText] = useState("");
  const [sort, setSort] = useState<SessionSort>("recent");
  const search = useDebounced(text.trim(), 300);
  const base = { ...query(), q: search || undefined, sort, limit: String(PAGE_SIZE) };
  const baseKey = JSON.stringify(base);
  const first = useDashboard<SessionPage>("sessions", base);
  const [more, setMore] = useState<{ key: string; sessions: SessionSummary[]; cursor?: string; error: unknown; loading: boolean }>({
    key: baseKey,
    sessions: [],
    error: null,
    loading: false,
  });
  const extra = more.key === baseKey ? more : { key: baseKey, sessions: [], cursor: undefined, error: null, loading: false };
  const nextCursor = extra.sessions.length ? extra.cursor : first.data?.nextCursor;

  const rows = useMemo(() => {
    const seen = new Set<string>();
    const out: SessionSummary[] = [];
    for (const session of [...(first.data?.sessions ?? []), ...extra.sessions]) {
      if (seen.has(session.sessionKey)) continue;
      seen.add(session.sessionKey);
      out.push(session);
    }
    return out;
  }, [first.data, extra.sessions]);

  const loadMore = () => {
    if (!nextCursor) return;
    const key = baseKey;
    setMore({ ...extra, loading: true, error: null });
    dashboardGet<SessionPage>("sessions", { ...base, cursor: nextCursor })
      .then((page) =>
        setMore((previous) =>
          previous.key === key ? { key, sessions: [...previous.sessions, ...page.sessions], cursor: page.nextCursor, error: null, loading: false } : previous,
        ),
      )
      .catch((error: unknown) => setMore((previous) => (previous.key === key ? { ...previous, error, loading: false } : previous)));
  };

  const span = resolveRange(filters, timeZone);
  const filtered = activeFilterCount(filters) > 0;

  return (
    <div className="space-y-4">
      <div className="flex flex-wrap items-center gap-3">
        <div className="relative w-full max-w-80">
          <Search className="pointer-events-none absolute left-3 top-1/2 h-4 w-4 -translate-y-1/2 text-ink-3" aria-hidden />
          <input
            type="search"
            value={text}
            onChange={(event) => setText(event.target.value)}
            placeholder="搜索会话 ID、模型或项目"
            aria-label="搜索会话"
            maxLength={100}
            className={cn(inputClass, "pl-9 pr-8")}
          />
          {text && (
            <button type="button" aria-label="清除搜索" onClick={() => setText("")} className="absolute right-2 top-1/2 -translate-y-1/2 rounded p-1 text-ink-3 hover:text-ink">
              <X className="h-3.5 w-3.5" />
            </button>
          )}
        </div>
        <Segmented label="排序" value={sort} options={SORTS} onChange={setSort} />
        <span className="ml-auto text-xs text-ink-3">
          {first.data ? `${rangeLabel(filters, span)} · 共 ${exact(first.data.total)} 个会话` : ""}
        </span>
      </div>

      <Card bodyClassName="p-0 pt-0">
        {!first.data ? (
          first.error ? (
            <LoadError error={first.error} onRetry={first.reload} />
          ) : (
            <div className="space-y-2 p-5">
              {Array.from({ length: 8 }, (_, i) => (
                <Skeleton key={i} className="h-9 w-full" />
              ))}
            </div>
          )
        ) : rows.length === 0 ? (
          <EmptyState
            title={search ? "没有匹配的会话" : "这个范围内没有会话"}
            hint={
              search
                ? "搜索只匹配会话 ID、模型名和项目名；会话标题和内容不会上传，所以搜不到。"
                : filtered
                  ? "当前筛选条件下没有会话，可以清除筛选或换一个时间范围。"
                  : "换一个时间范围试试。如果刚配对，第一次上传可能需要几分钟。"
            }
            action={
              filtered && !search ? (
                <Button size="sm" onClick={() => setFilters((previous) => ({ ...previous, devices: [], clients: [], models: [], projects: [] }))}>
                  清除筛选
                </Button>
              ) : undefined
            }
          />
        ) : (
          <SessionTable rows={rows} timeZone={timeZone} onOpen={openSession} />
        )}
        {first.error && first.data ? <div className="px-5"><LoadError error={first.error} onRetry={first.reload} compact /></div> : null}
      </Card>

      {nextCursor && rows.length > 0 && (
        <div className="flex flex-col items-center gap-2">
          {extra.error ? <p className="text-xs text-bad">{errorMessage(extra.error)}</p> : null}
          <Button onClick={loadMore} disabled={extra.loading}>
            {extra.loading ? "加载中…" : `加载更多（已显示 ${rows.length} / ${exact(first.data?.total ?? 0)}）`}
          </Button>
        </div>
      )}
      <p className="text-center text-[11px] leading-5 text-ink-3">
        只显示数字统计；会话标题、提示词和回答都不会离开你的电脑。
      </p>
    </div>
  );
}

function SessionTable({ rows, timeZone, onOpen }: { rows: SessionSummary[]; timeZone: string; onOpen: (key: string) => void }) {
  const th = "px-3 py-2.5 text-left text-xs font-medium text-ink-3 whitespace-nowrap";
  const td = "px-3 py-2.5 whitespace-nowrap";
  return (
    <div className="overflow-x-auto">
      <table className="w-full text-[13px]">
        <thead className="border-b border-line">
          <tr>
            <th className={cn(th, "pl-5")}>会话</th>
            <th className={cn(th, "hidden min-[1200px]:table-cell")}>设备</th>
            <th className={th}>模型</th>
            <th className={cn(th, "text-right")}>总 tokens</th>
            <th className={cn(th, "hidden text-right min-[1200px]:table-cell")}>缓存读取</th>
            <th className={cn(th, "text-right")}>输出</th>
            <th className={cn(th, "text-right")}>成本</th>
            <th className={cn(th, "text-right")}>轮次</th>
            <th className={cn(th, "pr-5 text-right")}>最后使用</th>
          </tr>
        </thead>
        <tbody>
          {rows.map((session) => (
            <tr
              key={session.sessionKey}
              tabIndex={0}
              onClick={() => onOpen(session.sessionKey)}
              onKeyDown={(event) => {
                if (event.key === "Enter" || event.key === " ") {
                  event.preventDefault();
                  onOpen(session.sessionKey);
                }
              }}
              className="cursor-default border-b border-line/60 last:border-0 hover:bg-surface-2/60 focus-visible:bg-surface-2/60"
            >
              <td className={cn(td, "max-w-[280px] pl-5")}>
                <div className="flex min-w-0 items-center gap-1.5">
                  <ClientName id={session.client} className="font-medium text-ink" />
                  <PrecisionBadge precision={session.precision} />
                </div>
                <div className="mt-0.5 flex min-w-0 items-center gap-1.5 text-[11px] text-ink-3">
                  <Tip content={`会话 ID：${session.sessionId}`}>
                    <span className="truncate font-mono">{shortId(session.sessionId, 10)}</span>
                  </Tip>
                  {session.projectLabel && session.projectLabel !== NO_PROJECT_KEY ? <span className="truncate">· {session.projectLabel}</span> : null}
                  {session.subagentCount > 0 ? <Badge className="py-0">含 {session.subagentCount} 个子代理</Badge> : null}
                </div>
              </td>
              <td className={cn(td, "hidden truncate text-ink-2 min-[1200px]:table-cell")}>{session.deviceName}</td>
              <td className={cn(td, "truncate text-ink-2")}>
                <Tip content={session.models.join("、") || "—"}>
                  <span>
                    {session.models.length ? <ModelName id={session.models[0]!} /> : "—"}
                    {session.models.length > 1 ? <span className="text-ink-3"> +{session.models.length - 1}</span> : null}
                  </span>
                </Tip>
              </td>
              <td className={cn(td, "text-right font-medium text-ink")}>
                <Num value={session.totalTokens} plain />
              </td>
              <td className={cn(td, "hidden text-right text-ink-2 min-[1200px]:table-cell")}>
                <Num value={session.cacheReadTokens} plain />
              </td>
              <td className={cn(td, "text-right text-ink-2")}>
                <Num value={session.outputTokens} plain />
              </td>
              <td className={cn(td, "num text-right text-ink-2")}>{usd(session.costUsd)}</td>
              <td className={cn(td, "num text-right text-ink-2")}>{exact(session.turnCount)}</td>
              <td className={cn(td, "pr-5 text-right text-ink-2")}>
                <Tip content={`开始 ${dateTime(session.startedAt, timeZone)} · 最后 ${dateTime(session.lastUsedAt, timeZone)}`}>
                  <span>{relativeTime(session.lastUsedAt)}</span>
                </Tip>
              </td>
            </tr>
          ))}
        </tbody>
      </table>
    </div>
  );
}
