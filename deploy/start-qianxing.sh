#!/usr/bin/env bash
set -Eeuo pipefail

script_dir="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
config_path="${1:-${script_dir}/qianxing.runtime.production.example.json}"
binary_path="${QX_BINARY:-${script_dir}/../target/release/qx-cli}"

if [[ ! -f "$config_path" ]]; then
  echo "runtime config not found: $config_path" >&2
  exit 2
fi
if [[ ! -x "$binary_path" ]]; then
  echo "qx-cli executable not found or not executable: $binary_path" >&2
  exit 2
fi

shift || true
exec "$binary_path" supervise "$config_path" "$@"
