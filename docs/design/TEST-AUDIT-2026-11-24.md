# Test audit, 2026-11-24

This is an audit only: no tests were deleted or changed. It was measured on master at `69eccde`, which includes gVisor, the removal of the legacy catalog, ask_lead and threads.

## TL;DR

- **The suite runs 969 tests in 5.0 s of wall time** once it is built (50.5 s of CPU across the 8-way parallel run). What you feel is mostly the compile: **test targets are about 50% of build CPU** (214 s of 430 s for a workspace rebuild).
- **Most tests guard something real.** About 45% are security or data-loss tests and must stay. Very few are pure trivia.
- **About 115 tests (12%) and about 3,000 lines (7% of about 44k test lines) can go at low risk**, plus 678 lines of golden JSON. That is the honest ceiling. Going further means dropping security or approval coverage.
- **Shedding tests buys little speed.** The big wins are structural:
  1. **Fix 9 slow tests.** They take 19.8 s of the 50.5 s test CPU, and the slowest single test (3.2 s) sets the wall-time floor. Fixed, the wall time would be about 1.5 to 2 s.
  2. **Shared fixtures.** About 1,000 lines are duplicated helpers.
  3. **Merge the 5 integration test binaries into 2.** That saves about 20 s of CPU per rebuild.
  4. **Trim whole-wording asserts inside tests that otherwise stay.** That is most of the web savings.

## 1. Measurements

Sources: `cargo nextest run --status-level all` (per-test times), `cargo nextest run --no-run --timings` (per-unit compile time), and line counts. Line counts include separate test files plus every `#[cfg(test)] mod … {` block through the end of its file.

### Tests, time and lines per crate

| Crate | Tests | Test CPU (s) | Test lines | Crate .rs lines | Test share |
|---|---:|---:|---:|---:|---:|
| agent | 406 | 21.1 | 18,877 | 49,201 | 38% |
| web | 276 | 22.2 | 13,992 | 43,414 | 32% |
| cluster | 114 | 2.4 | 5,952 (+946 fixture/golden JSON in all crates) | 12,067 | 49% |
| settings | 92 | 1.3 | 2,916 | 8,336 | 35% |
| server | 36 | 0.5 | 1,188 | 4,929 | 24% |
| auth | 21 | 2.1 | 587 | 2,064 | 28% |
| voice | 15 (+1 ignored) | 0.9 | 293 | 1,526 | 19% |
| appspec | 9 | 0.1 | 261 | 1,409 | 19% |
| **total** | **969** (+2 ignored) | **50.5** | **~44,100** | | |

`crates/escape-probe` is its own workspace, with 3 tests that are not in the run.

The largest modules:

- **web:**
  - `tests::lead_api` (the `lib.rs` inline module): 73 tests, 14.7 s
  - `feed` 24, `lead_api::auth` 14, `components` 12, `usage` 11, `markdown` 10, `provider_flow` 10
- **agent:**
  - `lead::tests` 33, `lead::schedules::tests` 21, `claude::tests` 20
  - `sandbox::tests` 19, `egress::tests` 18, `acp::tests` 17
  - `files` 13, `sandbox::limits` 13
- **cluster:** `app_tools::tests` 65, which is 3.9k lines in one file.
- **settings:** `app_access` 19, `factory_reset` 16.
- **server:** `config` 20.

### Wall time and the slowest tests

- **Distribution:** 764 of 969 tests run in under 50 ms. 11 take 0.5 s or more, and **9 take 1 s or more, adding up to 19.8 s (39% of all test CPU).**

| s | Test | Why it is slow | Fix |
|---:|---|---|---|
| 3.20 | web `tests::lead_api::claude_sign_in_can_be_cancelled` | Production code polls in the cancel path (`settings/providers.rs`, about 20 × 150 ms) | Inject the poll interval / wait |
| 2.92 | agent::escape `sandbox_escape_suite` | Real processes, by design | Keep. Trim the fixed 400/600 ms sleeps in `run_parent_dies` |
| 2.33 | agent::sandbox `cancelling_kills_background_grandchildren` | Real 1500/300/500 ms waits | Cut them to about 300/100/150 ms |
| 2.22 | web `install_button_starts_the_fixed_unit_and_polls` | Install-path poll in `settings/providers.rs` (up to 3 s) | Inject the wait |
| 2.21 | web `claude_signs_in_with_a_pasted_code_through_the_fifo` | The same poll | Inject the wait |
| 2.02 | agent `acp::probe::tests::non_acp_programs_fail_or_time_out` | A real 2 s probe timeout | Pass a 200 ms timeout in the test |
| 2.02 | agent `egress::relay::tests::a_half_closed_tunnel_ends_after_a_short_grace` | Real `HALF_CLOSE_GRACE` (2 s) on duplex streams | `#[tokio::test(start_paused = true)]` |
| 1.80 | auth `password::tests::standard_cost_is_argon2_default` | Full argon2 cost in an unoptimised build | `[profile.dev.package.argon2] opt-level = 3` (it also speeds the dev server) |
| 1.08 | agent `lead::tests::agent_tool_guards_time_limits_and_concurrency` | A real time limit | Paused time, or a shorter limit in the test |
| 0.52 / 0.42 | agent `egress::relay::tests::idle_tunnels_are_closed` / `download_cap_and_lifetime_close_the_tunnel` | Real idle and lifetime waits | Paused time |
| 0.38 | web `a_save_that_changes_nothing_shown_sends_nothing` | Waits 300 ms to prove that nothing arrives | Cut to about 20 ms, or shed (see §3) |

