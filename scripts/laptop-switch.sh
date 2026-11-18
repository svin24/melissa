#!/usr/bin/env bash
# Move a Fedora Kinoite machine (the dev laptop) onto the Melissa desktop image, check it after the
# reboot, or roll back. The owner runs this by hand: every step that changes the system is printed
# first and needs a "y"; sudo asks for the password itself. Nothing reboots on its own.
#
#   scripts/laptop-switch.sh preflight   read-only checks (rpm-ostree status, space, image, ...)
#   scripts/laptop-switch.sh switch      copy the image to root's storage, rebase, untrack initramfs-etc
#   scripts/laptop-switch.sh verify      after the reboot: release, Secure Boot, /nix, tailscale, queen, k3s
#   scripts/laptop-switch.sh rollback    back to the previous deployment (the old Kinoite)
#
# Guide: docs/agent-skills/bootc-image-build.md, "Moving the laptop onto the desktop image".
# Env: IMAGE (default localhost/melissa-desktop:dev).
set -uo pipefail
image=${IMAGE:-localhost/melissa-desktop:dev}
target="ostree-unverified-image:containers-storage:$image"
cmd=${1:-}

bold() { printf '\n\033[1m%s\033[0m\n' "$*"; }
ok() { printf '  ok    %s\n' "$*"; }
warn() { printf '  WARN  %s\n' "$*"; warns=$((warns + 1)); }
bad() { printf '  FAIL  %s\n' "$*"; fails=$((fails + 1)); }
warns=0 fails=0

# Print a command, ask, run it. Stops the script if the command fails.
step() {
    printf '\n  $ %s\n' "$*"
    local a
    read -r -p "  Run this? [y/N] " a || a=
    case $a in y|Y|yes) ;; *) echo "  stopped (nothing more was run)."; exit 1 ;; esac
    if ! bash -c "$*"; then echo "  that failed; stopping. Nothing reboots on its own." >&2; exit 1; fi
}

[ -t 0 ] || { echo "laptop-switch.sh is interactive; run it in a terminal." >&2; exit 2; }
[ -e /run/ostree-booted ] || { echo "not an ostree/bootc system; nothing to do here." >&2; exit 2; }

# Fields of the booted deployment from `rpm-ostree status --json`.
booted_json() {
    rpm-ostree status --json --booted | python3 -c '
import json, sys
d = next(x for x in json.load(sys.stdin)["deployments"] if x.get("booted"))
print("origin=" + (d.get("container-image-reference") or d.get("origin") or "?"))
for k in ("requested-packages", "requested-local-packages", "base-removals", "requested-base-removals",
          "base-local-replacements", "requested-base-local-replacements", "initramfs-etc", "initramfs-args"):
    v = d.get(k) or []
    if v: print(k + "=" + " ".join(map(str, v if isinstance(v, list) else [v])))
'
}

user_image_id() { podman image inspect --format '{{.Id}}' "$image" 2>/dev/null; }

