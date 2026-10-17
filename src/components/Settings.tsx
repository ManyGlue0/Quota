import { ChevronLeft, Download, Minus, Plus, RefreshCw, X } from "lucide-react";
import { useCallback, useEffect, useRef, useState } from "react";
import { api, on, type AlertLimit, type AlertRule, type HolsterStatus, type KeyStatus, type ProviderState } from "../lib/api";
import { Logo } from "../lib/providers";
import { useStore } from "../lib/store";
import { useReportHeight } from "./Panel";
import { Dot, Row, Section } from "./settingsUi";
import { Button, IconButton, PinButton, Segmented, Toggle } from "./ui";

export function SettingsPage() {
  const settings = useStore((s) => s.settings);
  const usage = useStore((s) => s.usage);
  const update = useStore((s) => s.updateSettings);
  const setRoute = useStore((s) => s.setRoute);
  const [version, setVersion] = useState("");
  const [hotkeyError, setHotkeyError] = useState<string | null>(null);
  const head = useRef<HTMLElement>(null);
  const content = useRef<HTMLDivElement>(null);
  useReportHeight(head, content);

  useEffect(() => {
    api.appInfo().then((i) => {
      setVersion(i.version);
      setHotkeyError(i.hotkeyError);
    });
  }, [settings?.hotkey]);

  return (
    <div className="page">
      <header className="settings-bar" ref={head}>
        <IconButton label="Back" onClick={() => setRoute({ name: "home" })}>
          <ChevronLeft size={19} />
        </IconButton>
        <h1 className="bar-title">Settings</h1>
        {settings?.clickOutsideCloses ? <PinButton /> : <span className="icon-btn-space" />}
      </header>
      <div className="page-scroll">
      {settings && <div className="page-content settings-content" ref={content}>
        <ProvidersSection providers={usage?.providers ?? []} />

        <Section title="Refresh">
          <Row
            stack
            label="Check"
            hint={settings.refreshMinutes === 0 ? "When you hover the tile or open the panel. Nothing in the background" : "In the background, and when you look"}
          >
            <Segmented
              id="refresh"
              value={String(settings.refreshMinutes)}
              onChange={(v) => update({ refreshMinutes: Number(v) })}
              options={[
                { value: "0", label: "When I look" },
                { value: "5", label: "5 min" },
                { value: "15", label: "15 min" },
              ]}
            />
          </Row>
        </Section>

        <NotificationsSection />

        <Section title="Appearance">
          <Row label="Theme">
            <Segmented
              id="theme"
              value={settings.theme}
              onChange={(theme) => update({ theme })}
              options={[
                { value: "system", label: "System" },
                { value: "light", label: "Light" },
                { value: "dark", label: "Dark" },
              ]}
            />
          </Row>
        </Section>

        <Section title="Panel">
          <HolsterRow />
          <Row label="Shortcut" hint={hotkeyError ?? "Opens and closes Quota from anywhere"} warn={!!hotkeyError}>
            <HotkeyRecorder value={settings.hotkey} onChange={(hotkey) => update({ hotkey })} />
          </Row>
          <Row label="Close on click outside" hint="The pin keeps the panel open">
            <Toggle label="Close when clicking outside" on={settings.clickOutsideCloses} onChange={(v) => update({ clickOutsideCloses: v })} />
          </Row>
          <Row label="Launch at startup">
            <Toggle label="Launch at startup" on={settings.launchAtStartup} onChange={(v) => update({ launchAtStartup: v })} />
          </Row>
        </Section>

        <UpdatesSection version={version} />

        <div className="about">
          Quota {version} · reads the logins already on this computer and sends them only to each provider. Costs are estimates at API rates.
        </div>
      </div>}
      </div>
    </div>
  );
}

// ---- updates -----------------------------------------------------------------------

type UpdateState =
  | { step: "idle" | "checking" | "latest" | "installing" }
  | { step: "available"; version: string }
  | { step: "downloading"; version: string; pct: number | null }
  | { step: "error"; message: string };