Other real waits that are cheap today but should use paused time:

- voice `engine::tests::clips_run_one_at_a_time_and_extra_ones_are_turned_away` and `a_slow_run_times_out_and_frees_the_queue`
- settings `tailscale::tests` (the fixture `wait: 400ms`; `wait_for` polls with `std::time::Instant` and a 300 ms sleep)
- agent `lead::events::tests::wait_since_wakes_on_new_events`

Production code also sleeps 600 ms after every app approval (`web/src/apps.rs`). Make that injectable too.

### Compile time

These numbers come from a workspace rebuild with dependencies already built. Total wall time was 106 s, and the units add up to 430 s of CPU.

| Unit | Compile s |
|---|---:|
| agent lib (test) | 64.2 |
| web lib (test) | 53.7 |
| cluster lib (test) | 24.2 |
| settings lib (test) | 14.4 |
| agent `escape` / `egress` / `sandbox` / `acp_live` integration tests | 8.6 / 7.6 / 7.5 / 7.1 |
| cluster `rbac_manifests` integration test | 6.1 |
| netbridge bin (test) / server bin (test) | 6.1 / 5.6 |
| voice, appspec, auth, device (test) | 3.1 / 3.0 / 3.0 / 0.4 |
| **all test units** | **≈ 214 (50%)** |
| non-test units, for comparison: web 53.4, agent 39.0, cluster 19.1, settings 9.6 | |

- **There are 15 test binaries:** 8 lib, 3 bin, and 5 integration (`agent/tests/{sandbox,egress,escape,acp_live}.rs` and `cluster/tests/rbac_manifests.rs`). Each integration binary links the whole crate and its dependencies again. Together they cost **36.9 s of CPU**.
- **Test code is 38% of agent and 32% of web,** so deleting tests shortens the two big `lib (test)` compiles roughly in proportion. Removing 7% of the test lines saves perhaps 5 to 8 s of CPU. The integration-binary merge is worth more.

### Environment note

`cargo nextest run` on its own fails one test in a fresh worktree: `escape::seccomp_off_backend_is_reported_not_passed`, with the error "probe helper missing (build it: just escape-probe)". `just test` builds the helper first, so `just check` is fine. The failure is deliberate, since a soft skip must not pass. Anyone who calls nextest directly should run `just escape-probe` once first.

## 2. KEEP: never shed

These are named by file. Wording may be trimmed inside these tests, but the tests stay.

### agent

- **Sandbox and isolation:**
  - all of `sandbox.rs` (19)
  - `sandbox/seccomp.rs` (6)
  - `sandbox/users.rs` (7)
  - `sandbox/acl.rs` (5)
  - `sandbox/limits.rs` (12 of 13; see §3 c)
  - `tests/sandbox.rs` (5)
  - `tests/escape.rs` (4)
  - `tests/egress.rs` (2)
  - `bin/melissa-netbridge` `rejects_bad_arguments`
- **Egress:**
  - `egress/{addr,dns,rules,web,relay}.rs` (all)
  - `egress/tests.rs`: 17 of 18. These include `mcp_socket_serves_only_its_own_tokens`, `learn_mode_allows_unlisted_hosts_but_not_private_ones`, the `web_reads_*` tests, `plain_http_is_forwarded_read_only` and `new_clis_reach_their_apis_but_not_their_telemetry_or_updates`.
- **Permissions, policy, owner approval, scope and taint:**
  - `tools/{policy,scope,permissions,confirm}.rs` (all)
  - `tools/permission_tests.rs` (9)
  - `tools/mod.rs` (10: lead-only, taint, scheduled confirmations, audit)
  - `tools/owner.rs` `a_cited_owner_message_is_checked_and_quoted_but_never_approves` and `ids_have_one_shape`
  - `session/permissions.rs` (9)
  - `acp/client.rs` `never_picks_always_options`
  - `mcp.rs` `allow_list_hides_tools`, `tokens_are_random_hex`, `cancelled_notification_stops_the_call_and_withdraws_its_confirmation`
- **Lead:**
  - Nearly all 33 tests in `lead/tests.rs`, for example:
    - `taint_outlives_the_session_and_the_daemon`
    - `deleting_a_helper_by_the_lead_needs_the_users_confirmation`
    - `a_lead_brokered_message_is_tagged_as_data_and_audited`
    - `a_helper_is_made_for_the_person_whose_message_the_lead_answers`
  - All of `lead/tests/talk.rs` (owner-message verification, flood limit, "replies come back as data")
  - `lead/agents/talk.rs` (2) and `lead/threads.rs` (1)
  - `lead/tests/schedules.rs` (3)
  - The owner-approval and scope tests in `lead/schedules/tests.rs`:
    - `an_agent_proposes_a_schedule_for_itself_and_the_owner_must_approve_it`
    - `an_agent_cannot_touch_another_agents_schedules`
    - `always_allow_rules_never_cover_schedules_but_no_permission_prompts_does`
    - `bad_input_is_refused_and_nothing_is_saved`
    - `only_five_field_cron_is_accepted`
  - `lead/transcript.rs` `setup_password_cards_stay_out_of_the_agents_context`
