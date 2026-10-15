import { AnimatePresence, motion } from "framer-motion";
import { Check, ChevronDown, Flame, LogIn, Pin, PinOff, Settings2, TriangleAlert } from "lucide-react";
import { useCallback, useEffect, useRef, useState } from "react";
import { api, type Metric, type ProviderState } from "../lib/api";
import {
  ago,
  countdown,
  exactTime,
  fill,
  money,
  pace,
  periodTotal,
  resetLabel,
  tokens,
  tokensParts,
  trend,
  useNow,
  type Period,
} from "../lib/format";
import { Logo, ringColor } from "../lib/providers";
import { useDark, useEscape, useStore } from "../lib/store";
import { useReportHeight } from "./Panel";
import { SpendRing } from "./SpendRing";
import { PinButton } from "./ui";

const spring = { type: "spring", stiffness: 520, damping: 38, mass: 0.8 } as const;
const rowSpring = { type: "spring", stiffness: 520, damping: 34 } as const;
const pageSpring = { type: "spring", stiffness: 420, damping: 40 } as const;

export function Home() {
  const usage = useStore((s) => s.usage);
  const content = useRef<HTMLDivElement>(null);
  const footer = useRef<HTMLElement>(null);
  useReportHeight(content, footer);
  const now = useNow();
  const providers = usage?.providers.filter((p) => p.enabled) ?? [];

  return (
    <div className="page">
      <div className="page-scroll">
        <div className="page-content" ref={content}>
          {usage && providers.length === 0 ? (
            <NoProviders />
          ) : (
            <>
              <TotalSpend providers={providers} now={now} />
              {providers.map((p, i) => (
                <ProviderSection key={p.id} p={p} index={i} now={now} />
              ))}
            </>
          )}
        </div>
      </div>
      <Footer ref={footer} now={now} />
    </div>
  );
}

function NoProviders() {
  const setRoute = useStore((s) => s.setRoute);
  return (
    <div className="empty">
      <h3>No providers on</h3>
      <p>Turn on a provider to see your limits and spend.</p>
      <button className="pill-btn" onClick={() => setRoute({ name: "settings" })}>
        Open settings
      </button>
    </div>
  );
}

// ---- Total spend ---------------------------------------------------------------

const PERIODS: { value: Period; label: string }[] = [
  { value: "today", label: "Today" },
  { value: "yesterday", label: "Yesterday" },
  { value: "month", label: "30 Days" },
];

