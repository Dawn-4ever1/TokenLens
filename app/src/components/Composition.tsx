import { TOKEN_PARTS } from "@/lib/labels";
import { exact, percent, share } from "@/lib/format";
import { Num } from "./common";
import type { Breakdown } from "@/lib/types";
import { Tip, cn } from "./ui";

type Part = (typeof TOKEN_PARTS)[number] & { tokens: number };

function Bar({ parts, whole, total, dense }: { parts: Part[]; whole: number; total: number; dense?: boolean }) {
  return (
    <div className={cn("flex w-full gap-px overflow-hidden rounded-md bg-surface-2", dense ? "h-2.5" : "h-4")}>
      {whole > 0 &&
        parts
          .filter((part) => part.tokens > 0)
          .map((part) => (
            <Tip key={part.key} content={`${part.label}：${exact(part.tokens)}（占总量 ${percent(share(part.tokens, total))}）`}>
              <div className="h-full min-w-[3px]" style={{ width: `${(part.tokens / whole) * 100}%`, background: part.color }} />
            </Tip>
          ))}
    </div>
  );
}

/**
 * Five mutually exclusive token buckets plus a legend. Cache read usually
 * dwarfs the rest, so when it is over half it gets a bar of its own and the
 * other four share a second bar scaled to their own sum; otherwise one 100%
 * bar. Legend percentages are always of the total.
 */
export function Composition({ value, dense, className }: { value: Breakdown; dense?: boolean; className?: string }) {
  const total = value.totalTokens;
  const parts: Part[] = TOKEN_PARTS.map((part) => ({ ...part, tokens: value[part.key] }));
  const cache = parts.filter((part) => part.key === "cacheReadTokens");
  const others = parts.filter((part) => part.key !== "cacheReadTokens");
  const cacheTokens = value.cacheReadTokens;
  const rest = total - cacheTokens;
  const split = total > 0 && rest > 0 && cacheTokens / total > 0.5;
  const caption = "mb-1 flex items-baseline justify-between gap-2 text-[11px] text-ink-3";
  return (
    <div className={className}>
      {split ? (
        <div className={cn("grid gap-x-5 gap-y-3", dense ? "grid-cols-1" : "grid-cols-1 min-[1000px]:grid-cols-2")}>
          <div className="min-w-0">
            <div className={caption}>
              <span>缓存读取占总量</span>
              <span className="num text-ink-2">{percent(share(cacheTokens, total))}</span>
            </div>
            <Bar parts={cache} whole={total} total={total} dense={dense} />
          </div>
          <div className="min-w-0">
            <div className={caption}>
              <span>其余四类（放大显示）</span>
              <span className="num text-ink-2">共占 {percent(share(rest, total))}</span>
            </div>
            <Bar parts={others} whole={rest} total={total} dense={dense} />
          </div>
        </div>
      ) : (
        <Bar parts={parts} whole={total} total={total} dense={dense} />
      )}
      <dl className={cn("mt-3 grid gap-x-4 gap-y-2", dense ? "grid-cols-5" : "grid-cols-2 min-[1200px]:grid-cols-5")}>
        {parts.map((part) => (
          <div key={part.key} className="min-w-0">
            <dt className="flex items-center gap-1.5 text-xs text-ink-3">
              <span aria-hidden className="h-2 w-2 shrink-0 rounded-sm" style={{ background: part.color }} />
              <span className="truncate">{part.label}</span>
            </dt>
            <dd className="mt-0.5 flex items-baseline gap-1.5">
              <Num value={part.tokens} className={cn("font-medium text-ink", dense ? "text-[13px]" : "text-sm")} />
              <span className="num text-[11px] text-ink-3">{percent(share(part.tokens, total))}</span>
            </dd>
          </div>
        ))}
      </dl>
    </div>
  );
}
