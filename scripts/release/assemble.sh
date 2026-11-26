#!/usr/bin/env bash
# Assembles release assets from per-target archives:
#   scripts/release/assemble.sh <version> <archive-dir> <out-dir>
# Writes the archives, install.sh, install.ps1 (hashes embedded),
# release-manifest.json (read by `smbs upgrade`), and SHA256SUMS covering all
# of them to <out-dir>. Requires jq and sha256sum.
set -euo pipefail

version="$1"
archives="$2"
out="$3"
root="$(cd "$(dirname "$0")/../.." && pwd)"
targets="${root}/release-targets.json"

[[ "$version" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]] || { echo "invalid version: $version" >&2; exit 1; }
mkdir -p "$out"

sed_args=(-e "s/@@VERSION@@/${version}/g")
manifest_targets='{}'
while IFS=$'\t' read -r key format; do
  name="smbs-${version}-${key}.${format}"
  [[ -f "${archives}/${name}" ]] || { echo "missing archive: ${name}" >&2; exit 1; }
  cp "${archives}/${name}" "${out}/${name}"
  hash="$(sha256sum "${out}/${name}" | cut -d ' ' -f 1)"
  sed_args+=(-e "s/@@SHA256_${key}@@/${hash}/g")
  manifest_targets="$(jq -c --arg key "$key" --arg asset "$name" --arg sha "$hash" '. + {($key): {asset: $asset, sha256: $sha}}' <<<"$manifest_targets")"
done < <(jq -r '.targets[] | [.key, .archive] | @tsv' "$targets")

for installer in install.sh install.ps1; do
  sed "${sed_args[@]}" "${root}/scripts/install/${installer}.in" > "${out}/${installer}"
  if grep -q '@@' "${out}/${installer}"; then
    echo "unfilled placeholder in ${installer}" >&2
    exit 1
  fi
done
chmod 755 "${out}/install.sh"

jq -n --arg version "$version" --argjson targets "$manifest_targets" \
  '{schemaVersion: 1, version: $version, tag: ("v" + $version), targets: $targets}' \
  > "${out}/release-manifest.json"

(cd "$out" && sha256sum smbs-* install.sh install.ps1 release-manifest.json > SHA256SUMS)
cat "${out}/SHA256SUMS"
