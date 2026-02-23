#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "${repo_root}"

if ! command -v rpmbuild >/dev/null 2>&1; then
  echo "error: rpmbuild is not installed." >&2
  exit 1
fi

version="$(sed -n 's/^version = "\([^"]*\)"$/\1/p' brush-shell/Cargo.toml | head -n1)"
if [[ -z "${version}" ]]; then
  echo "error: failed to read version from brush-shell/Cargo.toml" >&2
  exit 1
fi

name="brush-shell"
out_dir="${repo_root}/dist/rpm"
mkdir -p "${out_dir}"

source_tar="${out_dir}/${name}-${version}.tar.gz"
git archive --format=tar.gz --prefix="${name}-${version}/" -o "${source_tar}" HEAD

rpmbuild -bs packaging/rpm/brush-shell.spec \
  --define "version_override ${version}" \
  --define "_sourcedir ${out_dir}" \
  --define "_srcrpmdir ${out_dir}"

echo "SRPM written to: ${out_dir}"