preflight() {
    bold "Booted deployment (rpm-ostree status)"
    rpm-ostree status | sed 's/^/  /'
    local info; info=$(booted_json)
    printf '%s\n' "$info" | sed 's/^/  /'
    local reqs local_pkgs removals
    reqs=$(printf '%s\n' "$info" | sed -n 's/^requested-packages=//p')
    local_pkgs=$(printf '%s\n' "$info" | sed -n 's/^requested-local-packages=//p')
    removals=$(printf '%s\n' "$info" | grep -E '^(requested-)?base-(removals|local-replacements)=' || true)
    if [ -n "$reqs" ] && [ "$reqs" != tailscale ]; then
        warn "layered packages besides tailscale: $reqs (they stay layered on top of the image; check they are wanted)"
    elif [ "$reqs" = tailscale ]; then ok "layered: tailscale only (the image ships it; switch uninstalls the layer)"
    else ok "no layered packages"; fi
    [ -z "$local_pkgs" ] || warn "local RPMs layered: $local_pkgs (add --uninstall for each, or rpm-ostree reset)"
    [ -z "$removals" ] || warn "base overrides present: $removals (rpm-ostree reset may be needed)"
    if rpm-ostree status --json | python3 -c 'import json,sys; sys.exit(0 if any(d.get("staged") for d in json.load(sys.stdin)["deployments"]) else 1)'; then
        warn "a staged deployment is pending; the switch replaces it (rpm-ostree cleanup -p drops it)"
    fi

    bold "Transient root and Nix"
    if findmnt -no OPTIONS / | grep -q 'upperdir=/run'; then ok "transient root active (/ is an overlay in /run)"
    else warn "transient root not active now (findmnt /)"; fi
    if findmnt /nix >/dev/null; then ok "/nix mounted: $(findmnt -no SOURCE /nix)"; else warn "/nix is not mounted"; fi
    [ -f /etc/ostree/prepare-root.conf ] && ok "/etc/ostree/prepare-root.conf present (image ships the same in /usr/lib)"

    bold "Security and services"
    sb=$(mokutil --sb-state 2>&1 | head -n1); ok "Secure Boot: $sb"
    ok "SELinux: $(getenforce 2>/dev/null || echo unknown)"
    systemctl is-active --quiet tailscaled && ok "tailscaled active (state in /var/lib/tailscale stays: /var is kept)" \
        || warn "tailscaled not active"
    if ss -ltnH 'sport = :8080' 2>/dev/null | grep -q .; then
        warn "something listens on :8080 now; after the switch the system queen owns 127.0.0.1:8080 (dev daemons: listen = \"127.0.0.1:8090\")"
    else ok "port 8080 free"; fi
    # Plasma's first-run wizard runs before the login screen unless this flag exists (/etc carries over).
    if [ -e /etc/plasma-setup-done ] || ! systemctl is-enabled --quiet plasma-setup.service 2>/dev/null; then
        ok "Plasma first-run setup will not show (flag present or wizard not enabled)"
    else
        warn "/etc/plasma-setup-done missing: Plasma's setup wizard may show at boot (sudo touch /etc/plasma-setup-done)"
    fi
    getent passwd melissa >/dev/null && warn "a 'melissa' user already exists: $(getent passwd melissa)" \
        || ok "no 'melissa' user yet (systemd-sysusers creates it at boot)"

    bold "Space"
    local avail_sys avail_var
    avail_sys=$(df -B1G --output=avail /sysroot 2>/dev/null | tail -n1 | tr -d ' ')
    avail_var=$(df -B1G --output=avail /var/lib/containers 2>/dev/null | tail -n1 | tr -d ' ')
    [ "${avail_sys:-0}" -ge 20 ] && ok "/sysroot: ${avail_sys} GiB free" || bad "/sysroot: ${avail_sys:-?} GiB free (want 20+)"
    [ "${avail_var:-0}" -ge 10 ] && ok "/var/lib/containers: ${avail_var} GiB free" || bad "/var/lib/containers: ${avail_var:-?} GiB free (want 10+)"

    bold "Image"
    local uid; uid=$(user_image_id)
    if [ -z "$uid" ]; then bad "$image not in your container storage: devenv shell -- just image-build desktop"
    else
        ok "$image in your storage: ${uid:0:12}"
        rel=$(podman run --rm --network=host "$image" cat /usr/lib/melissa/release 2>/dev/null | tr '\n' ' ')
        case $rel in *MELISSA_VARIANT=desktop*) ok "release: $rel" ;; *) bad "not a desktop image: '$rel'" ;; esac
    fi
    echo "  Root's storage (needs sudo to look):"
    local rid; rid=$(sudo podman image inspect --format '{{.Id}}' "$image" 2>/dev/null || true)
    if [ -z "$rid" ]; then warn "$image not in root's storage yet ('switch' copies it)"
    elif [ "$rid" = "$uid" ]; then ok "root's storage has the same image (${rid:0:12})"
    else warn "root's storage has a different $image (${rid:0:12}); 'switch' copies yours over it"; fi

    bold "Result: $fails failed, $warns warnings"
    [ "$fails" = 0 ]
}

