import { cn } from "./ui";

/** The app icon (same artwork as `src-tauri/icons/source.svg`), cropped to its rounded square. */
export function AppMark({ className }: { className?: string }) {
  return (
    <svg viewBox="100 100 824 824" aria-hidden className={cn("shrink-0", className)}>
      <rect x="100" y="100" width="824" height="824" rx="185" fill="#1e2b4a" />
      <path d="M520.00 206.10A306 306 0 1 1 207.21 539.19L303.07 533.16A210 210 0 1 0 520.00 302.15Z" fill="#f6f1e7" />
      <path d="M206.21 523.22A306 306 0 0 1 296.77 294.49L362.52 364.50A210 210 0 0 0 302.06 517.19Z" fill="#8fa2c4" />
      <path d="M308.43 283.54A306 306 0 0 1 504.00 206.10L504.00 302.15A210 210 0 0 0 374.18 353.55Z" fill="#e5813f" />
      <circle cx="512" cy="512" r="146" fill="none" stroke="#7f8491" strokeWidth="16" />
      <circle cx="512" cy="512" r="74" fill="#f6f1e7" />
    </svg>
  );
}
