import { cn } from "./ui";

/** The app icon (same artwork as `src-tauri/icons/source.svg`). */
export function AppMark({ className }: { className?: string }) {
  return (
    <svg viewBox="0 0 1024 1024" aria-hidden className={cn("shrink-0", className)}>
      <rect x="100" y="100" width="824" height="824" rx="185" fill="#1a1714" />
      <g fill="none" strokeWidth="113.3" transform="rotate(-90 512 512)">
        <circle cx="512" cy="512" r="236.9" stroke="#f6f1e7" strokeDasharray="1143.3 345.2" />
        <circle cx="512" cy="512" r="236.9" stroke="#8a8378" strokeDasharray="262.7 1225.8" strokeDashoffset="-1184.5" />
      </g>
      <circle cx="512" cy="512" r="139.1" fill="#e5813f" />
      <path d="M435.8 498.6 A77.3 77.3 0 0 1 498.6 435.8" fill="none" stroke="#f6f1e7" strokeWidth="25.8" strokeLinecap="round" />
      <circle cx="568.7" cy="568.7" r="15.5" fill="#f0b48a" />
    </svg>
  );
}
