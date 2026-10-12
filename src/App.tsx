import { useEffect, useLayoutEffect } from "react";
import { flushSync } from "react-dom";
import { api, on, type ShellState, type Snapshot } from "./lib/api";
import { handleEscape, shouldCloseOnBlur, useDark, useStore } from "./lib/store";
import { Panel } from "./components/Panel";

export default function App() {
  const dark = useDark();

  useEffect(() => {
    const s = useStore.getState();
    api.shellState().then(s.setShell);
    api.snapshot().then(s.setUsage);
    s.loadSettings();

    const media = window.matchMedia("(prefers-color-scheme: dark)");
    const onMedia = () => useStore.setState({ systemDark: media.matches });
    media.addEventListener("change", onMedia);

    const offs = [
      on<ShellState>("shell", (st) => {
        const store = useStore.getState();
        // Every opening starts on the usage page, unless the tray asked for settings.
        if (st.mode === "closed") store.setRoute({ name: "home" });
        store.setShell(st);
      }),
      on<Snapshot>("usage", (u) => useStore.getState().setUsage(u)),
      on<string>("navigate", (to) => {
        if (to === "settings") useStore.getState().setRoute({ name: "settings" });
      }),
      on("shell-blur", () => {
        if (shouldCloseOnBlur()) api.close();
      }),
    ];
    return () => {
      offs.forEach((off) => off());
      media.removeEventListener("change", onMedia);
    };
  }, []);

  // Theme: crossfade with a view transition when the value changes.
  useLayoutEffect(() => {
    const root = document.documentElement;
    const next = dark ? "dark" : "light";
    if (root.dataset.theme === next) return;
    const doc = document as Document & { startViewTransition?: (cb: () => void) => unknown };
    if (root.dataset.theme && doc.startViewTransition && useStore.getState().shell.mode === "open") {
      doc.startViewTransition(() => flushSync(() => (root.dataset.theme = next)));
    } else {
      root.dataset.theme = next;
    }
  }, [dark]);

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key !== "Escape" || e.defaultPrevented) return;
      if (handleEscape()) return;
      api.close();
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, []);

  return (
    <div className="panel-host">
      <Panel />
    </div>
  );
}
