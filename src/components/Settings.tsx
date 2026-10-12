import { ChevronLeft } from "lucide-react";
import { useEffect, useRef, useState } from "react";
import { api, type HolsterStatus, type ProviderState, type Settings } from "../lib/api";
import { Logo } from "../lib/providers";
import { useStore } from "../lib/store";
import { useReportHeight } from "./Panel";
import { Dot, Row, Section } from "./settingsUi";
import { IconButton, PinButton, Segmented, Toggle } from "./ui";

type ProviderId = keyof Settings["providers"];

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
        <Section title="Providers">
          {(usage?.providers ?? []).map((p) => (
            <Row key={p.id} icon={<Logo id={p.id} size={16} />} label={p.name} hint={providerHint(p, settings.providers[p.id as ProviderId])} warn={p.enabled && (p.status === "error" || p.status === "signedOut")}>
              <Toggle
                label={p.name}
                on={settings.providers[p.id as ProviderId]}
                onChange={(v) => update({ providers: { [p.id]: v } })}
              />
            </Row>
          ))}
        </Section>

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
              <Row label="Glow at" hint="Holster's tab glows when a limit passes this">
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
              <Row label="Glow on reset" hint="When a limit you were close to resets">
                <Toggle label="Glow on reset" on={settings.glowOnReset} onChange={(v) => update({ glowOnReset: v })} />
              </Row>
            </>
          ) : (
            <Row label="Glow alerts" hint="Need background checks: pick 5 or 15 min to get them" />
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

function providerHint(p: ProviderState, on: boolean): string {
  if (!on) return "Off";
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