switch() {
    preflight || { echo "Fix the failures above first." >&2; exit 1; }
    bold "1/3 Copy the image into root's container storage (rpm-ostree reads it there)"
    local uid rid
    uid=$(user_image_id)
    rid=$(sudo podman image inspect --format '{{.Id}}' "$image" 2>/dev/null || true)
    if [ -n "$rid" ] && [ "$rid" = "$uid" ]; then ok "already there (${rid:0:12})"
    else step "podman save $image | sudo podman load"; fi

    bold "2/3 Rebase onto the image, dropping the layered tailscale (the image ships it)"
    local reqs; reqs=$(booted_json | sed -n 's/^requested-packages=//p')
    local un=""
    for p in $reqs; do [ "$p" = tailscale ] && un="--uninstall=tailscale"; done
    step "sudo rpm-ostree rebase $un $target"

    bold "3/3 Stop tracking /etc/ostree/prepare-root.conf in the initramfs (the image has it in /usr/lib)"
    if booted_json | grep -q '^initramfs-etc=.*prepare-root.conf'; then
        step "sudo rpm-ostree initramfs-etc --untrack=/etc/ostree/prepare-root.conf"
    else ok "not tracked; nothing to do"; fi

    bold "Staged. Check, then reboot when you are ready:"
    rpm-ostree status | sed 's/^/  /'
    cat <<EOF

  systemctl reboot
  # then: scripts/laptop-switch.sh verify
  # if anything is wrong: pick the previous entry in the GRUB menu, or scripts/laptop-switch.sh rollback
EOF
}

verify() {
    bold "After the switch"
    rel=$(cat /usr/lib/melissa/release 2>/dev/null | tr '\n' ' ')
    case $rel in *MELISSA_VARIANT=desktop*) ok "release: $rel" ;; *) bad "not on the Melissa desktop image ('$rel')" ;; esac
    rpm-ostree status --booted | sed 's/^/  /'
    sb=$(mokutil --sb-state 2>&1 | head -n1)
    case $sb in *enabled*) ok "Secure Boot: $sb" ;; *) warn "Secure Boot: $sb" ;; esac
    findmnt -no OPTIONS / | grep -q 'upperdir=/run' && ok "transient root active" || bad "transient root NOT active (/nix cannot mount)"
    findmnt /nix >/dev/null && ok "/nix mounted" || bad "/nix not mounted"
    if command -v nix >/dev/null; then ok "$(nix --version)"; else warn "nix not on PATH in this shell"; fi
    ok "SELinux: $(getenforce 2>/dev/null)"
    if command -v tailscale >/dev/null && tailscale status >/dev/null 2>&1; then ok "tailscale up ($(tailscale ip -4 2>/dev/null | head -n1))"
    else warn "tailscale status failed (systemctl status tailscaled)"; fi
    for u in melissa-queen k3s melissa-kubeconfig tailscaled sshd; do
        s=$(systemctl is-active "$u" 2>/dev/null); [ "$s" = active ] && ok "$u: active" || warn "$u: $s"
    done
    qu=$(systemctl show -p User --value melissa-queen); [ "$qu" = melissa ] && ok "queen runs as user melissa" || warn "queen user: '$qu'"
    curl -fsS --max-time 5 http://127.0.0.1:8080/healthz >/dev/null && ok "web UI: http://127.0.0.1:8080 (healthz ok)" \
        || warn "web UI not answering on 127.0.0.1:8080 yet"
    systemctl is-enabled --quiet melissa-firstboot.service 2>/dev/null && bad "melissa-firstboot is enabled (should not be on desktop)" \
        || ok "no first-boot account prompt on desktop"
    echo "  k3s node (needs sudo):"
    sudo k3s kubectl get nodes 2>&1 | sed 's/^/    /'
    echo "  SELinux denials this boot (needs sudo):"
    n=$(sudo journalctl -b --no-pager 2>/dev/null | grep -c 'avc:  denied' || true)
    [ "${n:-0}" = 0 ] && ok "no AVC denials" || warn "$n AVC denials: sudo journalctl -b | grep 'avc:  denied'"
    echo
    echo "  Next: sudo melissa-setup                (status of the system queen's agents)"
    echo "        sudo melissa-setup agent claude   (or grok, opencode: logins for the SYSTEM queen;"
    echo "                                           your own CLI logins in your home are unchanged)"
    bold "Result: $fails failed, $warns warnings"
    [ "$fails" = 0 ]
}

rollback() {
    rpm-ostree status | sed 's/^/  /'
    bold "Make the previous deployment the default for the next boot"
    step "sudo rpm-ostree rollback"
    echo "  Then: systemctl reboot. (Or, without this: choose the older entry in the GRUB menu at boot.)"
    echo "  /var is shared between deployments: k3s data, /var/lib/melissa and the melissa user's files stay."
}

case $cmd in
    preflight) preflight ;;
    switch) switch ;;
    verify) verify ;;
    rollback) rollback ;;
    *) sed -n '2,12p' "$0" | sed 's/^# \{0,1\}//'; exit 2 ;;
esac
