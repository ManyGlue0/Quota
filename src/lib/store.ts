import { useEffect } from "react";
import { create } from "zustand";
import { api, type DeepPartial, type Settings, type ShellState, type Snapshot } from "./api";
import type { Period } from "./format";

export type Route = { name: "home" } | { name: "settings" };

export interface Toast {
  id: number;
  text: string;
}

interface Store {
  shell: ShellState;
  settings: Settings | null;
  systemDark: boolean;
  usage: Snapshot | null;
  route: Route;
  toasts: Toast[];
  /** The Total Spend card's period. */
  period: Period;
  /** Keeps the panel open on click outside. Session only: resets on close. */
  pinned: boolean;

  setShell: (s: ShellState) => void;
  setRoute: (r: Route) => void;
  setUsage: (u: Snapshot) => void;
  loadSettings: () => Promise<void>;
  updateSettings: (patch: DeepPartial<Settings>) => Promise<void>;
  toast: (text: string) => void;
  dismissToast: (id: number) => void;
  setPeriod: (p: Period) => void;
  setPinned: (on: boolean) => void;
}

let toastId = 0;

export const useStore = create<Store>((set, get) => ({
  shell: { mode: "closed", scale: 1, panelWidth: 340, panelHeight: 480 },
  settings: null,
  systemDark: window.matchMedia("(prefers-color-scheme: dark)").matches,
  usage: null,
  route: { name: "home" },
  toasts: [],
  period: "today",
  pinned: false,

  setShell: (shell) => set(shell.mode === "closing" || shell.mode === "closed" ? { shell, pinned: false } : { shell }),
  setRoute: (route) => set({ route }),
  setUsage: (usage) => set({ usage }),
  loadSettings: async () => set({ settings: await api.getSettings() }),
  updateSettings: async (patch) => {
    try {
      set({ settings: await api.updateSettings(patch) });
    } catch (e) {
      get().toast(String(e));
      await get().loadSettings();
    }
  },
  toast: (text) => {
    const id = ++toastId;
    set((s) => ({ toasts: [...s.toasts.slice(-2), { id, text }] }));
    setTimeout(() => get().dismissToast(id), 3200);
  },
  dismissToast: (id) => set((s) => ({ toasts: s.toasts.filter((t) => t.id !== id) })),
  setPeriod: (period) => set({ period }),
  setPinned: (pinned) => set({ pinned }),
}));

export function useDark(): boolean {
  const theme = useStore((s) => s.settings?.theme ?? "system");
  const systemDark = useStore((s) => s.systemDark);
  return theme === "dark" || (theme === "system" && systemDark);
}

// ---- Escape handling: innermost overlay first, then close the panel. ----

const escStack: (() => void)[] = [];

export function useEscape(active: boolean, handler: () => void) {
  useEffect(() => {
    if (!active) return;
    escStack.push(handler);
    return () => {
      const i = escStack.lastIndexOf(handler);
      if (i >= 0) escStack.splice(i, 1);
    };
  }, [active, handler]);
}

export function handleEscape(): boolean {
  const top = escStack[escStack.length - 1];
  if (top) {
    top();
    return true;
  }
  return false;
}

// ---- Click outside: the shell asks after a blur; these keep the panel open. ----

let composing = false;
document.addEventListener("compositionstart", () => (composing = true), true);
document.addEventListener("compositionend", () => (composing = false), true);

export function shouldCloseOnBlur(): boolean {
  const s = useStore.getState();
  if (s.shell.mode !== "open" || s.pinned || composing) return false;
  if (escStack.length > 0 || document.querySelector(".hotkey.recording, .popover")) return false;
  return true;
}
