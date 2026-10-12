// Provider marks (scripts/logos, from OpenUsage's set; muse.svg is Simple
// Icons' Meta mark, CC0) → src/assets/providers, normalized the way OpenUsage
// draws them: fitted to the artwork's real bounding box (some sources bake
// whitespace into their viewBox, others run edge to edge), centered in a
// square with the same margin, one color (currentColor). The app and the
// Holster preview both use the results.
import { readFileSync, readdirSync, writeFileSync } from "node:fs";
import { join, resolve } from "node:path";
import { svgPathBbox } from "svg-path-bbox";

const root = resolve(import.meta.dirname, "..");
const from = join(root, "scripts/logos");
const to = join(root, "src/assets/providers");
/** Margin on every side, as a share of the square (Claude's original look). */
const INSET = 0.1;
const r = (n) => Math.round(n * 1000) / 1000;

for (const file of readdirSync(from).filter((f) => f.endsWith(".svg"))) {
  const src = readFileSync(join(from, file), "utf8");
  const paths = [...src.matchAll(/<path\b[^>]*\sd="([^"]+)"/g)].map((m) => m[1]);
  if (paths.length === 0) throw new Error(`${file}: no paths`);
  if (/<(?!svg|path|title|\/)/.test(src) || /transform=/.test(src)) throw new Error(`${file}: only plain paths are supported`);
  let [x0, y0, x1, y1] = [Infinity, Infinity, -Infinity, -Infinity];
  for (const d of paths) {
    const [a, b, c, e] = svgPathBbox(d);
    [x0, y0, x1, y1] = [Math.min(x0, a), Math.min(y0, b), Math.max(x1, c), Math.max(y1, e)];
  }
  const side = Math.max(x1 - x0, y1 - y0) / (1 - 2 * INSET);
  const vx = (x0 + x1) / 2 - side / 2;
  const vy = (y0 + y1) / 2 - side / 2;
  const svg =
    `<svg xmlns="http://www.w3.org/2000/svg" viewBox="${r(vx)} ${r(vy)} ${r(side)} ${r(side)}" fill="currentColor">` +
    paths.map((d) => `<path d="${d.replace(/\s+/g, " ").trim()}"/>`).join("") +
    `</svg>\n`;
  writeFileSync(join(to, file), svg);
  console.log(file, `${r(x1 - x0)}×${r(y1 - y0)}`);
}
