#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
crate_dir="${repo_root}/brush-shell"
cd "${crate_dir}"

if ! command -v cargo-deb >/dev/null 2>&1; then
  echo "error: cargo-deb is not installed." >&2
  echo "install with: cargo install cargo-deb" >&2
  exit 1
fi

exec cargo deb "$@"
