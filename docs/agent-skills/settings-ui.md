# Settings pages (Providers, Updates, Developer)

**When:** changing `/settings/...`, provider overrides (`providers.toml`), agent CLI versions, the image
update status/actions, or adding a Settings section. Load the `melissa-ui` skill first; read
[htmx-component.md](htmx-component.md) for the general patterns.

## Where things are (2026-11-19)
- `crates/settings` (`melissa-settings`, no UI): `providers` (the `providers.toml` model, checks, secret
  rules, `ProviderStore`, `BackendFacts`), `install` (`Installer`, `InstallWay`, `Job`, `unit_name`,
  `parse_show`, `read_log`, `claude_login_url`, `device_prompt`, `LoginEnd`, `send_code`/`CODE_FIFO`: Install
  and Sign in through the systemd units), `version` (`--version` probe, cached 10 min, 5 s timeout, clean env),
  `updates` (`Release`, `HostKind`, host helper `status.json` parser, request files, `UpdateAction`).
- `crates/server/src/config.rs`: `ServerConfig::apply_providers` (merge at startup), `check_provider` (a change
  checked against the real backend specs before saving), `BackendSpec::provider()`.
- `crates/web/src/settings/`: `mod.rs` (context `Settings`/`SettingsParts`, routes, audit), `providers.rs`,
  `updates.rs` (view building + handlers, UI and JSON API), `apply.rs` (saved changes apply by themselves when
  Melissa is idle), `pages.rs` (assembly from components; `SECTIONS`).
- Components (all in `components.rs`): `settings_frame` (section nav + breadcrumb), `settings_head` ("Checked
  just now · Check again"), `split_card` + `pick_list`/`PickRow` (two panes), `provider_mark` (lettered hex, no
  vendor logos), `toggle` (styled native checkbox, `role=switch`, htmx `hx-trigger="change"`, a Save button that
  shows only on keyboard focus for no-JS), `detail_head`, `facts` (dl), `group`, `text_field`, `lines_field`,
  `settings_form`, `var_list`/`VarRow` (secret → "set"), `command_block` (copy button via the existing
  `.copy-code` JS), `confirm_card`, `plain_action`, `actions`, `region`, `disclosure` (native `<details>`:
  "Details", "Use an API key instead", "Log"), `external_button` (vendor sign-in link, new tab,
  `noopener noreferrer`, http(s) only), `one_field_form` (one field + primary button: the code, the API key).
  Rail: `Nav::Settings`.
- Packaging: `packaging/common/usr/share/polkit-1/rules.d/50-melissa.rules` (restart the lead agent; shared with
  Nix), `tmpfiles.d/melissa-config.conf` (`/etc/melissa` owned by `melissa`); container only
  (`packaging/container/rootfs`): `polkit-1/rules.d/51-melissa-agent-install.rules` (start
  `melissa-agent-install@{claude,grok,opencode}.service` and `melissa-agent-login@{claude,grok}.service`),
  `melissa-agent-install@.service`, `melissa-agent-login@.service`,
  `tmpfiles.d/melissa-agent-install.conf` (`/run/melissa-agent-install`, root:melissa 0750, and the FIFO
  `claude-login.code`, root:melissa 0620). The Install
  button needs `HostKind::Container` plus the unit and the 51 rule; Nix shows the Nix note. Container: the host helper
  `packaging/container/host/melissa-host-update` + `melissa-update.{path,service}`, the Quadlet's two update
  mounts, the lead agent drop-in `ReadWritePaths=-/run/melissa-update-request` ([container-image.md](container-image.md)).

## Adding a section
1. A view + handlers file in `crates/web/src/settings/` (copy `updates.rs`' shape: `view()`, `page()`, a
   region with a stable id that forms swap, an `api_*` pair).
2. An entry in `pages::SECTIONS` and routes in `mod.rs` (`ui_routes`, `api_routes`).
3. New markup only as a reviewed component. Router tests in `crates/web/src/lib.rs` (`SettingsFixture`).

