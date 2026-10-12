// Renders src-tauri/icons/quota-icon.svg to a transparent 1024px PNG with
// headless Edge (always present on Windows 11), then lets `tauri icon`
// generate every platform size from it.
import { execFileSync } from "node:child_process";
import { existsSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";
import { pathToFileURL } from "node:url";

const root = resolve(import.meta.dirname, "..");
const svg = readFileSync(join(root, "src-tauri/icons/quota-icon.svg"), "utf8");

const edge = [
  join(process.env["ProgramFiles(x86)"] ?? "", "Microsoft/Edge/Application/msedge.exe"),
  join(process.env.ProgramFiles ?? "", "Microsoft/Edge/Application/msedge.exe"),
].find(existsSync);
if (!edge) throw new Error("Microsoft Edge wasn't found; it's needed to render the icon.");

const work = mkdtempSync(join(tmpdir(), "quota-icon-"));
const page = join(work, "icon.html");
const png = join(work, "icon.png");
writeFileSync(page, `<!doctype html><html><body style="margin:0;background:transparent">${svg}</body></html>`);
execFileSync(edge, [
  "--headless=new",
  "--disable-gpu",
  "--hide-scrollbars",
  "--default-background-color=00000000",
  "--force-device-scale-factor=1",
  "--window-size=1024,1024",
  `--screenshot=${png}`,
  pathToFileURL(page).href,
]);
if (!existsSync(png)) throw new Error("Edge didn't produce the icon render.");

execFileSync("pnpm", ["tauri", "icon", png], { cwd: root, stdio: "inherit", shell: true });
// Windows-only app: drop the mobile sets `tauri icon` also writes.
for (const dir of ["android", "ios"]) rmSync(join(root, "src-tauri/icons", dir), { recursive: true, force: true });
console.log("Icons regenerated from quota-icon.svg");
