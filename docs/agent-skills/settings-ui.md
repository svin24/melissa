# Settings pages (Providers, Updates)

**When:** changing `/settings/...`, provider overrides (`providers.toml`), agent CLI versions, the OS update
status/actions, or adding a Settings section. Load the `melissa-ui` skill first; read
[htmx-component.md](htmx-component.md) for the general patterns.

## Where things are (2026-11-19)
- `crates/settings` (`melissa-settings`, no UI): `providers` (the `providers.toml` model, checks, secret
  rules, `ProviderStore`, `BackendFacts`), `version` (`--version` probe, cached 10 min, 5 s timeout, clean env),
  `updates` (`Release`, `HostKind`, `bootc status` parser, helper files, `UpdateAction` → fixed unit).
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
- Image: `rootfs/usr/libexec/melissa/melissa-update`, `melissa-update-{status,check,apply,rollback}.service`,
  `rootfs/usr/share/polkit-1/rules.d/50-melissa.rules`, `tmpfiles.d/melissa-update.conf`, `polkit` package;
  `tmpfiles.d/melissa-config.conf` (`/etc/melissa` owned by `melissa`).

## Adding a section
1. A view + handlers file in `crates/web/src/settings/` (copy `updates.rs`' shape: `view()`, `page()`, a
   region with a stable id that forms swap, an `api_*` pair).
2. An entry in `pages::SECTIONS` and routes in `mod.rs` (`ui_routes`, `api_routes`).
3. New markup only as a reviewed component. Router tests in `crates/web/src/lib.rs` (`SettingsFixture`).

## Providers: rules
- A provider = an agent program by file name (`claude`, `grok`, `opencode`, `echo`, any other `[[backend]]`
  command's file name). Overrides apply to all its backends.
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
- "Set up": `sudo melissa-setup agent <id>` (or `login <id>`) shown with a copy button on images; the UI never
  installs (needs root and runs vendor installers).
- CLI: `melissa-queen provider [list]|enable|disable <id>|set <id> [--name|--command|--arg|--no-args|--env
  NAME=VALUE|--env NAME (stdin)|--unset NAME]`.

## Updates: rules
- Host kind: `/run/.containerenv` or `/run/systemd/container` → container; `/run/ostree-booted` → bootc; else
  "from source". Release: `/usr/lib/melissa/release`, else `MELISSA_VERSION`, else the cargo version.
- The queen can't run `bootc` (root even to read). The root helper writes `/run/melissa-update/status.json` and
  `last.json` (root:melissa 0640); `melissa-update-status.service` runs at boot. The queen asks for an action with
  `systemctl start --no-block <fixed unit>`; polkit allows only user `melissa`, only those units (and `restart
  melissa-queen.service`). Nothing from the request reaches the helper.
- Apply and rollback reboot: the UI first returns a `confirm_card`; only `confirm=yes` (API: `{"confirm":
  true}`, CLI `--yes`) starts them. Without the helper the page shows the `sudo bootc ...` commands; container
  mode shows the rebuild command; from source says `git pull && just demo`.
- Automatic updates stay off (the stock timer is masked, PLAN open question 20).
- CLI: `melissa-queen update [status]`, `update check|apply|rollback|restart [--yes]`.

## Gotchas
- serde/toml error messages quote the offending value: never show them for `providers.toml` (line number only).
- The toggle posts `enabled=on` or nothing; parse the form as `Vec<(String, String)>`.
- `hx-current-url` keeps the open provider when "Check again" swaps the region.
- **bootc only in `install_t`.** The helper never runs `bootc` itself: from a shell script (bin_t →
  `unconfined_service_t`) bootc stays out of `install_t`, its chcon self-relabel is denied (AVC `mac_admin`)
  and it carries on without it, which can break writing labels a newer policy brings. `bootc()` in the helper
  runs each call as a transient unit (`systemd-run --wait`); output goes to files PID 1 opens in
  `/run/melissa-update/.work` (0700). A pipe (`--pipe`) or `/tmp` from the helper's domain is denied to `init_t`.
- **Restart answers first.** The queen has no graceful shutdown, so `systemctl restart` asked inline killed it
  before the reply (UI: dropped connection, CLI: "connection closed"). `run()` returns, then asks systemd
  750 ms later; a refusal is logged and audited.
- **`/etc/melissa` must be `melissa`'s** (`tmpfiles.d/melissa-config.conf`): systemd's `ConfigurationDirectory=`
  never chowns, and saving `providers.toml` failed with EACCES. Container mode overrides that file (symlink).
- The apply unit ends "Failed with result 'signal'" because the reboot stops it; expected.
- After a rollback, bootc still lists the newer image as available (the tag still points at it); applying
  again goes back to it.

## Verified on the headless bootc VM (2026-11-19, Secure Boot on, SELinux enforcing)
Images `9dd9cc9` → `55a6086` (+ a `-v3` label bump) through the dev registry; see
[bootc-vm-test.md](bootc-vm-test.md#settings--updates-on-the-vm-2026-11-19).

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