## Providers: rules
- **Less text** (owner, 2026-11-19): the list plus one status word or short phrase (Not installed,
  Installing…, Sign in needed, Signing in…, Ready, Failed, Disabled, Ready after the current task, Applying…),
  and at most one primary action per state (Install, Sign in, nothing when ready). At most one short sentence of
  help. Everything technical (used by, backends, models, program, home, name/program/args form, variables,
  file) sits behind one "Details" disclosure. No host commands in the container UI (or the chat's hints);
  commands only for a run from source (`HostKind::Other`). Nix keeps one note: "This requires you add
  <Provider> via your Nix configuration."
- A provider = an agent program by file name (`claude`, `grok`, `opencode`, any other `[[backend]]`
  command's file name). Overrides apply to all its backends. **Echo is never listed** (UI, API → 404, Agent
  tasks page): it stays for tests; its backends still count as "another agent" for the disable lock.
- Precedence: defaults < `server.toml` < `providers.toml` (`$XDG_CONFIG_HOME/melissa`, `/etc/melissa` on the
  image; 0600). Merged once at startup; `ProviderStore::restart_needed` says a saved change waits.
- **Applying** (`apply.rs`): installs and sign-ins need nothing (backends look up a missing program again,
  `melissa_agent::cli::ProgramPath`, and check login files live). Saved settings (API key, program, enable):
  `apply()` calls `apply::schedule`, which restarts the lead agent (`UpdateAction::Restart`) once it is idle
  (`Ui::busy`: no chat turn, no helper working, no running agent task, no install/sign-in unit) and 4 s after
  the last change; audited `settings.apply`. The page says "Applying after the current task." /
  "Applying…" and polls. Without the restart right: "Restart Melissa to apply." (+ a command from source only).
- Program path must be absolute and keep the provider's file name (it picks the sandbox preset, egress allowlist
  and login check). Extra args go **before** Melissa's own; a denylist refuses permission/tool/mode flags; then
  the backend's own `validate` (Claude sessions only take `--model`, `--fallback-model`, `--effort`).
- A broken `providers.toml` never stops the lead agent: it is renamed `providers.toml.broken`, logged, ignored. An
  override that makes one backend invalid is dropped for that backend (warning).
- Variables: `SandboxSpec.vars` (fixed name/value, `#[serde(skip)]`), set inside the sandbox via
  `SandboxPlan::with_env`, or on the process when the sandbox is off. Names: UPPER_CASE, not reserved (PATH,
  HOME, proxies, `LD_*`, `MELISSA_*`).
- Secrets: a name with KEY, TOKEN, SECRET, PASSWORD, PASSWD, CREDENTIAL or AUTH. Never in HTML, JSON, logs or
  audit (audit = "variable X set" + SHA-256). `ProviderOverride`'s `Debug` and parse errors never print values
  (tested). Known gap: bwrap gets them as `--setenv` arguments (see PLAN open questions).
- **API key** ("Use an API key instead", shown when a sign-in is needed or a key is set): one password field,
  `POST /ui/settings/providers/{id}/key` (`value`; empty removes). The server picks the name (`key_var`:
  claude → `ANTHROPIC_API_KEY`, grok → `XAI_API_KEY`); the page never shows variable names outside Details.
- Disabling is refused while the lead agent runs on it, a helper uses it, or no other lead agent agent would be left.
- **Install** (`InstallWay::detect(root, MELISSA_RUNTIME)`): unit + polkit rule present → Install button
  (`POST /ui|/api/settings/providers/{id}/install`; id must be configured AND in `INSTALLABLE`; refused while
  running or installed). It runs `systemctl start --no-block melissa-agent-install@<id>.service` (a fixed
  `&'static str`), then the region polls `/ui/settings/providers/poll?p=<open>` every 2 s (`live_region`).
  Failure → "The install failed." + "Try again" + the log behind "Log". Success → "Ready" or "Sign in needed"
  at once (no restart). `HostKind::Nix` → only the Nix note (the unit is never called). Neither: one line, no
  command.
- **Sign in** (`Job::Login`, `POST .../{claude|grok}/login`, polkit: those two instances only), shown when the
  lead agent reports `not_logged_in`:
  - Claude: `melissa-agent-login@claude` runs `melissa-setup login claude --from-page`: `claude auth login`
    prints a URL (`claude_login_url`: only `https://claude.com|claude.ai|platform.claude.com/…oauth…`); the page
    shows "Open the sign-in page" + one "Paste the code" field + Continue + Cancel (no polling while the field
    waits). `POST .../claude/login/code` (`code`, visible ASCII ≤ 1024) → `Installer::send_code` writes one
    line into the FIFO `/run/melissa-agent-install/claude-login.code` (non-blocking open: ENXIO when no
    sign-in waits; FIFO type checked). Never an argument, variable, log or audit line. Cancel writes an empty
    line. The unit's exit status says what happened (`LoginEnd`): 0 signed in (checked with `claude auth
    status`), 3 code not accepted, 4 cancelled, 5 no code in 10 min, 6 not signed in afterwards.
  - Grok: `melissa-agent-login@grok` runs `grok login --device-auth`; `device_prompt` takes the URL (only
    `https://accounts.x.ai/…`) and the `XXXX-XXXX` code; the page shows the code and "Open the sign-in page"
    and polls.
  - OpenCode: no sign-in; one line "Free models work without an account." Provider keys: Details → variables.
  - Login readiness is checked live (credential file), so no restart is needed. Melissa never sees passwords.
