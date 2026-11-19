# Settings pages (Providers, Updates)

**When:** changing `/settings/...`, provider overrides (`providers.toml`), agent CLI versions, the image
update status/actions, or adding a Settings section. Load the `melissa-ui` skill first; read
[htmx-component.md](htmx-component.md) for the general patterns.

## Where things are (2026-11-19)
- `crates/settings` (`melissa-settings`, no UI): `providers` (the `providers.toml` model, checks, secret
  rules, `ProviderStore`, `BackendFacts`), `install` (`Installer`, `InstallWay`, `unit_name`, `parse_show`,
  `read_log`: Install button through the systemd unit), `version` (`--version` probe, cached 10 min, 5 s timeout, clean env),
  `updates` (`Release`, `HostKind`, host helper `status.json` parser, request files, `UpdateAction`).
- `crates/queen/src/config.rs`: `QueenConfig::apply_providers` (merge at startup), `check_provider` (a change
  checked against the real backend specs before saving), `BackendSpec::provider()`.
- `crates/web/src/settings/`: `mod.rs` (context `Settings`/`SettingsParts`, routes, audit), `providers.rs`,
  `updates.rs` (view building + handlers, UI and JSON API), `pages.rs` (assembly from components; `SECTIONS`).
- Components (all in `components.rs`): `settings_frame` (section nav + breadcrumb), `settings_head` ("Checked
  just now · Check again"), `split_card` + `pick_list`/`PickRow` (two panes), `provider_mark` (lettered hex, no
  vendor logos), `toggle` (styled native checkbox, `role=switch`, htmx `hx-trigger="change"`, a Save button that
  shows only on keyboard focus for no-JS), `detail_head`, `facts` (dl), `group`, `text_field`, `lines_field`,
  `settings_form`, `var_list`/`VarRow` (secret → "set"), `command_block` (copy button via the existing
  `.copy-code` JS), `confirm_card`, `plain_action`, `actions`, `region`. Rail: `Nav::Settings`.
- Packaging: `packaging/common/usr/share/polkit-1/rules.d/50-melissa.rules` (restart the queen; start
  `melissa-agent-install@{claude,grok,opencode}.service` and `melissa-agent-login@grok.service`),
  `melissa-agent-install@.service`, `melissa-agent-login@.service`,
  `tmpfiles.d/melissa-agent-install.conf` (`/run/melissa-agent-install`, root:melissa 0750),
  `tmpfiles.d/melissa-config.conf` (`/etc/melissa` owned by `melissa`); container: the host helper
  `packaging/container/host/melissa-host-update` + `melissa-update.{path,service}`, the Quadlet's two update
  mounts, the queen drop-in `ReadWritePaths=-/run/melissa-update-request` ([container-image.md](container-image.md)).

## Adding a section
1. A view + handlers file in `crates/web/src/settings/` (copy `updates.rs`' shape: `view()`, `page()`, a
   region with a stable id that forms swap, an `api_*` pair).
2. An entry in `pages::SECTIONS` and routes in `mod.rs` (`ui_routes`, `api_routes`).
3. New markup only as a reviewed component. Router tests in `crates/web/src/lib.rs` (`SettingsFixture`).

## Providers: rules
- A provider = an agent program by file name (`claude`, `grok`, `opencode`, any other `[[backend]]`
  command's file name). Overrides apply to all its backends. **Echo is never listed** (UI, API → 404, Agent
  tasks page): it stays for tests; its backends still count as "another agent" for the disable lock.
- Precedence: defaults < `queen.toml` < `providers.toml` (`$XDG_CONFIG_HOME/melissa`, `/etc/melissa` on the
  image; 0600). Merged once at startup; saved changes say "after a restart" (`ProviderStore::restart_needed`).
- Program path must be absolute and keep the provider's file name (it picks the sandbox preset, egress allowlist
  and login check). Extra args go **before** Melissa's own; a denylist refuses permission/tool/mode flags; then
  the backend's own `validate` (Claude sessions only take `--model`, `--fallback-model`, `--effort`).
- A broken `providers.toml` never stops the queen: it is renamed `providers.toml.broken`, logged, ignored. An
  override that makes one backend invalid is dropped for that backend (warning).
- Variables: `SandboxSpec.vars` (fixed name/value, `#[serde(skip)]`), set inside the sandbox via
  `SandboxPlan::with_env`, or on the process when the sandbox is off. Names: UPPER_CASE, not reserved (PATH,
  HOME, proxies, `LD_*`, `MELISSA_*`).
- Secrets: a name with KEY, TOKEN, SECRET, PASSWORD, PASSWD, CREDENTIAL or AUTH. Never in HTML, JSON, logs or
  audit (audit = "variable X set" + SHA-256). `ProviderOverride`'s `Debug` and parse errors never print values
  (tested). Known gap: bwrap gets them as `--setenv` arguments (see PLAN open questions).
