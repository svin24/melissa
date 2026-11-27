#!/bin/sh
# Melissa's one-line installer (container install). Run as root:
#
#   curl -fsSL https://raw.githubusercontent.com/svin24/melissa/master/get.sh | sudo sh
#   curl -fsSL https://raw.githubusercontent.com/svin24/melissa/master/get.sh | sudo CHANNEL=stable sh
#   curl -fsSL https://raw.githubusercontent.com/svin24/melissa/master/get.sh | sudo sh -s -- --preflight
#
# What it does:
#   1. Checks the host: root, Linux, systemd, podman 5 or newer, curl, tar, bash.
#   2. Downloads Melissa's source tarball from GitHub (codeload.github.com) into a temporary
#      directory and extracts packaging/container/ from it.
#   3. Runs packaging/container/install.sh from there (its own preflight runs first), with this
#      environment and the --preflight argument passed on. Settings: packaging/container/README.md.
#   4. Deletes the temporary directory. install.sh keeps uninstall.sh on the host:
#      sudo /usr/local/libexec/melissa/uninstall.sh [--purge]
#
# Which source it downloads:
#   MELISSA_REF set   that branch, tag or commit
#   CHANNEL=edge      master (the default; the edge image is built from master)
#   CHANNEL=stable    the latest release tag (v*), read from the redirect of
#                     https://github.com/svin24/melissa/releases/latest (no API token). No release
#                     yet: it stops and says to use edge.
# Nothing else is downloaded here; install.sh then pulls the container image.
set -eu

REPO=svin24/melissa
RELEASES_LATEST=https://github.com/$REPO/releases/latest
TARBALL_BASE=https://codeload.github.com/$REPO/tar.gz
SYSTEMD_RUN=/run/systemd/system

say() { echo "melissa-get: $*"; }
die() { echo "melissa-get: $*" >&2; exit 1; }

usage() {
    cat <<EOF
usage: get.sh [--preflight]
  curl -fsSL https://raw.githubusercontent.com/$REPO/master/get.sh | sudo sh
  curl -fsSL https://raw.githubusercontent.com/$REPO/master/get.sh | sudo sh -s -- --preflight
  Settings go between sudo and sh, e.g. sudo CHANNEL=stable sh (see packaging/container/README.md).
EOF
}

# get_curl ARGS...: curl over HTTPS only.
get_curl() { curl --proto '=https' --tlsv1.2 -fsS "$@"; }

# host_problems: one line per missing requirement (nothing printed: the host is fine).
host_problems() {
    v=
    [ "$(id -u)" = 0 ] || echo "run as root (curl ... | sudo sh)"
    [ "$(uname -s)" = Linux ] || echo "Melissa's container install needs Linux"
    [ -d "$SYSTEMD_RUN" ] || echo "systemd is not running as PID 1"
    for c in curl tar bash; do
        command -v "$c" >/dev/null 2>&1 || echo "$c is not installed"
    done
    if command -v podman >/dev/null 2>&1; then
        v=$(podman version --format '{{.Client.Version}}' 2>/dev/null || true)
        case "${v%%.*}" in
        '' | *[!0-9]*) echo "could not read podman's version" ;;
        *) [ "${v%%.*}" -ge 5 ] || echo "podman $v is too old: need 5.0 or newer (Quadlet)" ;;
        esac
    else
        echo "podman is not installed (need 5.0 or newer)"
    fi
}

# valid_ref REF: a branch, tag or commit name (letters, digits, . _ - /; no ..).
valid_ref() {
    case "$1" in
    '' | -* | *..* | *[!A-Za-z0-9._/-]*) return 1 ;;
    *) return 0 ;;
    esac
}

# latest_tag: the tag GitHub's releases/latest redirects to (stdout), or status 1.
latest_tag() {
    loc=$(get_curl -I "$RELEASES_LATEST" 2>/dev/null | tr -d '\r' | sed -n 's/^[Ll][Oo][Cc][Aa][Tt][Ii][Oo][Nn]: *//p' | tail -n 1)
    case "$loc" in
    */releases/tag/v*) tag=${loc##*/releases/tag/} ;;
    *) return 1 ;;
    esac
    valid_ref "$tag" || return 1
    echo "$tag"
}

# pick_ref: the source ref to download (stdout), from MELISSA_REF or CHANNEL.
pick_ref() {
    if [ -n "${MELISSA_REF:-}" ]; then
        valid_ref "$MELISSA_REF" || { echo "melissa-get: MELISSA_REF '$MELISSA_REF' is not a branch, tag or commit name" >&2; return 1; }
        echo "$MELISSA_REF"
        return 0
    fi
    case "${CHANNEL:-edge}" in
    edge) echo master ;;
    stable)
        latest_tag && return 0
        echo "melissa-get: there is no stable release yet ($RELEASES_LATEST has no v* tag)." >&2
        echo "melissa-get: use the edge channel (the default: leave CHANNEL out), or set MELISSA_REF." >&2
        return 1 ;;
    *) echo "melissa-get: CHANNEL must be edge or stable" >&2; return 1 ;;
    esac
}

# fetch REF DIR: download the tarball of REF into DIR and extract packaging/container/; prints
# the path of that directory.
fetch() {
    get_curl -L -o "$2/melissa.tar.gz" "$TARBALL_BASE/$1" || { echo "melissa-get: download of $TARBALL_BASE/$1 failed" >&2; return 1; }
    top=$(tar -tzf "$2/melissa.tar.gz" | head -n 1)
    top=${top%%/*}
    [ -n "$top" ] || { echo "melissa-get: the download is not a source tarball" >&2; return 1; }
    tar -xzf "$2/melissa.tar.gz" -C "$2" "$top/packaging/container" \
        || { echo "melissa-get: no packaging/container in the tarball of $1" >&2; return 1; }
    [ -f "$2/$top/packaging/container/install.sh" ] || { echo "melissa-get: no install.sh in the tarball of $1" >&2; return 1; }
    echo "$2/$top/packaging/container"
}

main() {
    case "${1:-}" in
    '' | --preflight) ;;
    -h | --help) usage; exit 0 ;;
    *) usage >&2; exit 2 ;;
    esac
    [ "$#" -le 1 ] || { usage >&2; exit 2; }

    say "Melissa container install: check this host, download Melissa's source from GitHub"
    say "  (codeload.github.com) into a temporary directory, run packaging/container/install.sh${1:+ $1}"
    say "  from it, then delete the directory. Undo later: sudo /usr/local/libexec/melissa/uninstall.sh"

    problems=$(host_problems)
    if [ -n "$problems" ]; then
        printf '%s\n' "$problems" | sed 's/^/melissa-get: FAIL: /' >&2
        exit 1
    fi
    ref=$(pick_ref) || exit 1

    tmp=$(mktemp -d "${TMPDIR:-/tmp}/melissa-get.XXXXXX")
    trap 'rm -rf "$tmp"' EXIT
    trap 'exit 1' HUP INT TERM
    say "downloading $TARBALL_BASE/$ref"
    dir=$(fetch "$ref" "$tmp") || exit 1
    say "running install.sh${1:+ $1} (source: $ref)"
    # stdin is this script when piped to sh: install.sh reads nothing from it.
    status=0
    bash "$dir/install.sh" "$@" </dev/null || status=$?
    exit "$status"
}

# Tests (packaging/container/test-get.sh) set GET_SH_SOURCED=yes to load only the functions.
[ "${GET_SH_SOURCED:-no}" = yes ] || main "$@"