- **Backends:**
  - `claude/tests.rs`:
    - `arguments_fix_tools_permissions_and_session`
    - `config_args_are_an_allowlist`
    - `permission_answers_are_one_off`
    - `cancel_interrupts_and_withdraws_pending_permissions`
    - `agent_withdrawn_requests_get_no_answer`
    - the tests that refuse injected model, effort and fast flags
  - `acp/tests.rs`:
    - `codex_signs_in_with_its_key_and_is_switched_to_read_only`
    - `opencode_flavor_owns_the_config`
    - `new_flavors_set_their_variables_and_codex_keeps_its_sign_in_file_out_with_a_key`
    - `cancel_answers_pending_permission_and_ends_turn`
    - `recorded_sessions_replay`, which scrubs tokens
  - `acp/replay.rs`: the scrubber tests
  - `cli.rs`: `flag_like_prompts_are_rejected`, `streams_stdout_and_stderr`, `readiness_names_what_is_missing`
- **Data loss:**
  - all of `migrate/tests.rs` (5), `files/tests.rs` (13), `lead/registry.rs` (5) and `device.rs` (2)
  - `lead/usage.rs` `saves_and_loads_and_reads_a_missing_file_as_none`
  - `lead/backends.rs` `model_cache_survives_a_restart` and `the_older_model_file_still_reads`
- **Secrets:**
  - `provider_usage/https.rs` (3)
  - `collect::the_report_serializes_without_secrets_or_raw_answers`
  - `gemini::an_expired_token_is_not_sent`

### web

- **lead_api, security and data loss:**
  - all of `lead_api/auth.rs` (14) and `lead_api/reset.rs` (6)
  - `cross_site_posts_are_rejected`, `cross_site_api_calls_are_rejected`, `ui_posts_need_the_same_origin`
  - `mcp_endpoint_serves_tools_per_session`
  - `a_model_that_does_not_match_its_checksum_is_refused`, `the_transcribe_route_checks_what_it_gets`
  - `settings_apps_sets_who_may_open_apps_and_fails_closed`
  - `settings_tailscale_enable_sign_in_and_disable`, `settings_tailscale_is_not_melissas_when_the_host_runs_it`
  - `setup_password_card_reveals_only_on_request_and_dismiss_is_for_good`
  - `install_button_starts_the_fixed_unit_and_polls`
  - `claude_signs_in_with_a_pasted_code_through_the_fifo`, `claude_sign_in_can_be_cancelled`, `grok_signs_in_with_a_device_code`
  - `the_api_key_goes_under_the_fixed_name_and_is_never_echoed`, `secrets_are_write_only_everywhere`
  - `provider_changes_are_checked`, `updates_need_a_yes_and_the_helper`
  - `confirmations_can_be_listed_and_resolved`, `operator_tool_calls_wait_for_the_inbox`
  - `hostile_log_lines_are_only_ever_text`
  - `force_delete_needs_the_name_and_then_removes_and_tells_the_lead`
  - `approvals_page_and_approve_deny`, `always_allow_saves_a_rule_listed_in_settings`, `no_permission_prompts_needs_a_yes`, `a_helpers_approval_shows_in_her_chat_and_melissas`
  - `sending_and_serving_files`, `a_helpers_files_and_retention`
  - `the_taint_note_shows_in_the_chat`
  - `a_fresh_start_keeps_the_one_chat_and_old_archives_stay_readable`
  - `the_picker_offers_speed_and_access_only_when_they_apply`
  - `helpers_are_deleted_with_their_chat_box_and_schedules_but_not_with_apps`
- **Scope:** `lead_api/details.rs` `each_chat_shows_its_own_agents_details`, `permissions_and_recent_activity_of_an_agent`, `helpers_are_made_renamed_pinned_and_deleted_in_their_settings`, `files_are_added_referenced_and_sent_from_the_panel`, `transfers_stop_at_the_receiving_boxs_cap`.
- **Members and owner:**
  - `lead_api/apps.rs` (3 of 4)
  - `lead_api/setup.rs` `the_owner_goes_through_setup_and_members_never_see_it` and `the_owner_renames_the_device_in_settings`
  - `lead_api/usage.rs` `a_member_sees_the_agents_and_devices_but_no_provider_limits`
  - `lead_api/about.rs` `the_owner_and_a_member_both_open_it`
- **`provider_flow_tests.rs`:**
  - `remove_is_refused_while_melissa_uses_the_provider`
  - `remove_uninstalls_deletes_keys_hides_it_and_add_brings_it_back`
  - `removing_from_source_keeps_the_program`
  - `the_registry_is_fetched_only_when_asked`
  - `a_local_acp_command_is_checked_before_it_is_saved`
  - `a_download_that_does_not_match_its_checksum_is_refused`
  - `providers_show_logos_where_allowed_and_letter_marks_otherwise`
