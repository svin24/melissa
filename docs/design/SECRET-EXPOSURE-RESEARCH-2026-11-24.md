# Keeping generated app secrets away from agents (research, 2026-11-24)

PLAN open question 86(c): `app_exec` (owner-approved, audited) can still read an app's environment, which holds the
setup password for apps that take it as an env value. This note looks at how other projects keep secrets away from
semi-trusted operators who can run commands, and away from logs. Research only; nothing here is built. Links were
checked on 2026-11-24 where noted; a few claims are marked as unverified.

## Summary

- **Short term (cheap, do now):** server-side redaction of every generated secret of the app (not only the setup
  password) in `app_exec` **and `app_logs`** output, matching the exact value, its base64 forms (all three byte
  alignments), hex, and any run of 8 or more consecutive characters of the value, also across line breaks. Tell
  the owner when a redaction happened (audit entry and a short note on the result), and reword the approval card
  for commands that can print the environment or files. Generated values are 32 characters from a 62-letter
  alphabet, so URL and JSON escaping leave them unchanged and false positives are negligible.
- **Medium term:** make the leaked value less useful and less exposed. (1) Offer secrets as files
  (`{secret_file: name}`, for images that read `*_FILE` variables) so `env` and `/proc/1/environ` show nothing.
  (2) Mark setup passwords as *seed-only* where the app only reads them at first start: once the owner has
  signed in and changed it (or after the first healthy start plus a confirmation), Melissa removes the env value
  and the Secret. A leaked bootstrap value then opens nothing.
- **Long term:** no shared admin password at all for apps that support it: Melissa as the login front (forward
  auth with trusted headers, or a small OIDC provider), as Sandstorm, Cloudron, YunoHost and Umbrel do in
  different ways. And give agents narrow per-app actions (a "console" action the server runs with the secret,
  in the style of proxy credential injection) so they rarely need raw `app_exec`.
- **What redaction cannot stop:** an agent that may run arbitrary commands in the container can always transform
  the value before printing it (`rev`, `tr`, `od`, one character per call, arithmetic on bytes, writing it to a
  file served by the app). Every CI system that masks secrets says the same: masking catches accidents, not a
  determined script. It also cannot stop the deeper fact that exec in the app container already gives full
  control of the app (create a new admin user, edit its database). The password only matters for what it opens
  **outside** that container: the app's login over the network, password reuse, and the copy that sits in the
  model provider's logs.

## How Melissa handles it today (code read 2026-11-24)

- The app spec declares `secrets` (at most 16, default 32 characters, 62-letter alphabet;
  `crates/appspec/src/lib.rs`). At the first deploy the server generates each value (`generate_secret` in
  `crates/cluster/src/app_tools.rs`) and creates one Kubernetes Secret per field (`<app>-secret-<field>`,
  `render.rs`, never overwritten). Tool results only say `generated` or `kept`.
- Containers get them as env vars through `secretKeyRef` (`EnvSource::Secret` in `render.rs`). The spec's
  `credentials {user?, password: <secret name>}` marks which one is the setup password.
- The owner reads it through the setup password card: a separate reader identity with `get` on one Secret,
  bound per app only for the reveal, removed on Dismiss (`crates/cluster/src/credentials.rs`,
  `crates/web/src/setup_password.rs`). Agents' identities cannot read Secrets.
- `AppExecTool` runs an argv in the pod (no shell unless the agent asks for `sh -c`), labelled
  `Effect::Destructive` with `reads_private` and `reads_untrusted`, so the owner confirms each call. It returns
  up to 64 KiB of stdout and stderr each, unfiltered. The audit records argv, stdin length and exit code, not the
  output.
- The Secret stays for the life of the app: "the app reads its password from it at every start".

Paths by which a generated secret can reach an agent today:

1. `app_exec` with `env`, `printenv`, `cat /proc/1/environ`, `ps eww`, reading the app's config file or database,
   or a shell script that prints variables. Each looks innocent on the approval card.
2. `app_logs`: some images print their admin password or full config at startup or on errors. The agent may also
   change the container's `command` through `update_app` (owner-approved, but long and easy to skim) to echo a
   variable, then read the logs.
3. All 16 generated fields, not only the setup password: database passwords, API keys, RCON passwords.

## What other projects do

Each entry: how it works, links, strength against an agent with exec in the app container, cost for Melissa,
app-compatibility notes.

### Kubernetes itself

