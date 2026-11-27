#!/bin/bash
# shellcheck shell=bash
# Melissa's AppArmor profile for its container (host/apparmor/melissa-container). Sourced by
# install.sh and uninstall.sh; run directly by escape-test.sh, gvisor-test.sh and CI:
#
#   sudo packaging/container/apparmor.sh load     install and load the profile (no AppArmor: nothing)
#   sudo packaging/container/apparmor.sh unload   unload and delete it
#
# Ubuntu (26.04) restricts unprivileged user namespaces (kernel.apparmor_restrict_unprivileged_userns)
# and loads bwrap-userns-restrict, whose `bwrap` profile attaches to /usr/bin/bwrap by path, also
# inside the --privileged container (which podman leaves AppArmor-unconfined), and denies agent users
# their capabilities. Melissa's profile covers the whole container instead: flags=(unconfined) with
# `userns,`, the way Ubuntu's own profiles for programs that need user namespaces look. Only Melissa's
# container gets it (podman --security-opt apparmor=melissa-container); the host's restriction stays
# on for everything else. The image's sandbox uses its own bwrap at /usr/libexec/melissa/bwrap, which
# Ubuntu's path-attached profile does not match. README, "Ubuntu and AppArmor".
#
# Paths are variables so test-install.sh can run this without root.

apparmor_here=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
APPARMOR_SYS=${APPARMOR_SYS:-/sys/kernel/security/apparmor}
APPARMOR_PROFILES=${APPARMOR_PROFILES:-$APPARMOR_SYS/profiles}
APPARMOR_DIR=${APPARMOR_DIR:-/etc/apparmor.d}
APPARMOR_PARSER=${APPARMOR_PARSER:-apparmor_parser}
USERNS_SYSCTL=${USERNS_SYSCTL:-/proc/sys/kernel/apparmor_restrict_unprivileged_userns}
MELISSA_APPARMOR_PROFILE=melissa-container
MELISSA_APPARMOR_SRC=${MELISSA_APPARMOR_SRC:-$apparmor_here/host/apparmor/$MELISSA_APPARMOR_PROFILE}

# apparmor_enabled: the host kernel runs AppArmor.
apparmor_enabled() { [ -d "$APPARMOR_SYS" ]; }

# apparmor_parser_found: the tool that loads profiles is installed.
apparmor_parser_found() { command -v "$APPARMOR_PARSER" >/dev/null 2>&1; }

# apparmor_profile_loaded: Melissa's profile is in the kernel.
apparmor_profile_loaded() {
    grep -q "^$MELISSA_APPARMOR_PROFILE " "$APPARMOR_PROFILES" 2>/dev/null
}

# apparmor_podman_args: the podman option that puts a container under Melissa's profile.
apparmor_podman_args() { echo "--security-opt apparmor=$MELISSA_APPARMOR_PROFILE"; }

# apparmor_load: install the profile to APPARMOR_DIR (loaded again after reboots, like every profile
# there) and load it now. On failure: the reason on stdout, status 1.
apparmor_load() {
    local dest=$APPARMOR_DIR/$MELISSA_APPARMOR_PROFILE out
    apparmor_parser_found || { echo "apparmor_parser is not installed (package apparmor)"; return 1; }
    install -m 0644 "$MELISSA_APPARMOR_SRC" "$dest" || { echo "could not write $dest"; return 1; }
    out=$("$APPARMOR_PARSER" -r "$dest" 2>&1) || { echo "apparmor_parser -r $dest failed: $out"; return 1; }
    apparmor_profile_loaded || { echo "$MELISSA_APPARMOR_PROFILE is not in $APPARMOR_PROFILES after loading"; return 1; }
}

# apparmor_unload: unload Melissa's profile and delete its file (nothing there: nothing done).
apparmor_unload() {
    local dest=$APPARMOR_DIR/$MELISSA_APPARMOR_PROFILE
    [ -e "$dest" ] || return 0
    if apparmor_profile_loaded && apparmor_parser_found; then "$APPARMOR_PARSER" -R "$dest" >/dev/null 2>&1 || true; fi
    rm -f "$dest"
}

# --- Ubuntu's restriction (what happens without Melissa's profile) ---------------------------

# bwrap_profile_loaded: a restricting bwrap profile is loaded (`unpriv_bwrap`, or a `bwrap` that
# isn't flags=(unconfined), e.g. the one docs/DEV-ENV.md suggests for user namespaces).
bwrap_profile_loaded() {
    [ -r "$APPARMOR_PROFILES" ] || return 1
    grep -E '^unpriv_bwrap |^bwrap ' "$APPARMOR_PROFILES" 2>/dev/null | grep -qv '(unconfined)$'
}