function TotalSpend({ providers, now }: { providers: ProviderState[]; now: number }) {
  const dark = useDark();
  const period = useStore((s) => s.period);
  const setPeriod = useStore((s) => s.setPeriod);
  const metric = useStore((s) => s.settings?.spendMetric ?? "cost");
  const update = useStore((s) => s.updateSettings);
  const [menu, setMenu] = useState(false);
  const closeMenu = useCallback(() => setMenu(false), []);
  useEscape(menu, closeMenu);

  const tracked = providers.filter((p) => p.spend);
  if (tracked.length === 0) return null;
  const rows = tracked
    .map((p) => {
      const t = periodTotal(p.spend, period, now);
      return { p, value: t ? (metric === "cost" ? t.cost : t.tokens) : 0 };
    })
    .filter((r) => r.value > 0)
    .sort((a, b) => b.value - a.value);
  const total = rows.reduce((s, r) => s + r.value, 0);
  const [big, unit] = metric === "cost" ? [money(total, "USD", true), "dollars"] : tokensParts(total);
  const emptyText = { today: "No usage today", yesterday: "No usage yesterday", month: "No usage in the last 30 days" }[period];

  return (
    <section className="spend-card">
      <header className="spend-head">
        <button className="spend-title" onClick={() => setMenu((m) => !m)} aria-haspopup="menu" aria-expanded={menu}>
          {metric === "cost" ? "Cost" : "Tokens"}
          <ChevronDown size={14} />
        </button>
        <AnimatePresence>
          {menu && (
            <>
              <div className="menu-scrim" onPointerDown={closeMenu} />
              <motion.div
                className="popover spend-menu"
                role="menu"
                initial={{ opacity: 0, scale: 0.96, y: -4 }}
                animate={{ opacity: 1, scale: 1, y: 0 }}
                exit={{ opacity: 0, scale: 0.97, y: -2, transition: { duration: 0.12 } }}
                transition={spring}
              >
                {(["cost", "tokens"] as const).map((m) => (
                  <button
                    key={m}
                    role="menuitemradio"
                    aria-checked={metric === m}
                    className="menu-item"
                    onClick={() => {
                      update({ spendMetric: m });
                      setMenu(false);
                    }}
                  >
                    <span>{m === "cost" ? "Cost" : "Tokens"}</span>
                    {metric === m && <Check size={14} />}
                  </button>
                ))}
              </motion.div>
            </>
          )}
        </AnimatePresence>
      </header>
      <div className="capsule" role="tablist">
        {PERIODS.map((o) => (
          <button key={o.value} role="tab" aria-selected={period === o.value} className={period === o.value ? "on" : ""} onClick={() => setPeriod(o.value)}>
            {period === o.value && <motion.span layoutId="capsule-pill" className="capsule-pill" transition={spring} />}
            <span>{o.label}</span>
          </button>
        ))}
      </div>
      {rows.length === 0 ? (
        <div className="spend-empty">{emptyText}</div>
      ) : (
        <div className="spend-body">
          <SpendRing slices={rows.map((r) => ({ id: r.p.id, value: r.value, color: ringColor(r.p.id, dark) }))}>
            <span className="ring-value">{big}</span>
            <span className="ring-unit">{unit}</span>
          </SpendRing>
          <ul className="legend">
            {rows.map((r) => (
              <motion.li key={r.p.id} layout transition={pageSpring}>
                <span className="legend-dot" style={{ background: ringColor(r.p.id, dark) }} />
                <span className="legend-name">{r.p.name}</span>
                <span className="legend-value">{metric === "cost" ? money(r.value) : tokens(r.value)}</span>
              </motion.li>
            ))}
          </ul>
        </div>
      )}
    </section>
  );
}

// ---- Provider sections -------------------------------------------------------------

function ProviderSection({ p, index, now }: { p: ProviderState; index: number; now: number }) {
  const expanded = useStore((s) => s.settings?.expanded.includes(p.id) ?? false);
  const pinned = useStore((s) => s.settings?.pinned.includes(p.id) ?? false);
  const update = useStore((s) => s.updateSettings);
  const meters = p.metrics.filter((m) => fill(m) !== null);
  const extras = p.metrics.filter((m) => fill(m) === null);
  const hasSpend = !!p.spend;
  const canExpand = hasSpend || extras.length > 0;
  const toggle = () => {
    const list = useStore.getState().settings?.expanded ?? [];
    update({ expanded: expanded ? list.filter((x) => x !== p.id) : [...list, p.id] });
  };

  return (
    <motion.section
      className="provider"
      initial={{ opacity: 0, x: 14, scale: 0.98 }}
      animate={{ opacity: 1, x: 0, scale: 1 }}
      transition={{ ...rowSpring, delay: index * 0.04 }}
    >
      <header className="provider-head">
        <Logo id={p.id} size={17} />
        <span className="provider-name">{p.name}</span>
        {p.plan && <span className="provider-plan">{p.plan}</span>}
        <PreviewPin id={p.id} name={p.name} pinned={pinned} />
      </header>
      <div className="card">
        <Notice p={p} now={now} />
        {meters.map((m) => (
          <Meter key={m.id} m={m} now={now} />
        ))}
        {hasSpend && <Trend values={trend(p.spend, now)} />}
        {canExpand && (
          <>
            <AnimatePresence initial={false}>
              {expanded && (
                <motion.div
                  className="more"
                  initial={{ height: 0, opacity: 0 }}
                  animate={{ height: "auto", opacity: 1 }}
                  exit={{ height: 0, opacity: 0 }}
                  transition={pageSpring}
                >
                  <div className="more-inner">
                    {hasSpend && <SpendRows p={p} now={now} />}
                    {extras.map((m) => (
                      <div key={m.id} className="row">
                        <span className="row-label">{m.label}</span>
                        <span className="row-value">{extraValue(m)}</span>
                      </div>
                    ))}
                  </div>
                </motion.div>
              )}
            </AnimatePresence>
            <button className={`caret ${expanded ? "open" : ""}`} onClick={toggle} aria-expanded={expanded} aria-label={expanded ? "Show less" : "Show more"}>
              <ChevronDown size={16} />
            </button>
          </>
        )}
      </div>
    </motion.section>
  );
}