/** Checks GitHub for a newer Quota when asked, and installs it on Update. */
function UpdatesSection({ version }: { version: string }) {
  const [state, setState] = useState<UpdateState>({ step: "idle" });

  useEffect(
    () =>
      on<{ downloaded: number; total: number | null }>("update-progress", ({ downloaded, total }) =>
        setState((s) =>
          s.step === "downloading" ? { ...s, pct: total ? Math.min(100, Math.round((downloaded / total) * 100)) : null } : s,
        ),
      ),
    [],
  );

  const check = async () => {
    setState({ step: "checking" });
    try {
      const found = await api.updateCheck();
      setState(found ? { step: "available", version: found.version } : { step: "latest" });
    } catch (e) {
      setState({ step: "error", message: String(e) });
    }
  };
  const install = async (version: string) => {
    setState({ step: "downloading", version, pct: 0 });
    try {
      await api.updateInstall();
      setState({ step: "installing" });
    } catch (e) {
      setState({ step: "error", message: String(e) });
    }
  };

  const hint =
    state.step === "checking"
      ? "Checking…"
      : state.step === "latest"
        ? "You have the latest version"
        : state.step === "available"
          ? `Version ${state.version} is ready. Quota restarts by itself`
          : state.step === "downloading"
            ? `Downloading ${state.version}${state.pct === null ? "…" : `, ${state.pct}%`}`
            : state.step === "installing"
              ? "Installing. Quota restarts by itself"
              : state.step === "error"
                ? state.message
                : "Looks for a newer version on GitHub";

  return (
    <Section title="Updates">
      <Row label={version ? `Quota ${version}` : "Quota"} hint={hint} warn={state.step === "error"}>
        {state.step === "available" ? (
          <Button variant="primary" onClick={() => install(state.version)}>
            <Download size={14} /> Update
          </Button>
        ) : (
          <Button
            variant="ghost"
            disabled={state.step === "checking" || state.step === "downloading" || state.step === "installing"}
            onClick={check}
          >
            <RefreshCw size={14} className={state.step === "checking" ? "spin" : ""} /> Check
          </Button>
        )}
      </Row>
    </Section>
  );
}

/** Every provider, in OpenUsage's order, with its on switch. Key-based
 *  providers (OpenRouter, Z.ai) take their API key right in their row. */
function ProvidersSection({ providers }: { providers: ProviderState[] }) {
  const settings = useStore((s) => s.settings);
  const update = useStore((s) => s.updateSettings);
  const [keys, setKeys] = useState<KeyStatus[]>([]);
  const reload = useCallback(() => {
    api.providerKeys().then(setKeys);
  }, []);
  useEffect(reload, [reload]);
  if (!settings) return null;
  return (
    <Section title="Providers">
      {providers.map((p) => {
        const on = settings.providers[p.id] ?? false;
        const key = keys.find((k) => k.id === p.id);
        return (
          <div key={p.id} className="provider-setting">
            <Row
              icon={<Logo id={p.id} size={16} />}
              label={p.name}
              hint={providerHint(p, on, key)}
              warn={on && (p.status === "error" || (p.status === "signedOut" && key?.source !== "none"))}
            >
              <Toggle label={p.name} on={on} onChange={(v) => update({ providers: { [p.id]: v } })} />
            </Row>
            {key && on && <KeyEditor name={p.name} status={key} onChange={reload} />}
          </div>
        );
      })}
    </Section>
  );
}

const KEY_PAGES: Record<string, string> = {
  openrouter: "openrouter.ai/keys",
  zai: "z.ai/manage-apikey",
};