- Disabling is refused while the Queen runs on it, a helper uses it, or no other Queen agent would be left.
- **Install** (`InstallWay::detect(root, MELISSA_RUNTIME)`): unit + polkit rule present → Install button
  (`POST /ui|/api/settings/providers/{id}/install`; id must be configured AND in `INSTALLABLE`; refused while
  running or installed; audited `settings.provider.install`, refusals too). It runs `systemctl start --no-block
  melissa-agent-install@<id>.service` (a fixed `&'static str`), then the region polls
  `/ui/settings/providers/poll?p=<open>` every 2 s (`live_region`) and shows the log tail (`log_view`); failure →
  error + "Try again". Success → "Installed · after a restart" + Restart button (the queen resolves programs at
  start). `MELISSA_RUNTIME=nix` → only "This requires you add <Provider> via your Nix configuration." (no button,
  no command, the unit is never called). Neither: container → `sudo podman exec -it melissa melissa-setup agent
  <id>`; from source → a note.
- **Sign in** (installed, no secret variable set, not logged in or just installed): Claude → `ANTHROPIC_API_KEY`
  or `CLAUDE_CODE_OAUTH_TOKEN` from `claude setup-token` on the owner's computer; Grok → `XAI_API_KEY`; OpenCode
  → free models, optional provider key. Plus `melissa-setup login <id>` on the host in the container (not when
  the page can sign in). Melissa never handles passwords.
- **Grok device sign-in** (`Job::Login`, `POST .../grok/login`): `melissa-agent-login@grok.service` (polkit:
  that instance only) runs `grok login --device-auth`; `device_prompt` takes the URL (only
  `https://accounts.x.ai/…`) and the `XXXX-XXXX` code from `/run/melissa-agent-install/grok-login.log`; the
  region polls while it waits. Login readiness is checked live (credential file), so no restart is needed.
  A failed/stopped sign-in shows "Try again" without the log (it holds the expired code). Open items: PLAN 62.
- CLI: `melissa-queen provider [list]|enable|disable <id>|set <id> [--name|--command|--arg|--no-args|--env
  NAME=VALUE|--env NAME (stdin)|--unset NAME]`.

## Updates: rules
- Host kind: `/run/.containerenv` or `/run/systemd/container` → container; else "from source". Release:
  `/usr/lib/melissa/release` (version, revision, runtime), else `MELISSA_VERSION`, else the cargo version.
- The queen can't run the host's podman. Check/apply/rollback: an empty file with that fixed name in
  `/run/melissa-update-request` (bind mount); the host's path unit runs `melissa-host-update requests`, which acts
  on those names only. Status comes back read-only in `/run/melissa-update/{status,last}.json`. Restart:
  `systemctl restart --no-block melissa-queen.service`, allowed by polkit for user `melissa` only.
- Apply and rollback restart Melissa: the UI first returns a `confirm_card`; only `confirm=yes` (API:
  `{"confirm": true}`, CLI `--yes`) starts them. Without the helper mounts the page shows the host commands
  (`sudo /usr/local/libexec/melissa/melissa-host-update check|apply|rollback`, and a rebuild for dev containers)
  and the running image from `/run/.containerenv`; from source it says `git pull && just demo`.
- Automatic updates: the install's `AUTO_UPDATE` (podman-auto-update.timer), shown on the page, not changeable there.
- CLI: `melissa-queen update [status]`, `update check|apply|rollback|restart [--yes]`.

## Gotchas
- serde/toml error messages quote the offending value: never show them for `providers.toml` (line number only).
- The toggle posts `enabled=on` or nothing; parse the form as `Vec<(String, String)>`.
- `hx-current-url` keeps the open provider when "Check again" swaps the region.
- **Restart answers first.** The queen has no graceful shutdown, so `systemctl restart` asked inline killed it
  before the reply (UI: dropped connection, CLI: "connection closed"). `run()` returns, then asks systemd
  750 ms later; a refusal is logged and audited.
- **`/etc/melissa` must be `melissa`'s** (`tmpfiles.d/melissa-config.conf`): systemd's `ConfigurationDirectory=`
  never chowns, and saving `providers.toml` failed with EACCES. Container mode overrides that file (symlink).
- After a rollback the newer image is the "previous" one (rollback toggles); with automatic updates on, the next
  timer run may install the newer image again.
- Restart through polkit works with logind masked in the container (verified 2026-11-19).

## Verified in the rootful container (2026-11-19)
Status (version, revision, digest; UI/API/CLI), check, apply (local image, restart on the new one), rollback and
roll forward (UI/API and host CLI), previous image kept, Restart (new queen PID); without the mounts, host
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
| Restart Melissa (UI 200, API 202, CLI exit 0; new queen PID) | works after fix |
| `melissa` can't: start/restart/stop/kill other units, stop/restart the helper units, template instances, enable/mask, set-property, set-environment, daemon-reload, reboot, transient units | denied |
| `nobody` / `bee` (no sudo) can't start the helper units or restart the queen | denied |
| AVC denials during status/check/apply/rollback | 0 after fix |
| Providers: list, versions, display name, secret + plain variables via UI/API/CLI | works after fix |
| Secrets in HTML, JSON, CLI output, journal, `audit.jsonl` | never; only `providers.toml` (0600 melissa) |