- **Escaping and XSS:**
  - `components.rs` `text_is_escaped`, `agent_text_is_markdown_with_html_escaped`, `markdown_xss_inputs_are_inert`
  - `markdown.rs` `hrefs_are_checked`, `raw_html_is_text`, `unsafe_links_keep_only_text`, `deep_nesting_is_capped`
  - `components/mobile.rs` `sheet_text_is_escaped`
  - `components/people.rs` `auth_pages_post_plain_forms_and_escape`
  - `logos.rs` `every_vendored_logo_passes_the_allowlist`, `the_raw_files_carry_nothing_active`, `active_or_external_content_is_refused`
  - `state.rs` `app_links_follow_the_visitors_host`
- **Other:**
  - `settings/remove_provider.rs` `deletes_only_inside_home_and_not_through_symlinks`
  - `settings/add_provider.rs` `command_lines_split_like_a_simple_shell`
  - `settings/setup.rs` `only_the_owner_is_sent_to_setup`
  - `app_list.rs` (all 5)
  - `apps.rs` `force_delete_needs_the_exact_name`
  - `auth/tests.rs` (5 of 6)
  - `agents.rs` `access_shows_only_with_the_developer_option_and_marks_the_chip`

### cluster

- **`app_tools/tests.rs` (65), security and data loss:**
  - Secrets: `a_new_setup_password_is_announced_once_without_the_value`, `install_applies_everything_in_order_and_never_returns_the_secret`
  - Checks before anything runs: `install_rejects_bad_arguments_before_touching_the_cluster`, `a_bad_spec_fails_before_anyone_is_asked`
  - Data kept or deleted: `uninstall_keeps_data_by_default_and_deletes_it_only_when_asked`, the `force_delete` tests and `no_agent_tool_can_force_delete`
  - Sanitising untrusted output: `status_reports_phase_url_and_sanitized_pod_reasons`, `logs_are_capped_and_cleaned`, `control_sequences_are_stripped`
  - Labels and approval: `labels_are_honest`, `through_the_registry_logs_taint_and_uninstall_waits_for_a_human`
  - Ownership and scope: every ownership test from `agent_a_never_touches…` to `agents_are_told_their_scope_and_scopeless_sessions_only_read`, plus `stop_follows_the_workspace_rules`
  - Exec: all `exec_*` argument, workspace and audit tests
  - Network access: `links_fail_closed_when_the_filter_is_not_in_force`, `links_follow_the_app_access_setting`, `links_leave_the_tailnet_node_out…`
  - The new `wait_for_apps`, gVisor and network-permission tests
- **Other cluster files:**
  - `render/tests.rs` `every_container_is_locked_down_and_sized` and the rest except the goldens
  - `tests/rbac_manifests.rs` (6)
  - `credentials/tests.rs` (3)
  - `health.rs` `spec_is_read_only`, `rbac_failures_are_tool_errors`

### settings, server, auth, appspec, voice

- **settings:**
  - all of `app_access/tests.rs` (19). It is the firewall; the nft text it pins is the firewall itself.
  - all of `factory_reset/tests.rs` (16)
  - `tailscale`:
    - `bad_output_and_foreign_urls_are_refused_without_quoting_them`
    - `tools_never_return_the_sign_in_url`
    - `not_managed_when_the_host_runs_tailscale`
    - `setting_file_round_trip_and_the_unit_condition`
    - `host_flag_and_management`
  - `install` (7: the fixed units, the URL host checks, the FIFO)
  - `providers`:
    - `secret_names_are_recognised`
    - `views_and_debug_never_show_secret_values`
    - `checks_reject_what_would_undo_melissas_setup`
    - `store_saves_0600_and_tracks_restart`
    - `bad_files_are_errors_without_their_text`
    - `added_providers_are_checked`
  - `registry`:
    - `unsafe_entries_are_manual_only`
    - `install_dirs_stay_under_the_agents_folder`
    - `unpacks_a_tarball_and_finds_the_program`
    - `rejects_what_is_not_a_registry`
    - `cache_keeps_only_good_copies`
  - `updates`:
    - `requests_are_empty_files_with_fixed_names`
    - `actions_map_to_fixed_requests`
    - `restart_needs_only_the_polkit_rule`
    - `bad_or_hostile_status_is_handled`
  - `lib.rs` `private_files_are_0600`
- **server:**
  - `config.rs`:
    - `rejects_unsafe_or_invalid_config`
    - `sandbox_is_on_by_default_and_configurable`
    - `network_and_allowlist_are_backend_keys`
    - `grok_acp_is_built_in_with_a_strict_permission_mode`
    - `overrides_a_backend_refuses_are_dropped`
    - `added_providers_become_lead_backends_with_their_hosts`
    - the new seccomp and limits config tests
  - `main.rs` `factory_reset_needs_the_word`, `tool_command_refuses_unknown_tools`
  - `preflight.rs` `seccomp_off_comes_from_the_backend_config` and `limits_line_reports_a_dangerous_panic_on_oom`
- **auth:** all 21.
- **appspec:** all 9 (spec validation).
- **voice:**
  - all of `model.rs` (checksums)
  - `fetch.rs` (redirects)
  - `whisper_cli_gets_a_private_clip_and_it_is_removed`, `bad_input_is_refused`, `clips_over_two_minutes_are_refused_before_converting`

## 3. SHED candidates

Counts are tests removed or merged away. Lines are estimates from reading the code.

