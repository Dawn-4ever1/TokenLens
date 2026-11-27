import { RefreshCw } from "lucide-react";
import { useCallback, useEffect, useState } from "react";
import { ClientName } from "@/components/common";
import { Badge, Button, Card, EmptyState, Skeleton, Tip, cn } from "@/components/ui";
import { app } from "@/lib/api";
import { useData } from "@/lib/data";
import { dateTime, relativeTime } from "@/lib/format";
import { clientLabel } from "@/lib/labels";
import { hasReset, quotaTone, resetCountdown, usedText, windowLabel, type QuotaTone } from "@/lib/quota";
import type { QuotaWindow, Quotas, SubscriptionQuota } from "@/lib/types";

/** The agents update their records as they are used; a minute is often enough. */
const POLL_MS = 60_000;

/**
 * Reads the quotas on opening the page, on every dashboard refresh, each
 * minute while the window is visible, on focus and on request (D082).
 * Reading is local only: no request leaves this computer.
 */
function useQuotas(): { quotas: Quotas | null; failed: boolean; reload: () => void } {
  const { tick } = useData();
  const [quotas, setQuotas] = useState<Quotas | null>(null);
  const [failed, setFailed] = useState(false);
  const [poll, setPoll] = useState(0);
  const reload = useCallback(() => setPoll((n) => n + 1), []);
  useEffect(() => {
    const id = setInterval(() => {
      if (document.visibilityState === "visible") reload();
    }, POLL_MS);
    window.addEventListener("focus", reload);
    return () => {
      clearInterval(id);
      window.removeEventListener("focus", reload);
    };
  }, [reload]);
  useEffect(() => {
    let alive = true;
    app
      .subscriptionQuotas()
      .then((next) => {
        if (!alive) return;
        setQuotas(next);
        setFailed(false);
      })
      .catch(() => {
        // Keep the last reading; say so only when there is none.
        if (alive) setFailed(true);
      });
    return () => {
      alive = false;
    };
  }, [tick, poll]);
  return { quotas, failed, reload };
}

/** Re-renders countdowns and "updated" times without reading again. */
function useNow(intervalMs: number): number {
  const [now, setNow] = useState(() => Date.now());
  useEffect(() => {
    const id = setInterval(() => setNow(Date.now()), intervalMs);
    return () => clearInterval(id);
  }, [intervalMs]);
  return now;
}

const BAR: Record<QuotaTone, string> = { normal: "var(--primary)", warn: "var(--warn)", bad: "var(--bad)" };
const TEXT: Record<QuotaTone, string> = {
  normal: "text-ink",
  warn: "text-[color-mix(in_oklch,var(--warn)_60%,var(--ink))]",
  bad: "text-bad",
};

export function Quota() {
  const { quotas, failed, reload } = useQuotas();
  const now = useNow(30_000);
  return (
    <div className="space-y-[var(--stack-space)]" data-testid="quota-page">
      <div className="flex flex-wrap items-start justify-between gap-x-6 gap-y-2">
        <p className="max-w-2xl text-xs leading-5 text-ink-3">
          Codex、Claude Code 记在这台电脑上的最新订阅额度。只读本机记录，不联网、不读取登录信息，也不上传；数字截至工具上一次更新，不受日期和筛选影响。
        </p>
        <Button variant="ghost" size="sm" onClick={reload}>
          <RefreshCw className="h-3.5 w-3.5" aria-hidden /> 重新读取
        </Button>
      </div>
      {quotas === null ? (
        failed ? (
          <Card>
            <EmptyState title="暂时读不到额度" hint="请稍后点「重新读取」。" />
          </Card>
        ) : (
          <div className="grid grid-cols-1 gap-[var(--grid-gap)] min-[1100px]:grid-cols-2">
            <Skeleton className="h-[220px] rounded-[var(--radius-card)]" />
            <Skeleton className="h-[220px] rounded-[var(--radius-card)]" />
          </div>
        )
      ) : quotas.subscriptions.length === 0 ? (
        <Card>
          <EmptyState
            title="这台电脑上还没有订阅额度"
            hint="用订阅账号登录 Codex 或 Claude Code 并使用一次后，额度会出现在这里。用 API Key 登录的工具没有订阅额度。"
          />
        </Card>
      ) : (
        <div className="grid grid-cols-1 gap-[var(--grid-gap)] min-[1100px]:grid-cols-2">
          {quotas.subscriptions.map((subscription) => (
            <SubscriptionCard key={subscription.client} subscription={subscription} now={now} />
          ))}
        </div>
      )}
    </div>
  );
}

function updatedText(updatedAt: string, now: number): string {
  const ago = relativeTime(updatedAt, now);
  return ago === "刚刚" ? "刚刚更新" : `${ago}更新`;
}

function SubscriptionCard({ subscription, now }: { subscription: SubscriptionQuota; now: number }) {
  const name = clientLabel(subscription.client);
  return (
    <Card
      data-testid={`quota-${subscription.client}`}
      title={
        <span className="flex min-w-0 items-center gap-2">
          <ClientName id={subscription.client} />
          {subscription.plan && <Badge>{subscription.plan}</Badge>}
        </span>
      }
      action={
        subscription.updatedAt ? (
          <Tip content={`${name} 上次读取额度：${dateTime(subscription.updatedAt)}`}>
            <span className="text-xs leading-6 text-ink-3">{updatedText(subscription.updatedAt, now)}</span>
          </Tip>
        ) : undefined
      }
    >
      {subscription.status === "unreadable" ? (
        <p className="py-2 text-sm leading-6 text-ink-3">暂时读不到额度：{name} 可能换了记录方式，需要等 TokenLens 更新适配。</p>
      ) : (
        <ul className="space-y-5 pt-1">
          {subscription.windows.map((limit, index) => (
            <QuotaRow key={`${limit.minutes}-${limit.scope}-${index}`} name={name} limit={limit} now={now} />
          ))}
        </ul>
      )}
    </Card>
  );
}

function QuotaRow({ name, limit, now }: { name: string; limit: QuotaWindow; now: number }) {
  const label = windowLabel(limit);
  const renewed = hasReset(limit, now);
  const used = renewed ? 0 : Math.min(100, Math.max(0, limit.usedPercent));
  const tone = renewed ? "normal" : quotaTone(used);
  const countdown = renewed ? null : resetCountdown(limit.resetsAt, now);
  return (
    <li className="min-w-0">
      <div className="flex min-w-0 items-baseline justify-between gap-3">
        <span className="truncate text-[13px] text-ink-2">{label}</span>
        <span className={cn("num shrink-0 text-base font-semibold", TEXT[tone])}>{renewed ? "已重置" : usedText(used)}</span>
      </div>
      <div
        role="meter"
        aria-label={`${name} ${label}`}
        aria-valuemin={0}
        aria-valuemax={100}
        aria-valuenow={Math.round(used)}
        data-tone={tone}
        className="mt-2 h-2 w-full overflow-hidden rounded-[var(--radius-small)] bg-surface-2"
      >
        <div className="h-full rounded-[var(--radius-small)]" style={{ width: `${used}%`, background: BAR[tone] }} />
      </div>
      <div className="mt-1.5 flex min-w-0 flex-wrap justify-between gap-x-3 text-[11px] text-ink-3">
        <span>{renewed ? "上次读数之后已重置，下次使用后更新" : countdown ?? ""}</span>
        {!renewed && limit.resetsAt && <span className="num">重置于 {dateTime(limit.resetsAt)}</span>}
      </div>
    </li>
  );
}
