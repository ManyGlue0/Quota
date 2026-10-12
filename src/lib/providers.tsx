/** Provider marks (scripts/normalize-logos.mjs: OpenUsage's set, fitted to a
 *  common box) and brand colors. */
const SVGS = import.meta.glob<string>("../assets/providers/*.svg", { query: "?raw", import: "default", eager: true });
const svg = (id: string) => SVGS[`../assets/providers/${id}.svg`];

/** `ring`: the Total Spend segment [light, dark] (OpenUsage's palette; brands
 *  in plain black get a gray that reads in both themes). `logo` tints the
 *  mark (`null`: the text color). */
const BRANDS: Record<string, { logo: string | null; ring: [string, string] }> = {
  claude: { logo: "#DE7356", ring: ["#DE7356", "#DE7356"] },
  codex: { logo: null, ring: ["#10A37F", "#10A37F"] },
  cursor: { logo: null, ring: ["#13120A", "#F5F5F7"] },
  antigravity: { logo: "#4285F4", ring: ["#4285F4", "#4285F4"] },
  copilot: { logo: null, ring: ["#A855F7", "#A855F7"] },
  devin: { logo: null, ring: ["#FF2D55", "#FF2D55"] },
  grok: { logo: null, ring: ["#8E8E93", "#98989D"] },
  muse: { logo: "#0082FB", ring: ["#0082FB", "#0082FB"] },
  ollama: { logo: null, ring: ["#5856D6", "#5856D6"] },
  opencode: { logo: null, ring: ["#6E6E73", "#AEAEB2"] },
  openrouter: { logo: null, ring: ["#6467F2", "#6467F2"] },
  zai: { logo: null, ring: ["#2D2D2D", "#D1D1D6"] },
};

export function ringColor(id: string, dark: boolean): string {
  const b = BRANDS[id];
  return b ? b.ring[dark ? 1 : 0] : "#8897AB";
}

/** The provider's logo, sized by CSS (`.logo` fills its box). Bundled SVGs only. */
export function Logo({ id, size = 16 }: { id: string; size?: number }) {
  const markup = svg(id);
  if (!markup) return null;
  return (
    <span
      className="logo"
      aria-hidden="true"
      style={{ width: size, height: size, color: BRANDS[id]?.logo ?? "var(--text)" }}
      dangerouslySetInnerHTML={{ __html: markup }}
    />
  );
}
