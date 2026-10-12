import { AnimatePresence, motion } from "framer-motion";
import { useLayoutEffect, useRef } from "react";
import { api } from "../lib/api";
import { useStore, type Route } from "../lib/store";
import { Home } from "./Home";
import { SettingsPage } from "./Settings";

const depth = (r: Route) => (r.name === "home" ? 0 : 1);

/** The panel is as tall as its content: a page reports the natural height of
 *  its parts and the window springs to it (scrolling only past the screen). */
export function useReportHeight(...refs: React.RefObject<HTMLElement | null>[]) {
  useLayoutEffect(() => {
    const els = refs.map((r) => r.current).filter((e): e is HTMLElement => !!e);
    let frame = 0;
    let last = 0;
    const report = () => {
      cancelAnimationFrame(frame);
      frame = requestAnimationFrame(() => {
        const h = Math.ceil(els.reduce((sum, e) => sum + e.getBoundingClientRect().height, 0));
        if (h !== last) {
          last = h;
          api.setHeight(h);
        }
      });
    };
    const ro = new ResizeObserver(report);
    els.forEach((e) => ro.observe(e));
    report();
    return () => {
      ro.disconnect();
      cancelAnimationFrame(frame);
    };
  }, []);
}

/** The open state. Content is laid out at the panel's final size and anchored
 *  to the top-right, so while the window morphs it is revealed, not reflowed. */
export function Panel() {
  const shell = useStore((s) => s.shell);
  const route = useStore((s) => s.route);
  const visible = shell.mode === "open";
  const prevDepth = useRef(depth(route));
  const dir = depth(route) >= prevDepth.current ? 1 : -1;
  prevDepth.current = depth(route);

  return (
    <div className="panel-root" style={{ width: `max(100%, ${shell.panelWidth}px)`, height: `max(100%, ${shell.panelHeight}px)` }}>
      <motion.div
        className="panel"
        initial={false}
        animate={visible ? { opacity: 1, filter: "blur(0px)", scale: 1 } : { opacity: 0, filter: "blur(6px)", scale: 0.985 }}
        transition={visible ? { duration: 0.32, ease: [0.16, 1, 0.3, 1] } : { duration: 0.09, ease: "easeIn" }}
        style={{ pointerEvents: visible ? "auto" : "none" }}
      >
        <button className="edge-handle" title="Close (Esc)" aria-label="Close" onClick={() => api.close()} />
        <AnimatePresence mode="popLayout" initial={false} custom={dir}>
          <motion.div
            key={route.name}
            className="route"
            custom={dir}
            variants={{
              enter: (d: number) => ({ opacity: 0, x: d * 28 }),
              center: { opacity: 1, x: 0 },
              exit: (d: number) => ({ opacity: 0, x: d * -20 }),
            }}
            initial="enter"
            animate="center"
            exit="exit"
            transition={{ type: "spring", stiffness: 420, damping: 40, opacity: { duration: 0.18 } }}
          >
            {route.name === "home" && <Home />}
            {route.name === "settings" && <SettingsPage />}
          </motion.div>
        </AnimatePresence>
        <Toasts />
      </motion.div>
    </div>
  );
}

function Toasts() {
  const toasts = useStore((s) => s.toasts);
  return (
    <div className="toasts">
      <AnimatePresence initial={false}>
        {toasts.map((t) => (
          <motion.div
            key={t.id}
            layout
            className="toast"
            initial={{ opacity: 0, y: 16, scale: 0.96 }}
            animate={{ opacity: 1, y: 0, scale: 1 }}
            exit={{ opacity: 0, y: 8, scale: 0.97, transition: { duration: 0.15 } }}
            transition={{ type: "spring", stiffness: 500, damping: 36 }}
          >
            <span>{t.text}</span>
          </motion.div>
        ))}
      </AnimatePresence>
    </div>
  );
}
