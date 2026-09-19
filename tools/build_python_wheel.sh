#!/usr/bin/env bash
set -euo pipefail

python_bin="${PYTHON:-python3}"
profile="${PROFILE:-release}"
output_directory="${OUTPUT_DIRECTORY:-dist}"
repository_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
python_project="${repository_root}/python"
target_directory="${repository_root}/target/${profile}"
package_directory="${python_project}/qianxing_bridge"

cargo_args=(build -p qx-python)
if [[ "${profile}" == "release" ]]; then
    cargo_args+=(--release)
elif [[ "${profile}" != "debug" ]]; then
    echo "PROFILE must be debug or release" >&2
    exit 2
fi

(
    cd "${repository_root}"
    cargo "${cargo_args[@]}"
)

native=""
for suffix in so dylib dll; do
    for stem in "_qianxing_native*" "lib_qianxing_native*"; do
        candidate="$(find "${target_directory}" -maxdepth 1 -type f -name "${stem}.${suffix}" -print -quit 2>/dev/null || true)"
        if [[ -n "${candidate}" ]]; then
            native="${candidate}"
            break 2
        fi
    done
done
if [[ -z "${native}" ]]; then
    echo "native extension artifact was not found in ${target_directory}" >&2
    exit 1
fi

# Cargo 的 cdylib 产物名与 Python 的导入名不一致：Linux/macOS 带 `lib` 前缀且
# 后缀是 `.so`/`.dylib`，Windows 是 `.dll`。Python 只会导入
# `_qianxing_native.so`（Unix）或 `_qianxing_native.pyd`（Windows）。
case "$(uname -s)" in
    Linux* | Darwin*) import_suffix="so" ;;
    *) import_suffix="pyd" ;;
esac
import_name="_qianxing_native.${import_suffix}"
cp -f "${native}" "$(dirname "${native}")/${import_name}"

for existing in "${package_directory}"/_qianxing_native.{pyd,so,dll,dylib}; do
    if [[ -f "${existing}" ]]; then
        rm -f "${existing}"
    fi
done
cp -f "${native}" "${package_directory}/${import_name}"
mkdir -p "${repository_root}/${output_directory}"
"${python_bin}" -m pip wheel --no-deps "${python_project}" \
    --wheel-dir "${repository_root}/${output_directory}"
