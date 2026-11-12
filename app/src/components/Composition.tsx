import { TOKEN_PARTS } from "@/lib/labels";
import { exact, percent, share } from "@/lib/format";
import { Num } from "./common";
import type { Breakdown } from "@/lib/types";
import { Tip, cn } from "./ui";

/** Five mutually exclusive token buckets as one 100% bar plus a legend. */
export function Composition({ value, dense, className }: { value: Breakdown; dense?: boolean; className?: string }) {
  const total = value.totalTokens;
  const parts = TOKEN_PARTS.map((part) => ({ ...part, tokens: value[part.key] }));
  return (
    <div className={className}>
      <div className={cn("flex w-full gap-px overflow-hidden rounded-md bg-surface-2", dense ? "h-2.5" : "h-4")}>
        {total > 0 &&
          parts
            .filter((part) => part.tokens > 0)
            .map((part) => (
              <Tip key={part.key} content={`${part.label}：${exact(part.tokens)}（${percent(share(part.tokens, total))}）`}>
                <div className="h-full min-w-[3px]" style={{ width: `${(part.tokens / total) * 100}%`, background: part.color }} />
              </Tip>
            ))}
      </div>
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