/** Pins a provider to Holster's preview. Shown on hover; stays lit once pinned,
 *  and hovering it then previews the unpin. */
function PreviewPin({ id, name, pinned }: { id: string; name: string; pinned: boolean }) {
  const update = useStore((s) => s.updateSettings);
  const [hover, setHover] = useState(false);
  const Icon = pinned && hover ? PinOff : Pin;
  const toggle = () => {
    const list = useStore.getState().settings?.pinned ?? [];
    update({ pinned: pinned ? list.filter((x) => x !== id) : [...list, id] });
  };
  const label = pinned ? `Unpin ${name} from Holster's preview` : `Pin ${name} to Holster's preview`;
  return (
    <button
      className={`preview-pin ${pinned ? "on" : ""}`}
      onClick={toggle}
      onPointerEnter={() => setHover(true)}
      onPointerLeave={() => setHover(false)}
      aria-pressed={pinned}
      aria-label={label}
      title={label}
    >
      <Icon size={13} fill={pinned && Icon === Pin ? "currentColor" : "none"} />
    </button>
  );
}

function Notice({ p, now }: { p: ProviderState; now: number }) {
  if (p.status === "signedOut")
    return (
      <div className="notice">
        <LogIn size={14} />
        <span>{p.message ?? "Not signed in"}</span>
      </div>
    );
  if (p.status === "error")
    return (
      <div className="notice">
        <TriangleAlert size={14} />
        <span>{p.message ?? "Couldn't check usage"}</span>
      </div>
    );
  if (p.status === "loading" && p.metrics.length === 0) return <div className="notice faint">Checking your limits…</div>;
  if (p.status === "ok" && p.metrics.length === 0)
    return <div className="notice faint">{p.spend ? "No plan limits. Spend from your logs" : "No plan limits to show"}</div>;
  if (p.status === "stale" && p.message)
    return (
      <div className="notice warn">
        <TriangleAlert size={14} />
        <span>
          {p.message}
          {p.updatedAt ? `. Numbers from ${ago(p.updatedAt, now)}` : ""}
        </span>
      </div>
    );
  return null;
}

function Meter({ m, now }: { m: Metric; now: number }) {
  const showLeft = useStore((s) => s.settings?.showLeft ?? false);
  const exact = useStore((s) => s.settings?.exactResets ?? false);
  const update = useStore((s) => s.updateSettings);
  const f = fill(m) ?? 0;
  const pc = pace(m, now);
  const flipLeft = () => update({ showLeft: !showLeft });
  const flipExact = () => update({ exactResets: !exact });

  let headline: string;
  if (m.kind === "money") {
    headline = showLeft ? `${money(Math.max(0, (m.limit ?? 0) - m.used), m.currency)} left` : `${money(m.used, m.currency)} of ${money(m.limit ?? 0, m.currency)}`;
  } else if (m.kind === "count") {
    const unit = m.unit ? ` ${m.unit}` : "";
    headline = showLeft ? `${count(Math.max(0, (m.limit ?? 0) - m.used))}${unit} left` : `${count(m.used)} of ${count(m.limit ?? 0)}${unit}`;
  } else {
    headline = showLeft ? `${Math.max(0, Math.round(100 - f))}% left` : `${Math.round(f)}% used`;
  }
  const reset = m.resetsAt ? resetLabel(m.resetsAt, now, exact) : m.id === "session" ? "Not started" : null;

  return (
    <div className={`meter ${pc.verdict}`}>
      <div className="meter-top">
        <span className="meter-label">{m.label}</span>
        {pc.verdict === "close" && pc.spare != null && <span className="meter-note">~{pc.spare}% spare</span>}
        {pc.verdict === "over" && (pc.reached || pc.runsOutAt) && (
          <button className="meter-note over" onClick={pc.reached ? undefined : flipExact}>
            <Flame size={12} />
            {pc.reached ? "Limit reached" : `Limit ${exact ? exactTime(pc.runsOutAt!, now) : `in ${countdown(pc.runsOutAt!, now)}`}`}
          </button>
        )}
      </div>
      <div className="bar">
        <motion.div className="bar-fill" initial={false} animate={{ width: `${f > 0 ? Math.max(f, 1.2) : 0}%` }} transition={pageSpring} />
        {pc.verdict !== "ok" && pc.elapsed != null && <span className="pace-tick" style={{ left: `${pc.elapsed * 100}%` }} />}
      </div>
      <div className="meter-foot">
        <button className="link" onClick={flipLeft}>
          {headline}
        </button>
        {reset && (
          <button className="link faint" onClick={m.resetsAt ? flipExact : undefined}>
            {reset}
          </button>
        )}
      </div>
    </div>
  );
}

