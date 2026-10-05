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

# 与 start-qianxing.ps1 同一格前置闸门：`supervise` 只做拓扑校验（plan_workers），
# 不检查配置引用的文件是否真的存在——数据集 bundle、研究快照、秘密文件的存在性只
# 在 runtime-check / live-check 里查。缺一个文件时各 worker 会各自在启动阶段失败，
# 监督器随后按 fail-fast 把其余进程停掉：7 个子进程被拉起又杀掉、process-logs 里
# 落下日志，而不是在任何子进程起来之前就拒绝。两个启动器的闸门口径应当一致（V13 R14）。
# 退出码固定 2 而不是 `$?`：`$?` 在此刻拿的是上一条 echo 的状态（0），会让调用方
# 以为闸门通过了；与本脚本另外两处拒绝（配置缺失、可执行文件缺失）同码。
if ! "$binary_path" runtime-check "$config_path"; then
  echo "runtime-check failed; no child process was started" >&2
  exit 2
fi

shift || true
exec "$binary_path" supervise "$config_path" "$@"
