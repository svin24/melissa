#!/usr/bin/env bash
# Release check: every crate of the main workspace (crates/*/Cargo.toml) has the same version, and it
# equals the release tag.
#
#   scripts/release-check.sh v0.0.3      (or 0.0.3; the leading v is dropped)
#   scripts/release-check.sh             only check that the versions are equal; prints the version
#
# crates/escape-probe is skipped: a test-only helper in its own workspace, never shipped.
# Used by .github/workflows/container.yml on tags; also runs locally (no network, no root).
set -euo pipefail

cd "$(dirname "$0")/.."
want=${1:-}
want=${want#v}
case "$want" in
"" | [0-9]*.[0-9]*.[0-9]*) ;;
*) echo "release-check: '$1' is not a version tag (vX.Y.Z)" >&2; exit 2 ;;
esac

version=
ok=yes
for toml in crates/*/Cargo.toml; do
    # A crate with its own [workspace] is not part of the release.
    grep -q '^\[workspace\]' "$toml" && continue
    v=$(sed -n 's/^version = "\(.*\)"/\1/p' "$toml" | head -n 1)
    if [ -z "$v" ]; then
        echo "release-check: no version in $toml" >&2; ok=no; continue
    fi
    [ -n "$version" ] || version=$v
    if [ "$v" != "$version" ]; then
        echo "release-check: $toml is $v, other crates are $version" >&2; ok=no
    fi
    if [ -n "$want" ] && [ "$v" != "$want" ]; then
        echo "release-check: $toml is $v, the tag is $want" >&2; ok=no
    fi
done
[ -n "$version" ] || { echo "release-check: no crates found" >&2; exit 1; }
[ "$ok" = yes ] || exit 1
echo "$version"
