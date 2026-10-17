// pnpm release:build: the installer for a GitHub release, signed for the
// in-app updater, plus the latest.json the updater reads.
//
// Needs Quota's private key, made with `pnpm tauri signer generate`, and its
// password: QUOTA_SIGNING_KEY (a path or the key itself; defaults to
// %USERPROFILE%\.tauri\quota.key) and QUOTA_SIGNING_KEY_PASSWORD, from this
// shell or from your Windows user environment variables. Named for Quota so
// other projects (Holster) keep their own keys; Tauri's generic
// TAURI_SIGNING_PRIVATE_KEY(_PASSWORD) work as a fallback.
//
// Writes src-tauri/target/release/bundle/nsis/latest.json; attach it to the
// release next to the installer. Optional: `-- --notes notes.md` puts a
// file's text in latest.json as the update's notes.
import { execFileSync, spawnSync } from "node:child_process";
import { existsSync, readFileSync, writeFileSync } from "node:fs";
import { homedir } from "node:os";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const root = join(dirname(fileURLToPath(import.meta.url)), "..");
const REPO = "ManyGlue0/Quota";

/** A variable from this shell, else from the user's Windows environment. */
function userEnv(name) {
  if (process.env[name]) return process.env[name];
  try {
    const out = execFileSync("reg", ["query", "HKCU\\Environment", "/v", name], { encoding: "utf8", stdio: ["ignore", "pipe", "ignore"] });
    // "    NAME    REG_SZ    value": keep the value exactly, spaces and all.
    const m = out.match(new RegExp(`^\\s+${name}\\s+(REG_SZ|REG_EXPAND_SZ)    (.*?)\\r?$`, "m"));
    if (!m) return undefined;
    return m[1] === "REG_EXPAND_SZ" ? m[2].replace(/%([^%]+)%/g, (_, v) => process.env[v] ?? "") : m[2];
  } catch {
    return undefined;
  }
}

const conf = JSON.parse(readFileSync(join(root, "src-tauri", "tauri.conf.json"), "utf8"));
if (!conf.plugins?.updater?.pubkey) {
  console.error("tauri.conf.json has no updater public key yet: put the line from quota.key.pub in plugins.updater.pubkey.");
  process.exit(1);
}
const key = userEnv("QUOTA_SIGNING_KEY") ?? userEnv("TAURI_SIGNING_PRIVATE_KEY") ?? join(homedir(), ".tauri", "quota.key");
const password = userEnv("QUOTA_SIGNING_KEY_PASSWORD") ?? userEnv("TAURI_SIGNING_PRIVATE_KEY_PASSWORD");
if (!existsSync(key) && !key.includes("untrusted comment")) {
  console.error(`No private key at ${key}. Make one with: pnpm tauri signer generate -w "${key}"`);
  process.exit(1);
}
if (password === undefined) {
  console.error("Set QUOTA_SIGNING_KEY_PASSWORD (in this shell or as a Windows user variable).");
  process.exit(1);
}

const build = spawnSync("pnpm", ["tauri", "build", "--config", "src-tauri/tauri.release.conf.json"], {
  cwd: root,
  stdio: "inherit",
  shell: true,
  env: { ...process.env, TAURI_SIGNING_PRIVATE_KEY: key, TAURI_SIGNING_PRIVATE_KEY_PASSWORD: password },
});
if (build.status !== 0) process.exit(build.status ?? 1);

const { version } = conf;
const dir = join(root, "src-tauri", "target", "release", "bundle", "nsis");
const installer = `Quota_${version}_x64-setup.exe`;
const sig = join(dir, `${installer}.sig`);
if (!existsSync(sig)) {
  console.error(`The build didn't sign the installer (${sig} is missing).`);
  process.exit(1);
}
const notesAt = process.argv.indexOf("--notes");
const latest = {
  version,
  notes: notesAt > 0 ? readFileSync(process.argv[notesAt + 1], "utf8").trim() : "",
  pub_date: new Date().toISOString(),
  platforms: {
    "windows-x86_64": {
      signature: readFileSync(sig, "utf8").trim(),
      url: `https://github.com/${REPO}/releases/download/v${version}/${installer}`,
    },
  },
};
writeFileSync(join(dir, "latest.json"), JSON.stringify(latest, null, 2) + "\n");
console.log(`\nSigned ${installer}\nWrote ${join(dir, "latest.json")}\nAttach both to the v${version} release.`);