/** 30 days of tokens, one bar per day. */
function Trend({ values }: { values: number[] }) {
  const max = Math.max(...values, 1);
  const w = 3;
  const gap = 2;
  const h = 18;
  return (
    <div className="row trend">
      <span className="row-label strong">Usage trend</span>
      <svg width={values.length * (w + gap) - gap} height={h} aria-hidden="true">
        {values.map((v, i) =>
          v > 0 ? (
            <rect key={i} x={i * (w + gap)} y={h - Math.max(2, (v / max) * h)} width={w} height={Math.max(2, (v / max) * h)} rx={1} className="trend-bar" />
          ) : (
            <rect key={i} x={i * (w + gap) + 0.5} y={h - 1.5} width={w - 1} height={1.5} rx={0.75} className="trend-zero" />
          ),
        )}
      </svg>
    </div>
  );
}

function SpendRows({ p, now }: { p: ProviderState; now: number }) {
  const rows: { label: string; period: Period }[] = [
    { label: "Today", period: "today" },
    { label: "Yesterday", period: "yesterday" },
    { label: "Last 30 days", period: "month" },
  ];
  return (
    <>
      {rows.map((r) => {
        const t = periodTotal(p.spend, r.period, now);
        return (
          <div key={r.period} className="row">
            <span className="row-label">{r.label}</span>
            <span className={`row-value ${t ? "" : "faint"}`}>{t ? `${money(t.cost, "USD", true)} · ${tokens(t.tokens)} tokens` : "No data"}</span>
          </div>
        );
      })}
      {p.spend && p.spend.unpriced.length > 0 && (
        <div className="row warn-row">
          <TriangleAlert size={12} />
          <span>Not priced: {p.spend.unpriced.join(", ")}</span>
        </div>
      )}
    </>
  );
}

function extraValue(m: Metric): string {
  if (m.kind === "money") return m.limit ? `${money(m.used, m.currency)} of ${money(m.limit, m.currency)}` : money(m.used, m.currency);
  return `${count(m.used)} ${m.unit ?? ""}`.trim();
}

const count = (n: number) => n.toLocaleString("en-US", { maximumFractionDigits: 2 });

// ---- Footer ------------------------------------------------------------------------

const Footer = ({ ref, now }: { ref: React.Ref<HTMLElement>; now: number }) => {
  const usage = useStore((s) => s.usage);
  const minutes = useStore((s) => s.settings?.refreshMinutes ?? 5);
  const setRoute = useStore((s) => s.setRoute);
  const [version, setVersion] = useState("");
  useEffect(() => {
    api.appInfo().then((i) => setVersion(i.version));
  }, []);

  const next = usage?.checkedAt && minutes > 0 ? usage.checkedAt + minutes * 60_000 : null;
  const status = usage?.refreshing
    ? "Updating…"
    : next
      ? next > now
        ? `Next update in ${countdown(next, now)}`
        : "Updating soon"
      : usage?.checkedAt
        ? `Updated ${ago(usage.checkedAt, now)}`
        : "";

  return (
    <footer className="footer" ref={ref}>
      <button className="footer-status" onClick={() => api.refresh()} disabled={usage?.refreshing}>
        <span>Quota {version}</span>
        <span className="faint">{status}</span>
      </button>
      <PinButton />
      <button className="pill-btn" onClick={() => setRoute({ name: "settings" })}>
        <Settings2 size={14} />
        Settings
      </button>
    </footer>
  );
};
