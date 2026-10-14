import { ChevronLeft } from "lucide-react";
import { useCallback, useEffect, useRef, useState } from "react";
import { api, type HolsterStatus, type KeyStatus, type ProviderState } from "../lib/api";
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

        <Section title="Updates">
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
          {settings.refreshMinutes > 0 ? (
            <>
              <Row label="Notify at" hint="Holster tells you when a limit passes this or runs out">
                <Segmented
                  id="alert"
                  value={String(settings.alertAt)}
                  onChange={(v) => update({ alertAt: Number(v) })}
                  options={[
                    { value: "0", label: "Off" },
                    { value: "80", label: "80%" },
                    { value: "90", label: "90%" },
                  ]}
                />
              </Row>
              <Row label="Notify on reset" hint="When a limit you were close to resets">
                <Toggle label="Notify on reset" on={settings.notifyOnReset} onChange={(v) => update({ notifyOnReset: v })} />
              </Row>
            </>
          ) : (
            <Row label="Notifications" hint="Need background checks: pick 5 or 15 min to get them" />
          )}
        </Section>

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

        <div className="about">
          Quota {version} · reads the logins already on this computer and sends them only to each provider. Costs are estimates at API rates.
        </div>
      </div>}
      </div>
    </div>
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
