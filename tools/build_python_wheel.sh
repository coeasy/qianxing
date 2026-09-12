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
    candidate="$(find "${target_directory}" -maxdepth 1 -type f -name "_qianxing_native*.${suffix}" -print -quit 2>/dev/null || true)"
    if [[ -n "${candidate}" ]]; then
        native="${candidate}"
        break
    fi
done
if [[ -z "${native}" ]]; then
    echo "native extension artifact was not found in ${target_directory}" >&2
    exit 1
fi

for existing in "${package_directory}"/_qianxing_native.{pyd,so,dll,dylib}; do
    if [[ -f "${existing}" ]]; then
        rm -f "${existing}"
    fi
done
cp -f "${native}" "${package_directory}/$(basename "${native}")"
mkdir -p "${repository_root}/${output_directory}"
"${python_bin}" -m pip wheel --no-deps "${python_project}" \
    --wheel-dir "${repository_root}/${output_directory}"
