import { useEffect, useState } from "react";
import type { Metric, Spend } from "./api";

/** How full a limit is (0–100), when it has one. */
export function fill(m: Metric): number | null {
  if (m.kind === "percent") return m.used;
  if ((m.kind === "money" || m.kind === "count") && m.limit && m.limit > 0) return Math.min(100, (m.used / m.limit) * 100);
  return null;
}

// ---- pace: a verdict on the whole window, from the current burn rate ----------
//
// Blue while you're on course to finish with at least 10% to spare, amber when
// you're projected to land inside the last 10%, red when you're projected to
// run out before the reset (or already have). Windows too young to project, and
// limits without a window, are judged by level: amber from 80% used, red from
// 90%. (OpenUsage's pacing rules.)

export type Verdict = "ok" | "close" | "over";

export interface Pace {
  verdict: Verdict;
  /** Where even burning would sit now (0–1), for the pace tick. */
  elapsed: number | null;
  /** Projected cushion at reset, in points (amber only). */
  spare: number | null;
  /** When the limit runs out at this rate (red only, epoch ms). */
  runsOutAt: number | null;
  reached: boolean;
}

export function pace(m: Metric, now: number): Pace {
  const f = fill(m) ?? 0;
  const base: Pace = { verdict: "ok", elapsed: null, spare: null, runsOutAt: null, reached: false };
  if (f >= 99.5) return { ...base, verdict: "over", reached: true };
  if (m.resetsAt && m.periodMs && now < m.resetsAt) {
    const start = m.resetsAt - m.periodMs;
    const elapsedMs = now - start;
    if (f > 0 && elapsedMs >= Math.max(60_000, m.periodMs * 0.01)) {
      const projected = (f / elapsedMs) * m.periodMs;
      const elapsed = elapsedMs / m.periodMs;
      if (projected <= 90) return { ...base, elapsed };
      if (projected <= 100) return { ...base, verdict: "close", elapsed, spare: Math.max(1, Math.round(100 - projected)) };
      const rate = projected / m.periodMs;
      const eta = now + (100 - f) / rate;
      return { ...base, verdict: "over", elapsed, runsOutAt: eta < m.resetsAt ? eta : null };
    }
  }
  if (f >= 90) return { ...base, verdict: "over" };
  if (f >= 80) return { ...base, verdict: "close" };
  return base;
}

// ---- numbers -------------------------------------------------------------------

export function money(n: number, currency: string | null = "USD", compact = false): string {
  const cur = currency ?? "USD";
  try {
    if (compact && n >= 1000) {
      return new Intl.NumberFormat("en-US", { style: "currency", currency: cur, notation: "compact", maximumFractionDigits: 2 }).format(n);
    }
    return new Intl.NumberFormat("en-US", {
      style: "currency",
      currency: cur,
      minimumFractionDigits: compact && n >= 100 ? 0 : 2,
      maximumFractionDigits: compact && n >= 100 ? 0 : 2,
    }).format(n);
  } catch {
    return `${n.toFixed(2)} ${cur}`;
  }
}

/** 1.2K, 62.1M, 1.52B. */
export function tokens(n: number): string {
  const units: [number, string][] = [
    [1e9, "B"],
    [1e6, "M"],
    [1e3, "K"],
  ];
  for (const [v, u] of units) {
    if (n >= v) {
      const x = n / v;
      return `${x >= 100 ? x.toFixed(0) : x >= 10 ? x.toFixed(1) : x.toFixed(2).replace(/\.?0+$/, "")}${u}`;
    }
  }
  return String(Math.round(n));
}

/** Big number + a quiet unit, for the ring's center: ["12.4", "million"]. */
export function tokensParts(n: number): [string, string] {
  const units: [number, string][] = [
    [1e9, "billion"],
    [1e6, "million"],
    [1e3, "thousand"],
  ];
  for (const [v, u] of units) {
    if (n >= v) {
      const x = n / v;
      return [x >= 100 ? x.toFixed(0) : x.toFixed(1), u];
    }
  }
  return [String(Math.round(n)), "tokens"];
}

// ---- time ----------------------------------------------------------------------

function clock(ms: number): string {
  return new Date(ms).toLocaleTimeString(undefined, { hour: "numeric", minute: "2-digit" });
}

/** "in 4h 26m", "in 3d 4h". */
export function countdown(ms: number, now: number): string {
  const m = Math.max(1, Math.ceil((ms - now) / 60000));
  if (m < 60) return `${m}m`;
  const h = Math.floor(m / 60);
  if (h < 24) return `${h}h ${m % 60}m`;
  const d = Math.floor(h / 24);
  return `${d}d ${h % 24}h`;
}

/** "today at 6:38 PM", "tomorrow at 9:00 AM", "Oct 16 at 6:00 PM". */
export function exactTime(ms: number, now: number): string {
  const day = (t: number) => new Date(t).toDateString();
  if (day(ms) === day(now)) return `today at ${clock(ms)}`;
  if (day(ms) === day(now + 86_400_000)) return `tomorrow at ${clock(ms)}`;
  return `${new Date(ms).toLocaleDateString(undefined, { month: "short", day: "numeric" })} at ${clock(ms)}`;
}

export function resetLabel(ms: number | null, now: number, exact: boolean): string | null {
  if (!ms) return null;
  if (ms <= now) return "Resetting…";
  return exact ? `Resets ${exactTime(ms, now)}` : `Resets in ${countdown(ms, now)}`;
}

export function ago(ms: number, now: number): string {
  const m = Math.round((now - ms) / 60000);
  if (m < 1) return "just now";
  if (m < 60) return `${m}m ago`;
  const h = Math.round(m / 60);
  return h < 24 ? `${h}h ago` : `${Math.round(h / 24)}d ago`;
}

// ---- spend periods ---------------------------------------------------------------

export type Period = "today" | "yesterday" | "month";

export function dayKey(t: number): string {
  const d = new Date(t);
  return `${d.getFullYear()}-${String(d.getMonth() + 1).padStart(2, "0")}-${String(d.getDate()).padStart(2, "0")}`;
}

/** Cost and tokens for a period; null when the period has no usage. */
export function periodTotal(spend: Spend | null, period: Period, now: number): { cost: number; tokens: number } | null {
  if (!spend) return null;
  const today = dayKey(now);
  const yesterday = dayKey(now - 86_400_000);
  const days = spend.days.filter((d) => (period === "today" ? d.date === today : period === "yesterday" ? d.date === yesterday : true));
  const cost = days.reduce((s, d) => s + d.cost, 0);
  const tok = days.reduce((s, d) => s + d.tokens, 0);
  return tok > 0 || cost > 0 ? { cost, tokens: tok } : null;
}

/** Tokens per day for the last 30 days, oldest first (zeros included). */
export function trend(spend: Spend | null, now: number): number[] {
  const by = new Map((spend?.days ?? []).map((d) => [d.date, d.tokens]));
  return Array.from({ length: 30 }, (_, i) => by.get(dayKey(now - (29 - i) * 86_400_000)) ?? 0);
}

/** A clock that ticks every 30 s while mounted, for countdowns. */
export function useNow(): number {
  const [now, setNow] = useState(() => Date.now());
  useEffect(() => {
    const t = setInterval(() => setNow(Date.now()), 30_000);
    return () => clearInterval(t);
  }, []);
  return now;
}
