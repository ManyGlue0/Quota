import { motion, useSpring, useTransform } from "framer-motion";
import { useEffect } from "react";

/** A donut segment: golden-ratio hole, hairline gaps, rounded corners. Angles
 *  are fractions of a turn, clockwise from noon. (OpenUsage's ring shape.) */
export function wedge(size: number, start: number, end: number, opts = { hole: 0.618, gap: 1.6, corner: 3 }): string {
  const R = size / 2;
  const r = R * opts.hole;
  const c0 = size / 2;
  const halfGap = opts.gap / R / 2;
  const a0 = start * 2 * Math.PI + halfGap;
  const a1 = end * 2 * Math.PI - halfGap;
  const width = a1 - a0;
  if (width <= 0.001) return "";
  const pt = (rho: number, a: number) => `${(c0 + rho * Math.sin(a)).toFixed(3)} ${(c0 - rho * Math.cos(a)).toFixed(3)}`;
  const large = (span: number) => (span > Math.PI ? 1 : 0);

  // Corners can't exceed the band, and a narrow slice shrinks them so the two
  // corners of one edge never cross.
  const s = Math.sin(Math.min(width / 2, Math.PI / 2));
  let k = Math.min(opts.corner, (R - r) / 2, (R * s) / (1 + s));
  if (s < 1) k = Math.min(k, (r * s) / (1 - s));

  if (k < 0.25) {
    return `M${pt(R, a0)} A${R} ${R} 0 ${large(width)} 1 ${pt(R, a1)} L${pt(r, a1)} A${r} ${r} 0 ${large(width)} 0 ${pt(r, a0)} Z`;
  }
  const bo = Math.asin(Math.min(1, k / (R - k)));
  const bi = Math.asin(Math.min(1, k / (r + k)));
  const eo = Math.sqrt((R - k) ** 2 - k * k); // where an outer corner meets the edge
  const ei = Math.sqrt((r + k) ** 2 - k * k);
  return [
    `M${pt(R, a0 + bo)}`,
    `A${R} ${R} 0 ${large(width - 2 * bo)} 1 ${pt(R, a1 - bo)}`,
    `A${k} ${k} 0 0 1 ${pt(eo, a1)}`,
    `L${pt(ei, a1)}`,
    `A${k} ${k} 0 0 1 ${pt(r, a1 - bi)}`,
    `A${r} ${r} 0 ${large(width - 2 * bi)} 0 ${pt(r, a0 + bi)}`,
    `A${k} ${k} 0 0 1 ${pt(ei, a0)}`,
    `L${pt(eo, a0)}`,
    `A${k} ${k} 0 0 1 ${pt(R, a0 + bo)}`,
    "Z",
  ].join(" ");
}

const arcSpring = { stiffness: 260, damping: 30, mass: 0.9 };

/** One provider's arc: its start and end spring to their new place, so a
 *  period switch slides and resizes each arc instead of redrawing the ring. */
function Arc({ size, start, end, color }: { size: number; start: number; end: number; color: string }) {
  const s = useSpring(start, arcSpring);
  const e = useSpring(end, arcSpring);
  useEffect(() => {
    s.set(start);
    e.set(end);
  }, [start, end, s, e]);
  const d = useTransform(() => wedge(size, s.get(), e.get()));
  return (
    <motion.path
      d={d}
      fill={color}
      initial={{ opacity: 0 }}
      animate={{ opacity: 1 }}
      exit={{ opacity: 0 }}
      transition={{ duration: 0.25 }}
    />
  );
}

export interface Slice {
  id: string;
  value: number;
  color: string;
}

/** Every slice keeps at least this share of the circle, so a tiny spender
 *  next to a dominant one still shows a sliver. Display only. */
const MIN_SHARE = 0.025;

export function SpendRing({ slices, size = 104, children }: { slices: Slice[]; size?: number; children?: React.ReactNode }) {
  const total = slices.reduce((t, s) => t + s.value, 0);
  const floored = slices.map((s) => Math.max(total > 0 ? s.value / total : 0, MIN_SHARE));
  const sum = floored.reduce((a, b) => a + b, 0) || 1;
  let acc = 0;
  const arcs = slices.map((s, i) => {
    const start = acc / sum;
    acc += floored[i];
    return { ...s, start, end: acc / sum };
  });
  return (
    <div className="ring" style={{ width: size, height: size }}>
      <svg width={size} height={size} viewBox={`0 0 ${size} ${size}`} aria-hidden="true">
        {total > 0 ? (
          arcs.map((a) => <Arc key={a.id} size={size} start={a.start} end={a.end} color={a.color} />)
        ) : (
          <circle
            cx={size / 2}
            cy={size / 2}
            r={(size / 2) * (1 + 0.618) / 2}
            strokeWidth={(size / 2) * (1 - 0.618)}
            className="ring-empty"
          />
        )}
      </svg>
      <div className="ring-center">{children}</div>
    </div>
  );
}