**Secrets as env vars vs mounted files.** Env vars are copied into the process environment and are visible to
anything in the container (`/proc/<pid>/environ`, `env`, crash dumps, child processes, many apps' debug pages).
Mounted Secret volumes are tmpfs files that only appear where mounted and update on rotation. The Kubernetes
good-practice guide recommends files over env for this reason.
- Links: <https://kubernetes.io/docs/concepts/security/secrets-good-practices/>,
  <https://kubernetes.io/docs/concepts/configuration/secret/#using-secrets-as-files-from-a-pod>
- Strength: low against a deliberate reader (`cat /run/secrets/...` works), but it removes the most common
  accidental leak (`env`, `/proc/1/environ`, "print the config" commands) and makes a request to read the file
  obvious on the approval card.
- Cost: small. A new env source (`{secret_file: name}` → path) plus a projected volume with `defaultMode 0400`.
- Compatibility: many official images read `*_FILE` variables (postgres `POSTGRES_PASSWORD_FILE`, mariadb/mysql
  `MARIADB_ROOT_PASSWORD_FILE`, WordPress, Nextcloud's `NEXTCLOUD_ADMIN_PASSWORD_FILE` and others, following the
  Docker secrets convention). Many others take env only. Keep env as the fallback.

**One-shot and expiring bootstrap credentials.** Kubernetes bootstrap tokens (used by kubeadm to join nodes) have
an expiry and are deleted by the token cleaner controller; projected service account tokens are short-lived and
audience-bound.
- Links: <https://kubernetes.io/docs/reference/access-authn-authz/bootstrap-tokens/>,
  <https://kubernetes.io/docs/concepts/storage/projected-volumes/#serviceaccounttoken>
- Strength: high for what it targets: a value that leaks after it expired is worthless. The idea carries over to
  setup passwords as "seed-only" secrets (see the recommendation).
- Cost: medium; Melissa needs to know when the app has consumed the value (first healthy start, or the owner says
  "I've signed in and changed it").
- Compatibility: works only for apps that read the admin password once at first initialisation (Nextcloud,
  Grafana, Gitea-style installers). Images that reapply the env value at every start (several linuxserver.io
  images, code-server's `PASSWORD`, game servers' RCON) need it forever, or they reset or lose access.

**Exec auditing and admission control.** `pods/exec` is a `CONNECT` on a subresource. Audit logs record the
request (command in the query string) but not the streamed output. Validating admission webhooks can match
`CONNECT` on `pods/exec` and deny by namespace, label or command; Kyverno and Gatekeeper ship policies such as
"block pod exec by label". ValidatingAdmissionPolicy lists `CONNECT` as an operation; I did not verify an
end-to-end `pods/exec` example with it.
- Links: <https://kubernetes.io/docs/tasks/debug/debug-cluster/audit/>,
  <https://kubernetes.io/docs/reference/access-authn-authz/extensible-admission-controllers/>,
  <https://kyverno.io/policies/>
- Strength: decides *whether* exec happens, nothing about what it prints. Melissa already has a stronger gate
  (the owner confirms each exec, and only the server's identity can exec). Useful only as defence in depth: deny
  exec into namespaces where no exec approval is pending.
- Cost: low to medium. Not where the gap is.

**Ephemeral debug containers.** `kubectl debug` adds a container with its own image and env to a running pod. It
does not inherit the app container's env; with a separate process namespace it cannot read `/proc/1/environ`.
- Link: <https://kubernetes.io/docs/concepts/workloads/pods/ephemeral-containers/>
- Strength: medium for diagnostics (network, mounted volumes) but useless for the main use of `app_exec`: running
  the app's own console tool, which lives in the app image and often needs the secret itself (an RCON client
  reads the RCON password from env).
- Cost: medium; a second exec flavour. Not worth it now.

### Secret masking in CI and logs

**GitHub Actions.** Repository and organisation secrets are masked in logs; `::add-mask::` registers more values at
run time. The runner also masks some derived forms of each secret (base64 at the three byte alignments and
several escaping styles: URL, JSON, shell argument; from the `actions/runner` secret masker, not restated in the
docs). GitHub's docs warn that transformed values are not reliably masked, advise against structured values
(JSON blobs) as secrets, and say masking is not a security boundary. Multi-line values must be masked line by
line.
- Links: <https://docs.github.com/en/actions/how-tos/write-workflows/choose-what-workflows-do/use-secrets>,
  <https://docs.github.com/en/actions/reference/workflows-and-actions/workflow-commands#masking-a-value-in-a-log>,
  <https://github.com/actions/runner>
- Strength against exec: catches `env` and `cat` of the exact or base64 value; trivially beaten by `rev` or
  printing characters one by one. Same for every entry in this section.
- Cost for Melissa: low. This is the model for the short-term step.

**GitLab masked (and hidden) variables.** A variable can be masked only if it is a single line, at least 8
characters, and uses a restricted alphabet (base64 plus a few symbols), so it can be found reliably in the log.
"Masked and hidden" variables also cannot be revealed in the UI after saving. Docs warn masking is best effort.
- Link: <https://docs.gitlab.com/ci/variables/#mask-a-cicd-variable>
- Lesson: restrict the alphabet and length so matching is reliable. Melissa already does (62 letters, 32
  characters).

**Jenkins Credentials Binding.** Bound credentials are masked in the build log, including some shell-quoted forms.
The plugin docs say masking is not a guarantee and that a build script can always print a transformed value.
- Link: <https://plugins.jenkins.io/credentials-binding/>

**Dagger.** Secrets are a typed object, never plain strings: they are scrubbed from all logs and output streams
(including multi-line values since 0.5), kept out of image layers and cache keys, and exposed to a container as an
env var or a mounted file only when asked. The docs still tell users not to `echo` or `cat` them, because
scrubbing is a safeguard.
- Links: <https://docs.dagger.io/api/secrets>, <https://dagger.io/blog/dagger-0-5-0>
- Lesson: treat secrets as a separate type end to end (Melissa's `SetupPassword` with a redacted `Debug` is the
  same idea) and scrub every output stream, not one tool.

**HashiCorp Vault audit devices.** Vault never writes secret values to its audit log; it writes an HMAC-SHA256 of
each value. An operator who has a candidate value can ask Vault (`/sys/audit-hash`) whether it appeared, without
the log holding it. Vault Agent renders secrets into files for apps (templates) rather than env.
- Links: <https://developer.hashicorp.com/vault/docs/audit>,
  <https://developer.hashicorp.com/vault/docs/agent-and-proxy/agent/template>
- Lesson for Melissa: the audit can record "output contained secret field X (redacted)" or a keyed hash of the
  output, so the owner can later learn whether a value was exposed, without storing output.

### AI agent and coding-agent tools

**OpenHands.** Secrets registered with the agent (`update_secrets`, or the Secrets settings page) are exported as
env vars in the agent's runtime, and the tool output shown to the model and user is masked for those values. A
2025 prompt-injection write-up showed a token being exfiltrated anyway (the agent sent it out over the network).
- Links: <https://docs.openhands.dev/sdk/guides/secrets>,
  <https://docs.openhands.dev/openhands/usage/settings/secrets-settings>,
  <https://embracethered.com/blog/posts/2025/openhands-the-lethal-trifecta-strikes-again>
- Strength: same as CI masking. The case study shows the real risk is egress, not display.

**Codex CLI.** `shell_environment_policy` controls which env vars the agent's shell commands inherit (`inherit =
all | core | none`, exclude and include globs); third-party guides describe a default filter on names containing
`KEY`, `SECRET` or `TOKEN` (defaults differ between versions; unverified against current official docs). The
commands simply never see the secret.
- Link: <https://developers.openai.com/codex/config-advanced>
- Lesson: for Melissa, the analogue is not handing secrets to the process the agent controls at all. For
  `app_exec` that is impossible while the app container itself needs them, hence the file and seed-only options.

**Claude Code, Docker Sandboxes, Cloudflare Sandbox: proxy injection.** Credentials stay outside the sandbox. A
proxy outside it adds the real credential to outgoing requests for allowed domains; the sandbox sees nothing or a
sentinel value (Docker Sandboxes show `proxy-managed`). Anthropic's web sandbox keeps git credentials and signing
keys out of the sandbox and handles git through a proxy; the Agent SDK deployment guide recommends the same pattern.
- Links: <https://www.anthropic.com/engineering/claude-code-sandboxing>,
  <https://code.claude.com/docs/en/agent-sdk/secure-deployment>,
  <https://docs.docker.com/ai/sandboxes/security/credentials/>, <https://blog.cloudflare.com/sandbox-auth/>
- Strength: high, because the agent's process never holds the value. It fits API calls the server makes on the
  agent's behalf. For Melissa it maps to **server-side app actions**: an app spec declares an action such as
  "console command" (for example RCON), and the Melissa server runs it with the secret, returning only the
  result. The agent never needs exec or the password for routine work.
- Cost: medium to high, per kind of app (an action type per protocol: RCON, an HTTP admin API, a CLI inside the
  container with env injected by the server only for that call). Fits Melissa's existing `game`/`nextcloud`
  tool style.

**E2B, Open Interpreter, Aider, Goose.** No secret-aware output filtering that I found. E2B passes env vars you set
straight into the sandbox; Open Interpreter and Aider run commands with the user's own environment; Goose keeps
extension secrets in the OS keyring, but commands it runs see whatever env they get. Their protection is
isolation and user confirmation, which Melissa already has.
- Links: <https://e2b.dev/docs>, <https://github.com/openinterpreter/open-interpreter>, <https://aider.chat/docs/>,
  <https://block.github.io/goose/docs/>

### Self-hosted app platforms

**Umbrel.** Apps run behind an `app_proxy` container that asks for the Umbrel login before the app (can be turned
off per app). Apps that need their own login show default credentials in the app's info; since 0.4.8 these are
per-app values derived from the device's secret seed rather than one shared password.
- Links: <https://github.com/getumbrel/umbrel-apps>,
  <https://community.umbrel.com/t/umbrel-0-4-8-is-out-with-cryptographically-secure-default-app-passwords/4696>
- Strength: the proxy login means a leaked app password alone is not enough from the network. Users with a
  terminal can read everything.

**CasaOS.** Shows app-store-defined default credentials in the app's tips; many apps ship fixed defaults the user
is expected to change. No protection beyond that. Link: <https://github.com/IceWhaleTech/CasaOS-AppStore>

**YunoHost.** Apps are integrated with the system's users through LDAP and its SSO portal (SSOwat, now the YunoHost
portal), which sets trusted auth headers for apps that support it. Many app packages create no separate admin
password: the YunoHost user chosen at install becomes the app admin. Link:
<https://doc.yunohost.org/en/packaging_sso_ldap_integration>
- Strength: high for the password problem, since there is no app-specific admin secret to leak. Cost: per-app
  packaging work; YunoHost maintains a package per app.

**Cloudron.** Apps declare addons in their manifest; the `oidc` (earlier `ldap`) addon wires the app to Cloudron's
user directory, so users sign in with their Cloudron account. Apps without SSO get an admin password shown once
after install, with a prompt to change it. Links: <https://docs.cloudron.io/packaging/addons/#oidc>,
<https://docs.cloudron.io/apps/>
- Strength: high where SSO exists. A per-app packaging cost, which Cloudron pays centrally.

**Sandstorm.** Apps have no login at all. The platform authenticates users and passes identity and permissions to
the app in request headers (`X-Sandstorm-User-Id`, `X-Sandstorm-Permissions`); each grain is isolated and cannot
reach the network without a capability.
- Link: <https://docs.sandstorm.io/en/latest/developing/auth/>
- Strength: highest: nothing to leak. Cost: apps must be ported; not realistic for arbitrary OCI images.

**Coolify.** Compose "magic" variables (`SERVICE_PASSWORD_<NAME>`, `SERVICE_USER_<NAME>`) are generated once, kept
between deploys and shown in the UI under Environment Variables. Anyone with the dashboard or a container shell
sees them. Link: <https://coolify.io/docs/knowledge-base/docker/compose>
- Close to Melissa's design; no protection against exec or the UI.

**CapRover and TrueNAS apps.** The user types the admin password into the one-click app or app form at install;
the value is stored in the app config, readable by admins and by anyone with a shell. Links:
<https://caprover.com/docs/one-click-apps.html>, <https://www.truenas.com/docs/truenasapps/>

### General patterns

- **Force a change at first login.** Apps such as Grafana (default admin login asks for a new password), Portainer
  (asks to create the admin on first visit and locks the setup after a few minutes) or Jenkins (initial admin
  password file, then a setup wizard) make the bootstrap value short-lived. Melissa cannot force this for apps
  that don't do it, but it can say so on the card and, for seed-only apps, remove the secret afterwards.
- **One-time bootstrap tokens.** As Kubernetes bootstrap tokens and the Jenkins initial password: valid until used,
  then gone. Strong when the app supports it.
- **Forward auth or SSO in front of the app** (Authelia, Authentik, oauth2-proxy, Pomerium, or Melissa itself).
  Protects the app's login from the network, so a leaked password is useful only from inside the trust boundary.
  Links: <https://www.authelia.com/integration/proxies/forwarding/>, <https://oauth2-proxy.github.io/oauth2-proxy/>.
  Caveat: apps that keep their own login behind the proxy still have their password; true "no password" needs the
  app to accept trusted headers or OIDC.

## Comparison for Melissa

| Approach | Stops accidental leak (`env`, config dump) | Stops a deliberate agent | Makes a leaked value worthless | Cost | App compatibility |
|---|---|---|---|---|---|
| Redact output of `app_exec` and `app_logs` (exact, base64, hex, 8-char windows) | Yes | No | No | Low | All apps |
| Clearer approval card (flag commands that can print env or files) | Partly (relies on the owner) | Partly | No | Low | All |
| Secrets as files (`*_FILE`) | Yes for env dumps | No | No | Low | Images that read `*_FILE` |
| Seed-only secrets removed after first use | n/a | n/a | Yes (once removed) | Medium | Apps that read the admin password only at first init |
| Owner changes the password at first login (card prompt) | n/a | n/a | Yes, if the owner does it | Very low | All, manual |
| Server-side app actions (proxy pattern) instead of raw exec | Yes | Yes for those actions | n/a | Medium-high | Per protocol |
| Forward auth / Melissa as OIDC provider | n/a | n/a | Network login needs Melissa's login too | High | SSO-capable apps; headers for some |
| Admission policy on `pods/exec` | No | No (gate exists) | No | Low-medium | All |

## Recommendation, ranked

1. **Redact in the server, for all generated secrets, in every app output path** (cheap, do first).
   - Where: one `Redactor` built per app from its Secrets (the deployer already reads or creates them; a reader
     with `get` on the app's Secrets is needed at exec time, scoped like the credentials reader). Apply it to
     `app_exec` stdout and stderr and to `app_logs` before truncation is reported, and to anything else that
     returns app-produced text.
   - What to match: the exact value; standard and URL-safe base64 at the three alignments (with and without
     padding); lower and upper hex; the value with whitespace or line breaks between characters removed before
     matching; and any run of 8 or more consecutive characters of the value (a 62-letter alphabet makes an
     8-character false match about 1 in 2×10^14 per position). URL and JSON escaping need no extra case for
     this alphabet. Replace with `[secret <field> hidden]`.
   - Tell people: add `"redacted": ["<field>"]` to the tool result (so the agent knows not to retry), mark the
     audit entry, and show a short line in the chat. Repeated hits from one agent are a signal for the owner.
   - Approval card: for `env`, `printenv`, `/proc/*/environ`, `ps e`, `sh -c` with `$` or `set`, and reads of
     known config paths, add one plain line ("This can show the app's passwords. They are hidden from the
     agent, but not reliably.") rather than relying on the argv alone. Also flag `update_app` changes to
     `command`/`args` that mention a secret-backed variable.
   - Honest limit: this stops accidents and lazy prompt injections, not an agent that transforms the value
     (`rev`, `cut -c1`, `od`, writing it into a file the app serves) or exfiltrates it from inside the app. Say
     so in DECISIONS.

2. **Make setup passwords short-lived where the app allows it** (medium).
   - Spec flag on a secret, e.g. `seed_only: true`: the app needs it only for first initialisation. After the
     owner confirms on the card ("I've signed in and set my own password"), Melissa removes the env reference
     (a normal approved update, restarts the pod) and deletes the Secret. Card copy for every app: change the
     password after first sign-in.
   - Add `{secret_file: name}` as an env value form for images that read `*_FILE`, and prefer it in agent
     guidance. This removes the most common accidental leak path (`env`, `/proc/1/environ`).

3. **Reduce the need for raw exec** (medium, incremental).
   - Server-side actions declared in the app spec, starting with game consoles (RCON) and app CLIs that need a
     secret: the server runs them and returns redacted output. `app_exec` stays for the rest, owner-approved.

4. **No shared admin password for SSO-capable apps** (long term, largest win, highest cost).
   - Melissa already owns people and logins (PLAN 6c) and decides who may reach apps. Next steps: forward auth
     for app ingress (Melissa checks the session, sets trusted user headers), then a small OIDC provider for
     apps that speak it. Then an app's admin is a Melissa person and there is no setup password to leak.
   - This also ties to open question 84 (a secure store for the user's app passwords).

5. **Not now:** admission policies for `pods/exec` (Melissa's gate is already stricter), ephemeral debug containers
   (wrong tool for app consoles), Sandstorm-style porting (not practical for arbitrary images).

## Open points for PLAN

- Does the owner want redaction hits shown in the chat, or only in the audit?
- Seed-only removal: what counts as "consumed" (owner confirmation only, or also detection)?
- Which apps in the catalogue read `*_FILE`, and which reapply the admin password from env at every start?
- Exec-time reading of an app's Secrets by the server needs a scoped reader identity; reuse the credentials reader
  pattern or extend the write identity?
