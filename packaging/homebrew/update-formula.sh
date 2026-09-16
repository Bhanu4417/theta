#!/usr/bin/env bash
# Rewrite the Homebrew formula for a published release.
#
#   ./update-formula.sh 0.1.0
#
# Reads SHA256SUMS from the release and substitutes the version and the four
# platform hashes into theta.rb, in place. Run it after `git tag` has been
# pushed and the release workflow has finished.
set -euo pipefail

version="${1:-}"
[ -n "$version" ] || { echo "usage: $0 <version>   e.g. $0 0.1.0" >&2; exit 2; }
version="${version#v}"

here="$(cd "$(dirname "$0")" && pwd)"
repo="Bhanu4417/theta"
sums_url="https://github.com/$repo/releases/download/v$version/SHA256SUMS"

echo "Fetching $sums_url"
sums=$(curl -fsSL "$sums_url") || {
    echo "error: could not fetch SHA256SUMS for v$version" >&2
    echo "       has the release workflow finished?" >&2
    exit 1
}

# Hash for an asset name, from "hash  name" lines.
hash_for() {
    local name="$1" h
    h=$(printf '%s\n' "$sums" | awk -v a="$name" '$2 == a { print $1; exit }')
    [ -n "$h" ] || { echo "error: no checksum for $name in SHA256SUMS" >&2; exit 1; }
    printf '%s' "$h"
}

a_arm_mac=$(hash_for "theta-aarch64-apple-darwin.tar.gz")
a_int_mac=$(hash_for "theta-x86_64-apple-darwin.tar.gz")
a_arm_lin=$(hash_for "theta-aarch64-unknown-linux-gnu.tar.gz")
a_int_lin=$(hash_for "theta-x86_64-unknown-linux-gnu.tar.gz")

formula="$here/theta.rb"
before=$(cat "$formula")

python3 - "$formula" "$version" "$a_arm_mac" "$a_int_mac" "$a_arm_lin" "$a_int_lin" <<'PY'
import re, sys
path, version, mac_arm, mac_int, lin_arm, lin_int = sys.argv[1:7]
src = open(path).read()
src = re.sub(r'  version "[^"]*"', f'  version "{version}"', src, count=1)
for placeholder, value in (
    ("REPLACE_WITH_AARCH64_APPLE_DARWIN_SHA256", mac_arm),
    ("REPLACE_WITH_X86_64_APPLE_DARWIN_SHA256", mac_int),
    ("REPLACE_WITH_AARCH64_LINUX_SHA256", lin_arm),
    ("REPLACE_WITH_X86_64_LINUX_SHA256", lin_int),
):
    src = src.replace(placeholder, value)
# Replace real hashes on re-runs, not just the placeholders.
lines = src.split("\n")
order = [mac_arm, mac_int, lin_arm, lin_int]
i = 0
for n, line in enumerate(lines):
    if re.match(r'\s*sha256 "[0-9a-fA-F]{64}"', line):
        lines[n] = re.sub(r'"[0-9a-fA-F]{64}"', f'"{order[i % 4]}"', line)
        i += 1
src = "\n".join(lines)
open(path, "w").write(src)
print(f"updated {path} for version {version}")
PY

echo "--- diff ---"
diff <(printf '%s\n' "$before") "$formula" || true
echo
echo "Copy $formula into Formula/theta.rb in the homebrew-tap repository."
