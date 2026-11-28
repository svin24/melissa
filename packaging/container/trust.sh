# shellcheck shell=bash
# Signature checks for Melissa's published images (sourced by install.sh and uninstall.sh).
#
# Images on ghcr.io/svin24/melissa are signed in GitHub Actions (.github/workflows/container.yml) with
# Melissa's cosign key; the public half is melissa-signing.pub next to this file. install.sh adds one entry
# for that repository to podman's /etc/containers/policy.json (sigstoreSigned, keyPath), so every pull of
# it (install, Settings → Updates, Melissa's update timers) is refused unless the signature matches. Other
# images and the rest of the policy are left as they were. uninstall.sh removes the entry again.
#
# Paths are variables so test-install.sh can run this without root.

SIGNED_REPO=${SIGNED_REPO:-ghcr.io/svin24/melissa}
POLICY=${POLICY:-/etc/containers/policy.json}
POLICY_DEFAULT=${POLICY_DEFAULT:-/usr/share/containers/policy.json}
REGISTRIES_D=${REGISTRIES_D:-/etc/containers/registries.d}
SIGNING_KEY=${SIGNING_KEY:-/etc/pki/containers/melissa.pub}
# jq or python3 edits the JSON; POLICY_TOOL forces one (tests).
POLICY_TOOL=${POLICY_TOOL:-}

# is_published IMAGE: is it Melissa's published image (tag or digest)?
is_published() { case "$1" in "$SIGNED_REPO":* | "$SIGNED_REPO"@*) return 0 ;; *) return 1 ;; esac; }

policy_tool() {
    if [ -n "$POLICY_TOOL" ]; then echo "$POLICY_TOOL"; return 0; fi
    if command -v jq >/dev/null 2>&1; then echo jq
    elif command -v python3 >/dev/null 2>&1; then echo python3
    else return 1; fi
}

# policy_edit add|remove IN OUT [KEYPATH]: IN with the entry for SIGNED_REPO set (add) or deleted
# (remove), written to OUT. Everything else in IN is kept.
policy_edit() {
    local op=$1 in=$2 out=$3 key=${4:-} tool
    tool=$(policy_tool) || return 1
    case "$tool" in
    jq)
        if [ "$op" = add ]; then
            jq --arg r "$SIGNED_REPO" --arg k "$key" \
                '.transports //= {} | .transports.docker //= {}
                 | .transports.docker[$r] = [{"type": "sigstoreSigned", "keyPath": $k,
                                              "signedIdentity": {"type": "matchRepository"}}]' \
                "$in" > "$out"
        else
            # An emptied transports.docker (and transports) goes too, so a policy install.sh created
            # compares equal to the distribution's default again.
            jq --arg r "$SIGNED_REPO" \
                'if .transports.docker then del(.transports.docker[$r]) else . end
                 | if .transports.docker == {} then del(.transports.docker) else . end
                 | if .transports == {} then del(.transports) else . end' \
                "$in" > "$out"
        fi ;;
    python3)
        python3 -I - "$op" "$in" "$out" "$SIGNED_REPO" "$key" <<'PY'
import json, sys
op, src, dst, repo, key = sys.argv[1:6]
with open(src) as f:
    p = json.load(f)
docker = p.setdefault("transports", {}).setdefault("docker", {})
if op == "add":
    docker[repo] = [{"type": "sigstoreSigned", "keyPath": key,
                     "signedIdentity": {"type": "matchRepository"}}]
else:
    docker.pop(repo, None)
    if not docker:
        del p["transports"]["docker"]
    if not p["transports"]:
        del p["transports"]
with open(dst, "w") as f:
    json.dump(p, f, indent=4)
    f.write("\n")
PY
        ;;
    *) return 1 ;;
    esac
}

# policy_same A B: the same JSON (key order and spacing ignored)?
policy_same() {
    case "$(policy_tool)" in
    jq) [ "$(jq -S . "$1")" = "$(jq -S . "$2")" ] ;;
    python3) python3 -I -c 'import json,sys; a,b=(json.load(open(x)) for x in sys.argv[1:3]); sys.exit(a!=b)' "$1" "$2" ;;
    *) return 1 ;;
    esac
}

# trust_install KEYFILE: require Melissa's signature for SIGNED_REPO. Prints "created" when
# POLICY did not exist (it then starts as a copy of the distribution's POLICY_DEFAULT).
trust_install() {
    local keyfile=$1 src=$POLICY created=no tmp
    policy_tool >/dev/null || { echo "need jq or python3 to edit $POLICY" >&2; return 1; }
    if [ ! -e "$POLICY" ]; then
        [ -r "$POLICY_DEFAULT" ] || { echo "no $POLICY and no $POLICY_DEFAULT" >&2; return 1; }
        src=$POLICY_DEFAULT; created=yes
    fi
    install -D -m 0644 "$keyfile" "$SIGNING_KEY"
    mkdir -p "$REGISTRIES_D" "$(dirname "$POLICY")"
    printf '# Melissa (install.sh): read the cosign signatures stored next to the image.\ndocker:\n  %s:\n    use-sigstore-attachments: true\n' \
        "$SIGNED_REPO" > "$REGISTRIES_D/melissa.yaml"
    tmp=$(mktemp "$POLICY.XXXXXX")
    if ! policy_edit add "$src" "$tmp" "$SIGNING_KEY"; then rm -f "$tmp"; return 1; fi
    chmod 0644 "$tmp" && mv -f "$tmp" "$POLICY"
    [ "$created" = yes ] && echo created
    return 0
}

# trust_remove CREATED: take the entry out again; delete POLICY when install created it and it is
# back to the distribution's default.
trust_remove() {
    local created=${1:-no} tmp
    rm -f "$REGISTRIES_D/melissa.yaml" "$SIGNING_KEY"
    [ -e "$POLICY" ] || return 0
    tmp=$(mktemp "$POLICY.XXXXXX")
    if ! policy_edit remove "$POLICY" "$tmp"; then rm -f "$tmp"; return 1; fi
    if [ "$created" = yes ] && [ -r "$POLICY_DEFAULT" ] && policy_same "$tmp" "$POLICY_DEFAULT"; then
        rm -f "$tmp" "$POLICY"
    else
        chmod 0644 "$tmp" && mv -f "$tmp" "$POLICY"
    fi
}
