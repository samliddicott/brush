#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "${repo_root}"

if ! command -v rpmbuild >/dev/null 2>&1; then
  echo "error: rpmbuild is not installed." >&2
  exit 1
fi

"${repo_root}/scripts/build-srpm.sh"

srpm="$(ls -1t "${repo_root}/dist/rpm"/*.src.rpm | head -n1 || true)"
if [[ -z "${srpm}" ]]; then
  echo "error: no SRPM found under dist/rpm" >&2
  exit 1
fi

rpmbuild --rebuild "${srpm}" \
  --define "_rpmdir ${repo_root}/dist/rpm" \
  --define "_srcrpmdir ${repo_root}/dist/rpm"

echo "RPM(s) written under: ${repo_root}/dist/rpm"
