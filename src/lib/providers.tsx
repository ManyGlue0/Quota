import claude from "../assets/providers/claude.svg?raw";
import codex from "../assets/providers/codex.svg?raw";
import cursor from "../assets/providers/cursor.svg?raw";

/** Each provider's mark (from OpenUsage's set) and brand colors. `ring` is its
 *  Total Spend segment; `logo` tints the mark (`null`: the text color). */
const BRANDS: Record<string, { svg: string; logo: string | null; ring: [string, string] }> = {
  claude: { svg: claude, logo: "#DE7356", ring: ["#DE7356", "#DE7356"] },
  codex: { svg: codex, logo: null, ring: ["#10A37F", "#10A37F"] },
  cursor: { svg: cursor, logo: null, ring: ["#13120A", "#F5F5F7"] },
};

export function ringColor(id: string, dark: boolean): string {
  const b = BRANDS[id];
  return b ? b.ring[dark ? 1 : 0] : "#8897AB";
}

/** The provider's logo, sized by CSS (`.logo` fills its box). Bundled SVGs only. */
export function Logo({ id, size = 16 }: { id: string; size?: number }) {
  const b = BRANDS[id];
  if (!b) return null;
  return (
    <span
      className="logo"
      aria-hidden="true"
      style={{ width: size, height: size, color: b.logo ?? "var(--text)" }}
      dangerouslySetInnerHTML={{ __html: b.svg }}
    />
  );
}
