export interface Swatch {
  bg: string;
  strong: string;
  ink: string;
}

export const COLOR_KEYS = [
  "mint",
  "peach",
  "lavender",
  "sky",
  "butter",
  "rose",
  "sage",
  "lilac",
  "apricot",
  "mist",
] as const;
export type ColorKey = (typeof COLOR_KEYS)[number];

const LIGHT: Record<ColorKey, Swatch> = {
  mint: { bg: "#E2F5EB", strong: "#46B086", ink: "#1D5741" },
  peach: { bg: "#FDE7DC", strong: "#E8916A", ink: "#773C20" },
  lavender: { bg: "#ECE7FB", strong: "#8D79DF", ink: "#42338B" },
  sky: { bg: "#E1EEFC", strong: "#5797DE", ink: "#1D4B7C" },
  butter: { bg: "#FAF1D2", strong: "#D2A82E", ink: "#6A500D" },
  rose: { bg: "#FCE3E9", strong: "#DF7592", ink: "#7A253E" },
  sage: { bg: "#E7EEDF", strong: "#86A271", ink: "#3B4F2D" },
  lilac: { bg: "#F3E5F7", strong: "#B679C9", ink: "#5B2B6D" },
  apricot: { bg: "#FDEAD5", strong: "#E99F55", ink: "#764311" },
  mist: { bg: "#E7EBF0", strong: "#8897AB", ink: "#374252" },
};

const DARK: Record<ColorKey, Swatch> = {
  mint: { bg: "#1E3A2F", strong: "#7AD6AF", ink: "#CDF2E1" },
  peach: { bg: "#41291E", strong: "#F2A887", ink: "#FBDACB" },
  lavender: { bg: "#2E2848", strong: "#B2A2F5", ink: "#E2DBFD" },
  sky: { bg: "#1E2F47", strong: "#89BCF2", ink: "#D4E7FB" },
  butter: { bg: "#3A331E", strong: "#EACA68", ink: "#F6EAC2" },
  rose: { bg: "#42232C", strong: "#F29AB1", ink: "#FBD8E1" },
  sage: { bg: "#293224", strong: "#A8C394", ink: "#DCE7D3" },
  lilac: { bg: "#38273E", strong: "#D19EE1", ink: "#F0DBF6" },
  apricot: { bg: "#3F2F1D", strong: "#F3BA7F", ink: "#FBE2C8" },
  mist: { bg: "#292E36", strong: "#A8B4C4", ink: "#DDE2E9" },
};

export function swatch(key: string, dark: boolean): Swatch {
  const k = (COLOR_KEYS as readonly string[]).includes(key) ? (key as ColorKey) : "mist";
  return (dark ? DARK : LIGHT)[k];
}

/** CSS custom properties for a palette color (`--c-bg`, `--c-strong`, `--c-ink`). */
export function colorVars(key: string, dark: boolean): React.CSSProperties {
  const s = swatch(key, dark);
  return { "--c-bg": s.bg, "--c-strong": s.strong, "--c-ink": s.ink } as React.CSSProperties;
}
