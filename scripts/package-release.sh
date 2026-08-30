#!/bin/sh
set -eu

usage() {
  echo "usage: package-release.sh --out <directory> [--target <rust-target>]" >&2
  exit 2
}

script_directory=$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd)
repository=$(CDPATH='' cd -- "$script_directory/.." && pwd)
output=
target=

while [ "$#" -gt 0 ]; do
  case "$1" in
    --out)
      [ "$#" -ge 2 ] || usage
      output=$2
      shift 2
      ;;
    --target)
      [ "$#" -ge 2 ] || usage
      target=$2
      shift 2
      ;;
    *) usage ;;
  esac
done

[ -n "$output" ] || usage
if [ -z "$target" ]; then
  target=$(rustc -vV | awk '/^host: / { print $2 }')
fi
case "$target" in
  *[!A-Za-z0-9_.-]*|'') echo "invalid release target" >&2; exit 2 ;;
esac

version=$(awk '
  /^\[package\]$/ { package = 1; next }
  /^\[/ { package = 0 }
  package && $1 == "version" { gsub(/"/, "", $3); print $3; exit }
' "$repository/Cargo.toml")
[ -n "$version" ] || { echo "could not read package version" >&2; exit 1; }

mkdir -p "$output"
output=$(CDPATH='' cd -- "$output" && pwd)
temporary=$(mktemp -d "${TMPDIR:-/tmp}/driftctl-package.XXXXXX")
trap 'rm -rf "$temporary"' EXIT HUP INT TERM

(
  cd "$repository"
  cargo build --release --locked --target "$target"
)
binary="$repository/target/$target/release/driftctl"
[ -f "$binary" ] || { echo "release binary was not produced" >&2; exit 1; }

install -m 0755 "$binary" "$temporary/driftctl"
archive="driftctl-v${version}-${target}.tar.gz"
tar -czf "$output/$archive" -C "$temporary" driftctl

if command -v sha256sum >/dev/null 2>&1; then
  (cd "$output" && sha256sum "$archive" > "$archive.sha256")
elif command -v shasum >/dev/null 2>&1; then
  (cd "$output" && shasum -a 256 "$archive" > "$archive.sha256")
else
  echo "sha256sum or shasum is required" >&2
  exit 1
fi

printf '%s\n' "$output/$archive" "$output/$archive.sha256"
