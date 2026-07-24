import { type ClassValue, clsx } from "clsx";
import { twMerge } from "tailwind-merge";

export function cn(...inputs: ClassValue[]) {
  return twMerge(clsx(inputs));
}

/** Seconds → compact human duration ("0.4s", "2m33s", "41.6m", "1.2h"). */
export function fmtDuration(s: number): string {
  if (s < 10) return `${s.toFixed(2).replace(/\.?0+$/, "")}s`;
  if (s < 60) return `${Math.round(s)}s`;
  if (s < 3600) {
    const m = s / 60;
    return m < 10 ? `${m.toFixed(1).replace(/\.0$/, "")}m` : `${Math.round(m)}m`;
  }
  return `${(s / 3600).toFixed(1).replace(/\.0$/, "")}h`;
}

/** Megabytes → "84 MB" / "5.8 GB". */
export function fmtMemory(mb: number): string {
  if (mb < 1024) return `${Math.round(mb)} MB`;
  const gb = mb / 1024;
  return `${gb < 10 ? gb.toFixed(1) : Math.round(gb)} GB`;
}

/** USD → "$0.074" / "$1.19". */
export function fmtUsd(usd: number): string {
  return usd < 0.1 ? `$${usd.toFixed(3)}` : `$${usd.toFixed(2)}`;
}