### (a) Brittle copy asserts: about 40 tests, about 1,050 lines, plus about 800 lines of trims

| Group | Tests | Files | Lines | Replacement | Risk |
|---|---:|---|---:|---|---|
| Whole-page wording on Settings and the sidebar | 7 | `web/src/lib.rs`: `settings_pages_render_and_link_from_the_rail`, `settings_shows_the_list_at_its_root_and_a_section_below_it`, `settings_lists_grouped_sections_with_icons_and_a_search_field`, `the_sidebar_has_search_new_helper_groups_and_a_bottom_bar`, `cluster_and_details_show_apps_and_devices`, `usage_lists_melissa_and_helpers_with_only_reported_numbers`, `nixos_updates_show_nix_commands_and_only_restart` | 240 | One table test, "each Settings route answers 200 and has its section id". Keep `data-list` presence and `v["actions"]==["restart"]` | low |
| Mobile markup | 5 | `web/src/tests/lead_api/mobile.rs`: `a_chat_has_a_back_button_to_the_agent_list`, `the_agent_list_has_a_new_button_and_a_tab_bar`, `the_action_sheet_shows_the_agent_and_the_desktop_menu_items`, `the_details_panel_is_a_sheet_with_a_handle_and_says_whose`, `the_composer_says_changes_need_permission` | 117 | Keep the 2 routing tests | low |
| About page | 2 | `lead_api/about.rs`: `it_is_last_in_the_list_with_the_version_on_a_phone`, `search_finds_it` | 30 | none | low |
| Component markup | 7 | `components.rs` `markdown_elements` (move its "images are not loaded" line into the XSS test) and `app_list_rows_are_a_real_list_with_status_words`; `components/panel.rs` `the_sections_bar_has_four_labelled_tabs` and `the_updates_tab_shows_a_dot_only_when_an_update_is_available`; `components/mobile.rs` `the_back_link_and_cancel_have_names`; `workspace.rs` `the_markup_has_no_settled_group_and_keys_each_row`; `agents.rs` `nothing_usable_says_so` | 200 | none; lib.rs and logic tests already cover these | low–med |
| Usage wording | 3 | `usage.rs` `the_note_mentions_context_and_cost_only_when_something_is_missing` and `resets_and_checks_in_words`; `lead_api/usage.rs` `the_owner_switches_between_limits_cost_and_tokens` | 105 | Keep `limit_cards_in_every_state` | low (`resets_and_checks_in_words`: med) |
| Provider picker markup | 2 | `provider_flow_tests.rs` `the_picker_uses_logos_with_a_letter_fallback`, `the_add_panel_works_without_script` | 71 | `providers_show_logos_where_allowed_and_letter_marks_otherwise` stays | low |
| Words helpers | 6 | `settings/list.rs` `counts_read_as_words`, `tailscale_reads_as_a_word_or_nothing`; `settings/remove_provider.rs` `names_read_naturally`; settings `updates.rs` `ago_reads_well`; agent `provider_usage/mod.rs` `lengths_in_words`; agent `echo.rs` `echo_reply_repeats_the_message_and_counts_the_recap` | 50 | none | low |
| Prompt and CLI wording | 4 | server `main.rs` `prompts_map_turning_off_to_stop_app`, `lead_prompt_passes_on_the_app_platform_words`; server `client.rs` `update_status_shows_digests_and_last_message`; agent `acp/tests.rs` `one_turn_renders_concise_output` | 80 | Keep the "it is data, not instructions" prompt pins (they guard against prompt injection) | low |
| Golden manifests | 2 | `cluster/src/render/tests.rs` `nextcloud_renders_to_the_golden_objects`, `minecraft_renders_to_the_golden_objects`, plus the `golden()` helper and `tests/golden/*.json` | 65 + 678 JSON | Make `every_container_is_locked_down_and_sized` loop over both apps (today it checks minecraft only) | med |

**Trims (tests stay, the wording asserts go):**

- **web `lib.rs`, about 300 lines:**
  - `home_is_the_chat_with_its_services`: keep CSP, the no-`style=`/`<script>`/`onclick` checks, escaping and the approve `hx-post`.
  - `app_rows_open_details_with_status_logs_and_actions`
  - `chatting_with_a_helper`
  - `the_developer_toggle_shows_thinking_and_timings_in_the_chat`
  - `pages_answer_with_the_main_column_alone_for_in_page_navigation`
  - the "wanted words" lists in `install_button_starts_the_fixed_unit_and_polls` and `updates_need_a_yes_and_the_helper`
- **`lead_api/details.rs`, about 180 lines:** `each_chat_shows_its_own_agents_details`, `the_lead_agent_is_renamed_like_a_helper`.
- **`lead_api/setup.rs` and `auth.rs`, about 70 lines.**
- **cluster `app_tools/tests.rs` and `health.rs`, about 35 lines:** compare against the `NOT_INSTALLED`/`NOT_ANSWERING` constants instead of sentences.
- **settings `tailscale`:** the pinned status sentences.
- **server `preflight.rs`, about 40 lines:** the rendered summary lines in `lead_uses_first_available_backend_in_her_order`, `no_agent_and_sandbox_problems_say_what_to_do`, `limits_line_says_whether_agents_are_limited` and `gvisor_line_says_whether_apps_get_their_own_kernel`. Assert on the report's fields, not the text.
- **server `config.rs` `gemini_codex_and_muse_session_backends_are_built_in`:** keep only the approval-mode checks.
- **agent:**
  - The fake agent pins `"<contains:You are the lead agent (test).>"` and `"You are agent Ada (id …)"` across 5+ lead tests. Use one stable marker.
  - `the_user_renames_the_lead_agent_and_its_prompts_follow`: use `matches!(LeadError::Invalid(_))` instead of exact error text.