- **Audit**: requests and refusals (`settings.provider.install|login`, codes never), and the outcome once the
  unit ends (`<id>: install succeeded|failed (…)`, `<id>: sign-in succeeded|failed (code not accepted|…)`),
  watched in the background whether or not the page is open.
- CLI: `melissa-server provider [list]|enable|disable <id>|set <id> [--name|--command|--arg|--no-args|--env
  NAME=VALUE|--env NAME (stdin)|--unset NAME]`.

## Updates: rules
- Host kind: `/run/.containerenv` or `/run/systemd/container` → container; else "from source". Release:
  `/usr/lib/melissa/release` (version, revision, runtime), else `MELISSA_VERSION`, else the cargo version.
- The lead agent can't run the host's podman. Check/apply/rollback: an empty file with that fixed name in
  `/run/melissa-update-request` (bind mount); the host's path unit runs `melissa-host-update requests`, which acts
  on those names only. Status comes back read-only in `/run/melissa-update/{status,last}.json`. Restart:
  `systemctl restart --no-block melissa-server.service`, allowed by polkit for user `melissa` only.
- Page: version + one status ("Up to date", "Update available", "Ready to install"), Update / Roll back
  buttons, the last result; installed-as, revision, images, automatic updates (and Nix commands) behind
  "Details". Apply and rollback restart Melissa: the UI first returns a short `confirm_card`; only
  `confirm=yes` (API: `{"confirm": true}`, CLI `--yes`) starts them. Without the helper mounts: one line, no
  host commands (UI and API); from source the page shows `git pull && just demo`.
- Automatic updates: the install's `AUTO_UPDATE` (podman-auto-update.timer), under Details, not changeable there.
- CLI: `melissa-server update [status]`, `update check|apply|rollback|restart [--yes]`.

## Gotchas
- serde/toml error messages quote the offending value: never show them for `providers.toml` (line number only).
- The toggle posts `enabled=on` or nothing; parse the form as `Vec<(String, String)>`.
- `hx-current-url` keeps the open provider when "Check again" swaps the region.
- **Restart answers first.** The lead agent has no graceful shutdown, so `systemctl restart` asked inline killed it
  before the reply (UI: dropped connection, CLI: "connection closed"). `run()` returns, then asks systemd
  750 ms later; a refusal is logged and audited.
- **`/etc/melissa` must be `melissa`'s** (`tmpfiles.d/melissa-config.conf`): systemd's `ConfigurationDirectory=`
  never chowns, and saving `providers.toml` failed with EACCES. Container mode overrides that file (symlink).
- After a rollback the newer image is the "previous" one (rollback toggles); with automatic updates on, the next
  timer run may install the newer image again.
- Restart through polkit works with logind masked in the container (verified 2026-11-19).

## Verified in the rootful container (2026-11-19)
Status (version, revision, digest; UI/API/CLI), check, apply (local image, restart on the new one), rollback and
roll forward (UI/API and host CLI), previous image kept, Restart (new lead agent PID); without the mounts, host
commands. Details: [container-image.md](container-image.md).

## Verified earlier on the headless bootc VM (2026-11-19; bootc helper archived)
Images `9dd9cc9` → `55a6086` through the dev registry (details at tag `bootc-archive-2026-11-19`). The
Providers and polkit results still apply to the container (same rule, now restart-only).

| Check | Result |
|---|---|
| Status: booted/rollback image, version, digest (UI, API, CLI); `melissa-update-status` at boot | works |
| Check via polkit as `melissa` (UI/API/CLI), "Update available" shown; bootc errors reach `last.json` | works |
| Apply: UI/API need the confirmation (card / 409), then download + reboot into the new digest (~40 s) | works |
| Rollback: 409 without confirm, 202 with; reboots into the previous image | works |
| Restart Melissa (UI 200, API 202, CLI exit 0; new lead agent PID) | works after fix |
| `melissa` can't: start/restart/stop/kill other units, stop/restart the helper units, template instances, enable/mask, set-property, set-environment, daemon-reload, reboot, transient units | denied |
| `nobody` / `agent` (no sudo) can't start the helper units or restart the lead agent | denied |
| AVC denials during status/check/apply/rollback | 0 after fix |
| Providers: list, versions, display name, secret + plain variables via UI/API/CLI | works after fix |
| Secrets in HTML, JSON, CLI output, journal, `audit.jsonl` | never; only `providers.toml` (0600 melissa) |

**Developer section** (2026-11-19): the third section, `/settings/developer` (route in `lib.rs`, state `Tasks`): Agent
  tasks (`pages::dashboard` in `frame_at(.., "Developer", ..)`); `/tasks/{id}` is a page below it (breadcrumb
  Settings / Developer / Task #n). `GET /tasks` redirects there when a lead agent is present; the task form and
  fragments keep their `/tasks…` URLs.
