import { AnimatePresence, motion } from "framer-motion";
import { Pin, PinOff } from "lucide-react";
import { forwardRef, useState } from "react";
import { useStore } from "../lib/store";

export const spring = { type: "spring", stiffness: 520, damping: 38, mass: 0.8 } as const;
export const softSpring = { type: "spring", stiffness: 300, damping: 30 } as const;

// ---- buttons --------------------------------------------------------------

type IconButtonProps = React.ButtonHTMLAttributes<HTMLButtonElement> & { label: string; active?: boolean };

export const IconButton = forwardRef<HTMLButtonElement, IconButtonProps>(function IconButton(
  { label, className, active, children, ...rest },
  ref,
) {
  return (
    <button
      ref={ref}
      aria-label={label}
      title={label}
      className={`icon-btn ${active ? "active" : ""} ${className ?? ""}`}
      {...rest}
    >
      {children}
    </button>
  );
});

/** Keeps the panel open on click outside. Shown only while that behavior is
 *  on; pinned, it stays lit and hovering previews the unpin. */
export function PinButton() {
  const enabled = useStore((s) => s.settings?.clickOutsideCloses ?? true);
  const pinned = useStore((s) => s.pinned);
  const setPinned = useStore((s) => s.setPinned);
  const [hover, setHover] = useState(false);
  if (!enabled) return null;
  const Icon = pinned && hover ? PinOff : Pin;
  return (
    <IconButton
      label={pinned ? "Unpin" : "Keep open"}
      active={pinned}
      className="pin-btn"
      aria-pressed={pinned}
      onPointerEnter={() => setHover(true)}
      onPointerLeave={() => setHover(false)}
      onClick={() => setPinned(!pinned)}
    >
      <AnimatePresence mode="popLayout" initial={false}>
        <motion.span
          key={`${pinned}-${Icon === PinOff}`}
          className="pin-icon"
          initial={{ opacity: 0, scale: 0.6, rotate: pinned ? -30 : 30 }}
          animate={{ opacity: 1, scale: 1, rotate: pinned ? 0 : 45 }}
          exit={{ opacity: 0, scale: 0.6, transition: { duration: 0.1 } }}
          transition={spring}
        >
          <Icon size={16} fill={pinned && Icon === Pin ? "currentColor" : "none"} />
        </motion.span>
      </AnimatePresence>
    </IconButton>
  );
}

export function Button({
  variant = "default",
  className,
  ...rest
}: React.ButtonHTMLAttributes<HTMLButtonElement> & { variant?: "default" | "primary" | "ghost" | "danger" }) {
  return <button className={`btn btn-${variant} ${className ?? ""}`} {...rest} />;
}

// ---- small controls ------------------------------------------------------------

export function Toggle({ on, onChange, label }: { on: boolean; onChange: (v: boolean) => void; label: string }) {
  return (
    <button role="switch" aria-checked={on} aria-label={label} className={`toggle ${on ? "on" : ""}`} onClick={() => onChange(!on)}>
      <motion.span className="toggle-knob" layout transition={spring} />
    </button>
  );
}

export function Segmented<T extends string>({
  value,
  options,
  onChange,
  id,
}: {
  value: T;
  options: { value: T; label: string }[];
  onChange: (v: T) => void;
  id: string;
}) {
  return (
    <div className="segmented">
      {options.map((o) => (
        <button key={o.value} className={o.value === value ? "on" : ""} onClick={() => onChange(o.value)}>
          {o.value === value && <motion.span layoutId={`seg-${id}`} className="segmented-pill" transition={spring} />}
          <span>{o.label}</span>
        </button>
      ))}
    </div>
  );
}