Better pattern for wording overall: one `insta`-style snapshot per page is **not** recommended. It would still break on every copy change, just more conveniently. Prefer asserting ids, `hx-*` targets, `href`s and the absence of dangerous markup.

### (b) Duplicates: about 45 tests, about 900 lines

| Group | Merged away | Files and tests | Lines | Replacement | Risk |
|---|---:|---|---:|---|---|
| Web routing and partial duplicates | 9 | `lib.rs` `htmx_dispatch_opens_the_task` + `plain_dispatch_redirects_and_errors_are_shown`; `task_page_renders_the_reply_as_markdown`; `voice_input_is_locked_without_a_speech_engine`; `settings_apps_without_the_filter_says_the_firewall_decides`; `nix_installs_point_at_the_nix_configuration` + `without_the_unit_the_container_shows_no_host_command`; `echo_is_never_listed` + `the_built_in_echo_is_not_offered`; `another_settings_section_answers_alone_for_the_section_swap`; `the_light_dark_choice_comes_with_the_page` | 230 | Table tests: (htmx / plain), (install way → button, 409, no calls); one echo test | low |
| About-page duplicates | 2 | `lead_api/about.rs` `the_license_and_the_credits_are_built_in` (repeats `settings/about.rs` `the_texts_are_built_in`), `it_swaps_in_place` | 37 | none | low |
| Feed approval words | 4 | `feed.rs` `stopping_an_app_is_asked_but_not_dangerous`, `a_file_transfer_names_the_file_and_both_boxes`, `every_kind_of_request_has_a_tag_line`, `approval_words_fall_back_for_unknown_tools` | 100 | One table (tool, args) → (destructive, permanent, tag kind) | med |
| Feed notes | 2 | `feed.rs` `errors_and_stops_become_notes`, `a_model_chosen_before_the_first_message_leaves_the_chat_empty` | 25 | Fold into `a_steered_turn_says_so_and_a_plain_stop_says_stopped` and `a_switch_mid_chat_is_one_line_not_two` | low |
| Small web units | 9 | `components.rs` `bare_urls_become_links`; `markdown.rs` `linkify_skips_non_urls`, `linkify_keeps_all_text`; `apps.rs` `details_show_other_ports_with_their_protocol`; `state.rs` `tile_addresses_hide_the_tailnet_name`; `settings/setup.rs` `a_hidden_checklist_stays_hidden`, `the_current_model_is_preselected_and_default_means_the_default_entry`; `agents.rs` `the_footer_is_left_out_when_nothing_applies`; `components/panel.rs` the two `usage_table_*` tests (into one) | 120 | Table tests | low |
| `lead_api/apps.rs` | 1 | `without_k3s_the_list_says_it_isnt_set_up` (the same as `state.rs` `NotSetUp`) | 20 | none | med |
| Cluster reach repeated in app_tools | 4 | `app_tools/tests.rs` `container_without_hairpin_keeps_every_covered_link`, `loopback_bind_drops_every_other_address`, `without_a_publish_bind_a_failed_probe_keeps_the_link`, `the_agent_gets_the_no_k3s_words_over_mcp` | 77 | `reach.rs` unit tests plus `links_leave_out_addresses_the_host_does_not_publish_on` for the wiring | low |
| Cluster misc | 4 | `app_tools/tests.rs` `my_apps_shows_the_lead_agent_every_app_grouped_by_owner` (merge); `reach.rs` `probe_mode_an_app_that_does_not_answer_is_not_an_address_problem`; `health.rs` `api_vanishing_mid_call_is_unreachable`; `device.rs` `urls_bracket_ipv6` | 50 | Fold into their siblings | low |
| Agent parsers and caches | 10 | `provider_usage/pace.rs` (4 → 1), `grok.rs` (3 → 1), `opencode.rs` (2 of 3 merged); `attachments.rs` `gib_sizes_are_binary_and_labelled_so` + `sizes_read_well`; `lead/backends.rs` the two "seen for one model … kept" tests | 60 | Table tests | low |
| Agent backend scripts | 3 | `lead/tests/switching.rs` `reasoning_levels_come_from_the_agent_and_apply_at_the_next_session` + `a_fast_mode_comes_from_the_agent_and_applies_at_the_next_session`; `acp/tests.rs` `real_opencode_transcript_replays` + `real_grok_transcript_replays`; `egress/tests.rs` `default_denylists_parse_and_lease_dirs_hold_the_dns_files` (misnamed) into `default_allowlists_parse` | 115 | One parametrised test each | low (the replay merge: med) |
| Agent cli | 1 | `cli.rs` `missing_program_is_unavailable` (asserted at the start of `a_program_installed_later_is_found_without_a_restart`) | 6 | none | low |
| Settings / voice | 4 | `tailscale` `magic_dns_off_means_no_name`; `updates.rs` `parses_a_running_only_status` + `parses_staged_previous_and_found_update`; voice `wav.rs` 3 conversion tests into 1 | 40 | Tables | low |
| Server config | 1 | `config.rs` `empty_file_means_defaults` (the same as `missing_file_yields_defaults`) | 8 | none | low |

