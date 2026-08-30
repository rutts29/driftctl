#!/bin/sh
set -eu

usage() {
  echo "usage: install.sh --version <vX.Y.Z> [--target <rust-target>] [--bin-dir <directory>]" >&2
  exit 2
}

version=
target=
binary_directory=${HOME:+$HOME/.local/bin}
base_url=${DRIFTCTL_BASE_URL:-https://github.com/rutts29/driftctl/releases/download}

while [ "$#" -gt 0 ]; do
  case "$1" in
    --version)
      [ "$#" -ge 2 ] || usage
      version=$2
      shift 2
      ;;
    --target)
      [ "$#" -ge 2 ] || usage
      target=$2
      shift 2
      ;;
    --bin-dir)
      [ "$#" -ge 2 ] || usage
      binary_directory=$2
      shift 2
      ;;
    *) usage ;;
  esac
done

[ -n "$version" ] || usage
[ -n "$binary_directory" ] || { echo "--bin-dir is required when HOME is unset" >&2; exit 2; }
printf '%s\n' "$version" | grep -Eq '^v[0-9]+\.[0-9]+\.[0-9]+([-.][0-9A-Za-z.-]+)?$' || {
  echo "version must be pinned as vX.Y.Z" >&2
  exit 2
}

if [ -z "$target" ]; then
  machine=$(uname -m)
  system=$(uname -s)
  case "$system:$machine" in
    Linux:x86_64) target=x86_64-unknown-linux-gnu ;;
    *) echo "unsupported platform: $system $machine" >&2; exit 1 ;;
  esac
fi
case "$target" in
  x86_64-unknown-linux-gnu) ;;
  *) echo "unsupported release target: $target" >&2; exit 1 ;;
esac

command -v curl >/dev/null 2>&1 || { echo "curl is required" >&2; exit 1; }
command -v tar >/dev/null 2>&1 || { echo "tar is required" >&2; exit 1; }

archive="driftctl-${version}-${target}.tar.gz"
temporary=$(mktemp -d "${TMPDIR:-/tmp}/driftctl-install.XXXXXX")
trap 'rm -rf "$temporary"' EXIT HUP INT TERM
curl -fsSL "$base_url/$version/$archive" -o "$temporary/$archive"
curl -fsSL "$base_url/$version/$archive.sha256" -o "$temporary/$archive.sha256"

[ "$(wc -l < "$temporary/$archive.sha256" | tr -d ' ')" = 1 ] || {
  echo "invalid checksum manifest" >&2
  exit 1
}
expected=$(awk -v file="$archive" 'NF == 2 && $2 == file { print $1 }' "$temporary/$archive.sha256")
printf '%s\n' "$expected" | grep -Eq '^[0-9a-fA-F]{64}$' || {
  echo "invalid checksum manifest" >&2
  exit 1
}
if command -v sha256sum >/dev/null 2>&1; then
  actual=$(sha256sum "$temporary/$archive" | awk '{ print $1 }')
elif command -v shasum >/dev/null 2>&1; then
  actual=$(shasum -a 256 "$temporary/$archive" | awk '{ print $1 }')
else
  echo "sha256sum or shasum is required" >&2
  exit 1
fi
[ "$actual" = "$expected" ] || { echo "archive checksum mismatch" >&2; exit 1; }

entries=$(tar -tzf "$temporary/$archive")
[ "$entries" = "driftctl" ] || { echo "release archive has unexpected entries" >&2; exit 1; }
tar -xzf "$temporary/$archive" -C "$temporary" -- driftctl
[ -f "$temporary/driftctl" ] || { echo "release archive has no driftctl binary" >&2; exit 1; }
chmod 0755 "$temporary/driftctl"
"$temporary/driftctl" --help >/dev/null

mkdir -p "$binary_directory"
staged="$binary_directory/.driftctl.install.$$"
trap 'rm -rf "$temporary"; rm -f "$staged"' EXIT HUP INT TERM
install -m 0755 "$temporary/driftctl" "$staged"
mv -f "$staged" "$binary_directory/driftctl"
printf 'installed driftctl %s to %s\n' "$version" "$binary_directory/driftctl"