# userns_restricted: kernel.apparmor_restrict_unprivileged_userns is 1.
userns_restricted() { [ -r "$USERNS_SYSCTL" ] && [ "$(cat "$USERNS_SYSCTL")" = 1 ]; }

# apparmor_restricts: without Melissa's profile, the container's agent sandbox would fail here.
apparmor_restricts() { bwrap_profile_loaded || userns_restricted; }

# bwrap_profile_files: the files in APPARMOR_DIR with a profile that attaches to /usr/bin/bwrap
# and isn't unconfined, one per line.
bwrap_profile_files() {
    local f
    for f in "$APPARMOR_DIR"/*; do
        [ -f "$f" ] || continue
        grep -E '^[[:space:]]*profile[[:space:]]+[^[:space:]]+[[:space:]]+/usr/bin/bwrap([[:space:]]|$)' "$f" 2>/dev/null \
            | grep -qv 'unconfined' && echo "$f"
    done
    return 0
}

# apparmor_fallback_message REASON: Melissa's profile could not be loaded on a host that restricts:
# what is blocked, the fix, and (last resort) the commands that turn Ubuntu's restriction off.
apparmor_fallback_message() {
    local files f userns=no
    files=$(bwrap_profile_files)
    userns_restricted && userns=yes
    cat <<EOF
FAIL: this host's AppArmor restricts user namespaces and bwrap (Ubuntu), and Melissa's own AppArmor
  profile could not be loaded: $1
  What is blocked: Melissa's agents can't run in their sandbox as their own users. Normally
  install.sh loads the profile $MELISSA_APPARMOR_PROFILE ($APPARMOR_DIR/$MELISSA_APPARMOR_PROFILE), which lets
  only Melissa's container create user namespaces; the host's restriction stays on.
EOF
    if apparmor_parser_found; then
        echo "  Fix that (the profile needs AppArmor 4: abi/4.0 and the userns rule) and run install.sh again."
    else
        echo "  Fix that (install the apparmor package: sudo apt install apparmor) and run install.sh again."
    fi
    cat <<EOF
  Last resort, if the profile can't be used: turn Ubuntu's restriction off for the whole host, now
  and after reboots (apparmor_parser skips files listed in $APPARMOR_DIR/disable/):
EOF
    if bwrap_profile_loaded; then
        if [ -z "$files" ]; then
            echo "    (no file in $APPARMOR_DIR attaches to /usr/bin/bwrap; find it with:"
            echo "     grep -lE 'profile +[^ ]+ +/usr/bin/bwrap' -r /etc/apparmor.d)"
            files="$APPARMOR_DIR/<that file>"
        fi
        for f in $files; do
            echo "    sudo ln -s $f $APPARMOR_DIR/disable/"
            echo "    sudo apparmor_parser -R $f"
        done
    else
        files=''
    fi
    if [ "$userns" = yes ]; then
        cat <<EOF
    echo 'kernel.apparmor_restrict_unprivileged_userns = 0' | sudo tee /etc/sysctl.d/60-melissa-userns.conf
    sudo sysctl -w kernel.apparmor_restrict_unprivileged_userns=0
EOF
    fi
    echo "  Undo later:"
    for f in $files; do
        echo "    sudo rm $APPARMOR_DIR/disable/${f##*/} && sudo apparmor_parser -r $f"
    done
    if [ "$userns" = yes ]; then
        echo "    sudo rm /etc/sysctl.d/60-melissa-userns.conf && sudo sysctl -w kernel.apparmor_restrict_unprivileged_userns=1"
    fi
    cat <<EOF
  Caveats of the last resort:
  - It turns off an Ubuntu security restriction for the whole host, not just for Melissa.
  - Any program can then use bwrap to create user namespaces, which is what the restriction prevents.
EOF
    [ "$userns" = yes ] && echo "  - With the sysctl at 0, any program can create user namespaces, with or without bwrap."
    cat <<EOF
  - Melissa's container is rootful and --privileged anyway: root in it is root on this host.
  Then run install.sh again. Details: packaging/container/README.md, "Ubuntu and AppArmor".
EOF
}

# Sourced: only the functions.
[ "${BASH_SOURCE[0]}" = "$0" ] || return 0

set -euo pipefail
case "${1:-}" in
load)
    apparmor_enabled || { echo "apparmor.sh: no AppArmor on this host; nothing to load"; exit 0; }
    if reason=$(apparmor_load); then
        echo "apparmor.sh: loaded $MELISSA_APPARMOR_PROFILE ($APPARMOR_DIR/$MELISSA_APPARMOR_PROFILE)"
    else
        echo "apparmor.sh: $reason" >&2; exit 1
    fi ;;
unload) apparmor_unload; echo "apparmor.sh: removed $MELISSA_APPARMOR_PROFILE" ;;
*) sed -n '2,8p' "$0"; exit 2 ;;
esac