**Further merge, not counted above (med risk):** `lead/tests.rs` drives a full Lead plus a fake ACP agent for scope decisions that `tools/scope.rs` already unit-tests:

- `reassigning_by_the_lead_needs_the_users_confirmation`
- `the_leads_destructive_action_on_a_agents_workspace_names_the_agent`
- `the_user_reassigns_workspaces_without_a_confirmation`
- `the_lead_transfers_files_between_boxes_only_with_the_users_yes`

Keep one end-to-end test.

Also not counted: `agent_tools_refuse_a_reused_name_and_show_ids` repeats the resolver cases in `registry.rs`.

### (c) Trivial: about 17 tests, about 330 lines

| Tests | Files | Lines | Risk |
|---|---|---:|---|
| `no_provider_settings_change_nothing`, `parses_custom_backends`, `claude_and_opencode_session_backends_are_built_in` (78 lines of pinned preset values; first move its `ro.is_empty()` check, "the user's OpenCode config is not bound", into `sandbox_is_on_by_default_and_configurable`) | server `config.rs` | 120 | low |
| `parses_provider_and_update_commands`, `parses_backend_commands`, `parses_serve_and_tool_commands`: flag-by-flag CLI parsing, 190 lines collapsed into one 40-line table (−2 tests) | server `main.rs` | 150 | med (hand-written parser) |
| `first_line_trims_and_handles_empty` | server `preflight.rs` | 4 | low |
| `limits_round_trip_through_serde` | agent `sandbox/limits.rs` | 10 | low |
| `placeholder_is_replaced_or_appended` (fold), `shorten_cuts_long_text`, `reads_lines_and_skips_blanks`, `agents_without_a_usage_request_answer_none`, `provider_ids_are_the_command_file_names` | agent `cli.rs`, `lead/transcript.rs`, `acp/transport.rs`, `acp/tests.rs`, `provider_usage/mod.rs` | 32 | low |
| `truncation_keeps_full_text_in_title`, `oob_targets_the_inner_content`, `durations_in_short_words`, `each_composer_names_its_chat_for_drafts`, `browsers_get_short_labels`, `the_checker_reads_both_spellings` (it tests a test helper) | web `components.rs`, `feed.rs`, `lib.rs`, `auth/tests.rs`, `tokens.rs` | 55 | low |
| `provider_ids`, and the two `prefs.rs` round trips merged into one | settings `providers.rs`, `prefs.rs` | 28 | low |

Note that web has three separate duration formatters: `feed`, `pages::format_elapsed` and `usage::duration`. Unify them and test them once.

### (d) Slow: shed 1 test, fix the rest

- **Shed:** `a_save_that_changes_nothing_shown_sends_nothing` (15 lines), or cut its wait to 20 ms.
- **Fix the rest:** every other slow test is a security or behaviour test, so fix it instead (see §1). The fixes are worth about 15 s of test CPU and bring wall time down from 5 s to about 1.5–2 s.

### (e) Legacy

- **Already done:** the catalog legacy (`appspec/src/legacy.rs`) is gone on master, and so is `a_catalog_install_gets_its_spec_and_stays_manageable`.
- **Owner decision:** 4 tests in `cluster/src/app_tools/tests.rs` (about 230 lines, plus the `legacy_nextcloud` fixture) still cover installs made before 2026-11-19, and the code paths they test (`legacy_owner`, the old label keys) are still live:
  - `old_label_keys_are_read_as_aliases_and_relabelled_once`
  - `the_relabel_patch_keeps_what_it_cannot_map`
  - `legacy_workspaces_migrate_and_the_reassign_path_moves_them`
  - `unassigned_workspaces_from_before_agents_are_only_the_leads_with_approval`
  
  They are data-migration tests, so they are KEEP until that migration window is declared closed. Then they go **together with the code**, not before it.
- **Removed features still asserted:** `the_sidebar_has_search_new_helper_groups_and_a_bottom_bar` (the "no New chat" checks) and `the_markup_has_no_settled_group_and_keys_each_row`. Both are already counted under (a).
- **A router that exists only in tests:** `dashboard_lists_backends_and_sets_csp` tests a router that only the tests build (`/` redirects to `/tasks`). Shed it (28 lines) and replace it with a security-headers test on the real router (see §5).

## 4. Structural wins

1. **Fewer integration binaries.** There are 5 today, and each one links the crate again (36.9 s of CPU combined).
   - **agent:** merge `tests/{sandbox,egress,escape,acp_live}.rs` into one `tests/it/main.rs` with modules. That saves about 20 s of CPU per rebuild and removes the duplicated `scratch`/`host_program`/`namespaces_work` helpers (about 45 lines).
     - `packaging/nix/tests/integration-tests.nix` and `vm.nix` name the `sandbox`/`egress` binaries, and `docs/agent-skills/acp-sessions.md` names `acp_live`. Update them in the same change.
     - The escape suite already has `MELISSA_REQUIRE_SANDBOX`-style "fail rather than skip" logic. Extend it to `sandbox` and `egress`, which today print SKIP and pass.
   - **cluster:** `tests/rbac_manifests.rs` uses only public `render::*` constants and files under `CARGO_MANIFEST_DIR`. Move it into `src/render/` as a unit test, which removes one binary (6.1 s). Do this after the network-permissions work lands.
