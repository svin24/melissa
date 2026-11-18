#!/usr/bin/env bash
# Prepare vm/<variant>/: a throwaway SSH key and a bootc-image-builder config with a test user.
# Everything under vm/ is gitignored. Never reuse these credentials outside local test VMs.
set -euo pipefail
variant=${1:?usage: vm-prep.sh <desktop|headless>}
dir=vm/$variant
mkdir -p "$dir"
[ -f vm/test_key ] || ssh-keygen -q -t ed25519 -N '' -C melissa-test-vm -f vm/test_key
[ -f vm/test_password ] || head -c 12 /dev/urandom | base64 | tr -d '/+=' > vm/test_password
cat > "$dir/config.toml" <<TOML
[[customizations.user]]
name = "bee"
password = "$(cat vm/test_password)"
key = "$(cat vm/test_key.pub)"
groups = ["wheel"]
TOML
chmod 600 "$dir/config.toml" vm/test_password
echo "prepared $dir (user: bee, password in vm/test_password, key vm/test_key)"