function KeyEditor({ name, status, onChange }: { name: string; status: KeyStatus; onChange: () => void }) {
  const [draft, setDraft] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const toast = useStore((s) => s.toast);

  const save = async () => {
    setBusy(true);
    setError(null);
    try {
      useStore.setState({ settings: await api.setProviderKey(status.id, draft) });
      setDraft("");
      toast(`${name} key saved`);
      onChange();
    } catch (e) {
      setError(String(e));
    }
    setBusy(false);
  };
  const remove = async () => {
    setBusy(true);
    try {
      await api.clearProviderKey(status.id);
      toast(`${name} key removed`);
      onChange();
    } catch (e) {
      setError(String(e));
    }
    setBusy(false);
  };

  if (status.source === "saved")
    return (
      <div className="key-row saved">
        <span className="key-note">Key saved in Windows Credential Manager</span>
        <Button variant="ghost" className="key-btn" disabled={busy} onClick={remove}>
          Remove
        </Button>
      </div>
    );
  return (
    <form
      className="key-row"
      onSubmit={(e) => {
        e.preventDefault();
        if (draft.trim()) save();
      }}
    >
      <div className="key-field">
        <input
          className="text-input mono key-input"
          type="password"
          autoComplete="off"
          spellCheck={false}
          placeholder={status.source === "env" ? `Using ${status.env[0]}` : "Paste your API key"}
          value={draft}
          onChange={(e) => setDraft(e.target.value)}
        />
        <Button variant="primary" className="key-btn" type="submit" disabled={busy || !draft.trim()}>
          Save
        </Button>
      </div>
      {error ? (
        <span className="key-note warn">{error}</span>
      ) : (
        <span className="key-note">
          {status.source === "env" ? "Paste one here to use it instead" : `Make one at ${KEY_PAGES[status.id] ?? name}. Kept in Windows Credential Manager`}
        </span>
      )}
    </form>
  );
}

function providerHint(p: ProviderState, on: boolean, key?: KeyStatus): string {
  if (!on) return "Off";
  if (key?.source === "none") return "Needs an API key";
  switch (p.status) {
    case "ok":
    case "stale":
      return p.plan ? `Signed in · ${p.plan}` : "Signed in";
    case "signedOut":
      return p.message ?? "Not signed in";
    case "error":
      return p.message ?? "Couldn't check usage";
    default:
      return "Checking…";
  }
}

const ALERT_LIMITS: { value: AlertLimit; label: string; name: string }[] = [
  { value: "any", label: "Any", name: "Any limit" },
  { value: "session", label: "5-hour", name: "5-hour limit" },
  { value: "weekly", label: "Weekly", name: "Weekly limit" },
  { value: "monthly", label: "Monthly", name: "Monthly limit" },
];
const MAX_ALERTS = 8;

/** Holster notifications: a list of "this limit reaches this much", plus resets. */
function NotificationsSection() {
  const settings = useStore((s) => s.settings);
  const update = useStore((s) => s.updateSettings);
  if (!settings) return null;
  const rules = settings.alerts;
  const set = (alerts: AlertRule[]) => update({ alerts });
  const change = (i: number, patch: Partial<AlertRule>) => set(rules.map((r, j) => (j === i ? { ...r, ...patch } : r)));
  return (
    <Section title="Notifications">
      {settings.refreshMinutes === 0 && (
        <Row label="Background checks are off" hint="Notifications need them" warn>
          <Button variant="default" className="key-btn" onClick={() => update({ refreshMinutes: 5 })}>
            Every 5 min
          </Button>
        </Row>
      )}
      {rules.map((r, i) => (
        <div key={i} className="alert-rule">
          <div className="alert-rule-head">
            <span className="alert-rule-text">
              {ALERT_LIMITS.find((l) => l.value === r.limit)?.name ?? "Any limit"} {r.at >= 100 ? "runs out" : "reaches"}
            </span>
            <PercentStepper value={r.at} onChange={(at) => change(i, { at })} />
            <IconButton label="Remove" className="small danger" onClick={() => set(rules.filter((_, j) => j !== i))}>
              <X size={14} />
            </IconButton>
          </div>
          <Segmented id={`alert-${i}`} value={r.limit} onChange={(limit) => change(i, { limit })} options={ALERT_LIMITS} />
        </div>
      ))}
      <div className="alert-add">
        <Button
          variant="ghost"
          className="key-btn"
          disabled={rules.length >= MAX_ALERTS}
          onClick={() => set([...rules, { at: rules.length ? 90 : 80, limit: "any" }])}
        >
          <Plus size={14} />
          Add notification
        </Button>
      </div>
      <Row label="When a limit resets" hint="If you were close to running out">
        <Toggle label="Notify when a limit resets" on={settings.notifyOnReset} onChange={(v) => update({ notifyOnReset: v })} />
      </Row>
    </Section>
  );
}