2. **Shared fixtures, about 1,000 lines:**
   - **Temp dirs:** `scratch()`/`temp()` is copied about 16 times across agent, settings and voice, and about 60 more places build `temp_dir().join(format!(…pid))` inline. These folders are left behind when a test panics. Use one `test_support` module per crate, or `tempfile` as a dev-dependency (a new dependency needs its reason in the commit message).
   - **web:** a session-request builder is written 6 times (`as_`, `as_get`/`as_post`, `get_as`), and the box builders `two()`, `box2()`, `box3()`, `fx()` and `setup_box()` each wire an owner and a member by hand. Replace both with one `TestBox` in `tests/lead_api/mod.rs`. Also:
     - Five SSE frame readers become one `frames_until(body, pred)`.
     - Five inline polling loops become one `eventually()`.
     - The fake workspace store is defined twice in `lib.rs`.
     - The "cross-site: refused / without script: redirect" blocks repeated in about 10 tests become one table over every mutating route.
   - **agent:**
     - `fixture`/`policy`/`open_replay`/`init_steps` are duplicated between `claude/tests.rs` and `acp/tests.rs`.
     - The `Names`/registry/spawn helpers are duplicated between `lead/schedules/tests.rs` and `tools/permission_tests.rs`.
     - `audit_lines()` appears 3 times.
     - About 10 fake `ToolSpec` structs should become one configurable `TestTool { risk, scope, lead_only }`.
   - **cluster/appspec:** the minecraft spec is defined 3 times, the tailscale status JSON 2 times, and there are three separate fake clusters.
3. **Cheaper levels:**
   - The scattered same-origin checks in about 10 router tests become a unit table on `same_origin()` plus one route table.
   - Picker and echo-hiding markup moves into `agents.rs` unit tests.
   - Nix/container update wording becomes a check on the JSON `actions` only.
   - `tests/egress.rs` `sandboxed_agent_reaches_only_allowlisted_hosts_and_mcp` repeats the allow/deny matrix from `egress/tests.rs`. In the namespace test, keep only what needs a real namespace.
4. **Build profile:** setting `[profile.dev.package.argon2] opt-level = 3` removes 1.8 s from one test.

## 5. Missing high-value tests

1. **web: a unit table for `same_origin()`.** Nothing tests `Sec-Fetch-Site: same-site`/`cross-site`, `Origin: null`, an Origin/Host mismatch, or an Origin with no Host.
2. **web: security headers on the real `router_with_settings`.** `X-Frame-Options: DENY`, `nosniff` and `Referrer-Policy` are never asserted, and CSP is only checked on home.
3. **web: member scope for helper chats, file boxes and approving confirmations.** Only the apps routes have a member test. Check the intended rules first.
4. **web: an invite that expires on the clock.** Only the single-use case is tested.
5. **agent: `files::remove_box` has no direct test.** This is a data-loss path. Test that it removes only that owner's box, does not follow a symlinked box, and returns the right count.
6. **cluster: no agent tool can read or reveal an app's setup password.** This should be a structural test, like `no_agent_tool_can_force_delete`.
7. **cluster: lockdown on every app.** `every_container_is_locked_down_and_sized` checks minecraft only. Loop over all specs, including the gVisor `runtimeClassName`. This is needed before the goldens go.
8. **The sandbox and egress integration tests pass as SKIP** when bwrap or user namespaces are missing. Only the NixOS VM test notices. Add the same "required" switch the escape suite has.

## 6. Recommendation

**Target:** about **115 tests** (969 → about 855) and about **3,000 Rust lines** plus 678 lines of golden JSON, all at low risk (a few medium-risk items are marked above). This comes from shedding and merging (about 2,250 lines) plus trims and shared fixtures (about 750+). It is about 12% of the tests and 7% of the test lines. Going further than this means cutting security coverage, which this audit advises against.

**Order (small, reviewable changes):**

1. **Slow tests** (§1 table). Biggest felt win, about 15 s of test CPU, wall 5 s → about 2 s. Touches some production code to make waits injectable.
2. **Integration binary merge in agent,** plus the "required, not SKIP" switch, then cluster `rbac_manifests` to a unit test once the network work lands. Saves about 25 s of compile CPU.
3. **Web wording shed and trims** (§3a, b, c for web; about 63 tests, about 1,800 lines). Wait until the UI agent's current change has landed. Do it together with the `TestBox`/`frames_until`/`eventually` helpers.
4. **Server, cluster, settings, voice and agent tables** (§3b, c; about 50 tests, about 900 lines).
5. **Add the 8 missing tests** (§5), most of them small.
6. **Later:** the 4 legacy-migration tests, together with their code, when the migration window is closed.

Rule to keep it lean: a new UI test asserts structure (ids, `hx-*`, `href`, escaping, member/owner difference), never whole sentences. Wording lives in one place, and a copy change should not touch tests.
