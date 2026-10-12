# Quota

Your AI usage limits and what they cost, at a glance, in the [Holster](../Holster) dock. A Windows
take on [OpenUsage](https://github.com/robinebers/openusage) (MIT): its provider research, pricing
data, spend scanners and popover design are ported here. Quota is not affiliated with OpenUsage and
uses its own name and mark, per OpenUsage's trademark policy.

- **Hover** the Quota tile: each provider's fullest limit in a pace-colored ring, when it resets,
  and what today has cost. The preview is a page on disk, so it shows the last known numbers even
  while Quota is closed.
- **Click** it: a compact panel, as tall as its content.
  - **Total spend**: Today / Yesterday / 30 Days, as a ring of each provider's share (cost or
    tokens), in each provider's brand color.
  - **Per provider**: every limit as a bar colored by pace (blue on course, amber cutting it close,
    red running out before the reset, with when), a 30-day usage trend, and a caret that opens
    Today / Yesterday / Last 30 days with cost and tokens, plus reset credits and extra usage.
  - Click a meter's value to flip used / left, its reset to flip countdown / exact time.
- Holster's tab **glows** when a limit passes your alert threshold (80% by default) or a limit you
  were close to resets. Never on a timer.

## Providers

| Provider | Login it reads | Limits | Spend |
|---|---|---|---|
| Claude | None: asks Claude Code itself (`claude -p /usage`) | Session, weekly, per-model weekly | Claude Code session logs (`~/.claude/projects`) |
| Codex | `~/.codex/auth.json` (Codex CLI, ChatGPT sign-in) | Windows by length (session, weekly, monthly), model limits, reset credits, credits | Codex session rollouts (`~/.codex/sessions`) |
| Cursor | `%APPDATA%\Cursor\User\globalStorage\state.vscdb` (Cursor app) | Billing-cycle total, Cursor / other model pools, on-demand | Cursor's usage export |

Nothing to configure: sign in with the tool itself.

**Quota never touches your Claude login.** It runs the real Claude
Code, headless and windowless, with its built-in `/usage` command and reads what it prints.
`/usage` is a local command (no model request, nothing counted against your limits); the run is
kept out of your session history (`--no-session-persistence`) and skips MCP servers and hooks.
Claude Code signs in and renews its own token, as always. The plan name is read from Claude
Code's saved login on disk and never sent anywhere.

Codex and Cursor are read with their saved logins directly (OpenUsage's approach); tokens go only
to that provider's own API.

- Codex tokens are renewed only once they've expired (or the API rejects them). The
  renewed token is written back into the tool's own file, touching only the token fields, and
  only if the file still holds the login Quota started from.
- Cursor's database is opened read-only and never written; a renewed token stays in memory.
- Spend is an **estimate at API rates** from measured token counts (what the usage would cost on
  the API, not your subscription bill). Logs are scanned incrementally: the first scan reads the
  last 30 days, later ones only what was appended. Models no price list knows are left out and
  named under the card's caret.
- **When I look** (default): nothing is checked in the background. Hovering the tile shows the last
  numbers at once and asks Quota for fresh ones (the preview updates a second or two later);
  opening the panel does the same. Numbers under 30 seconds old aren't checked again. At launch
  only the local logs are read. Glow alerts need background checks, so they're off in this mode.
- Or every 5 or 15 minutes in the background (and when you look). A provider that rate-limits
  Quota is left alone until it says to come back.
- `quota.exe --settings` opens Quota straight to Settings.

## Data

Pricing ships in `src-tauri/resources/pricing/` (from OpenUsage): its supplement (Cursor-native
models, fast multipliers, slug aliases), a [LiteLLM](https://github.com/BerriAI/litellm) snapshot
and a [models.dev](https://models.dev) snapshot. Provider logos in `src/assets/providers/` come from
OpenUsage's set and belong to their owners.

## Develop

```sh
pnpm install
pnpm tauri dev        # run
cd src-tauri && cargo test
cargo test --release real_logs -- --ignored --nocapture   # scan this machine's logs
pnpm tauri build      # NSIS installer (current user)
pnpm app-icon         # re-render icons from src-tauri/icons/quota-icon.svg
```

Data lives in `%APPDATA%\app.quota.desktop\`: `settings.json`, `usage.json` (last known
numbers) and `holster-preview\` (the page Holster serves, its `state.json` and the tile mark).
