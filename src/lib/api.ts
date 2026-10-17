import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";

export type MetricKind = "percent" | "money" | "count";

export interface Metric {
  id: string;
  label: string;
  kind: MetricKind;
  used: number;
  limit: number | null;
  currency: string | null;
  /** Epoch milliseconds. */
  resetsAt: number | null;
  periodMs: number | null;
  /** A plan limit: drawn as a meter and counted for alerts. */
  primary: boolean;
  unit: string | null;
}

export interface DaySpend {
  /** Local day, "YYYY-MM-DD". */
  date: string;
  cost: number;
  tokens: number;
}

export interface Spend {
  days: DaySpend[];
  unpriced: string[];
}

export type ProviderStatus = "loading" | "ok" | "stale" | "signedOut" | "error";

export interface ProviderState {
  id: string;
  name: string;
  /** Palette name. */
  color: string;
  /** Lucide icon name. */
  icon: string;
  enabled: boolean;
  status: ProviderStatus;
  message: string | null;
  plan: string | null;
  metrics: Metric[];
  updatedAt: number | null;
  checkedAt: number | null;
  spend: Spend | null;
  retryAt: number | null;
}

export interface Snapshot {
  providers: ProviderState[];
  refreshing: boolean;
  checkedAt: number | null;
}

export interface Settings {
  theme: "system" | "light" | "dark";
  hotkey: string;
  clickOutsideCloses: boolean;
  launchAtStartup: boolean;
  showLeft: boolean;
  exactResets: boolean;
  expanded: string[];
  /** Providers pinned to Holster's preview (none: the fullest three show). */
  pinned: string[];
  spendMetric: "cost" | "tokens";
  refreshMinutes: number;
  /** Provider id → on (absent: off). */
  providers: Record<string, boolean>;
  knownProviders: string[];
  /** Holster notifies when a limit passes one of these. */
  alerts: AlertRule[];
  notifyOnReset: boolean;
}

export type AlertLimit = "any" | "session" | "weekly" | "monthly";

/** "Notify me when {limit} reaches {at}%" (100: when it runs out). */
export interface AlertRule {
  at: number;
  limit: AlertLimit;
}

export type ShellMode = "closed" | "opening" | "open" | "closing";

export interface ShellState {
  mode: ShellMode;
  scale: number;
  panelWidth: number;
  panelHeight: number;
}

export interface HolsterStatus {
  installed: boolean;
  registered: boolean;
  error: string | null;
  warnings: string[];
  port: number;
}

/** Where a key-based provider's key comes from. The key itself never reaches the UI. */
export interface KeyStatus {
  id: string;
  source: "saved" | "env" | "none";
  env: string[];
}

export type DeepPartial<T> = { [K in keyof T]?: T[K] extends object ? DeepPartial<T[K]> : T[K] };

export const api = {
  shellState: () => invoke<ShellState>("shell_state"),
  close: () => invoke("shell_close"),
  setHeight: (height: number) => invoke("shell_set_height", { height }),

  snapshot: () => invoke<Snapshot>("usage_snapshot"),
  refresh: () => invoke("usage_refresh"),

  getSettings: () => invoke<Settings>("get_settings"),
  updateSettings: (patch: DeepPartial<Settings>) => invoke<Settings>("update_settings", { patch }),
  holsterStatus: () => invoke<HolsterStatus>("holster_status"),
  providerKeys: () => invoke<KeyStatus[]>("provider_keys"),
  setProviderKey: (id: string, key: string) => invoke<Settings>("set_provider_key", { id, key }),
  clearProviderKey: (id: string) => invoke("clear_provider_key", { id }),
  appInfo: () => invoke<{ version: string; hotkeyError: string | null }>("app_info"),
  updateCheck: () => invoke<{ version: string; notes: string | null } | null>("update_check"),
  updateInstall: () => invoke("update_install"),
};

export function on<T>(event: string, handler: (payload: T) => void): () => void {
  let un: UnlistenFn | null = null;
  let dead = false;
  listen<T>(event, (e) => handler(e.payload)).then((u) => {
    if (dead) u();
    else un = u;
  });
  return () => {
    dead = true;
    un?.();
  };
}