function PercentStepper({ value, onChange }: { value: number; onChange: (v: number) => void }) {
  // Steps of 5, from wherever the value is.
  const down = Math.max(5, Math.ceil(value / 5) * 5 - 5);
  const up = Math.min(100, Math.floor(value / 5) * 5 + 5);
  return (
    <div className="stepper">
      <button aria-label="Lower" disabled={value <= 5} onClick={() => onChange(down)}>
        <Minus size={13} />
      </button>
      <output>{value}%</output>
      <button aria-label="Raise" disabled={value >= 100} onClick={() => onChange(up)}>
        <Plus size={13} />
      </button>
    </div>
  );
}

/** Quota lives in Holster's dock: say whether it's there, or what's wrong. */
function HolsterRow() {
  const [st, setSt] = useState<HolsterStatus | null>(null);
  useEffect(() => {
    api.holsterStatus().then(setSt);
  }, []);
  if (!st) return null;
  const problem = st.error ?? (st.warnings.length ? st.warnings.join(" ") : null);
  const hint = !st.installed
    ? "Not installed. The shortcut and tray icon open Quota"
    : (problem ?? (st.registered ? "Hover the tile to peek, click to open" : "Joins the dock the next time Quota starts"));
  return (
    <Row label="Holster dock" hint={hint} warn={!!problem}>
      <Dot state={!st.installed ? "off" : st.error ? "error" : problem || !st.registered ? "warn" : "online"} />
    </Row>
  );
}

// ---- hotkey recorder -----------------------------------------------------------------

const KEY_NAMES: Record<string, string> = {
  " ": "Space",
  ArrowUp: "Up",
  ArrowDown: "Down",
  ArrowLeft: "Left",
  ArrowRight: "Right",
  Escape: "Escape",
  Enter: "Enter",
  Tab: "Tab",
  Backspace: "Backspace",
};

function keyName(e: KeyboardEvent): string | null {
  if (["Control", "Alt", "Shift", "Meta"].includes(e.key)) return null;
  if (KEY_NAMES[e.key]) return KEY_NAMES[e.key];
  if (/^F\d{1,2}$/.test(e.key)) return e.key;
  if (e.code.startsWith("Key")) return e.code.slice(3);
  if (e.code.startsWith("Digit")) return e.code.slice(5);
  return e.key.length === 1 ? e.key.toUpperCase() : null;
}

function HotkeyRecorder({ value, onChange }: { value: string; onChange: (v: string) => void }) {
  const [recording, setRecording] = useState(false);
  const [preview, setPreview] = useState("");

  useEffect(() => {
    if (!recording) return;
    const down = (e: KeyboardEvent) => {
      e.preventDefault();
      e.stopPropagation();
      if (e.key === "Escape" && !e.ctrlKey && !e.altKey) {
        setRecording(false);
        return;
      }
      const mods = [e.ctrlKey && "Ctrl", e.altKey && "Alt", e.shiftKey && "Shift", e.metaKey && "Super"].filter(Boolean) as string[];
      const k = keyName(e);
      setPreview([...mods, k ?? ""].filter(Boolean).join("+"));
      if (k && mods.length > 0) {
        onChange([...mods, k].join("+"));
        setRecording(false);
      }
    };
    window.addEventListener("keydown", down, true);
    return () => window.removeEventListener("keydown", down, true);
  }, [recording, onChange]);

  return (
    <button className={`hotkey ${recording ? "recording" : ""}`} onClick={() => (setPreview(""), setRecording((r) => !r))}>
      {recording ? preview || "Press keys…" : value.split("+").map((k) => <kbd key={k}>{k}</kbd>)}
    </button>
  );
}
