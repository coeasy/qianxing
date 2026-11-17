#!/usr/bin/env bash
set -euo pipefail

python_bin="${PYTHON:-python3}"
output_directory="${OUTPUT_DIRECTORY:-dist}"
repository_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
python_project="${repository_root}/python"
if [[ "${output_directory}" == /* ]]; then
    wheel_output_directory="${output_directory}"
else
    wheel_output_directory="${repository_root}/${output_directory}"
fi

# Maturin and its CycloneDX SBOM use this timestamp. Derive it from the source commit so
# repeated builds of the same checkout produce byte-identical wheel metadata and SBOM.
if [[ -z "${SOURCE_DATE_EPOCH:-}" ]]; then
    export SOURCE_DATE_EPOCH="$(git -C "${repository_root}" show -s --format=%ct HEAD)"
fi

# Fail before building if this interpreter cannot run pip and maturin.
if ! "${python_bin}" -m pip --version >/dev/null 2>&1; then
    echo "[failure] ${python_bin} has no pip; install Python with pip enabled before building the wheel" >&2
    exit 1
fi

if ! "${python_bin}" -m maturin --version >/dev/null 2>&1; then
    echo "[failure] ${python_bin} has no maturin 1.15.0; install it with uv pip install --python ${python_bin} maturin==1.15.0" >&2
    exit 1
fi

# The wheel carries the platform report renderer so a clean venv can run `project report`
# without a second install (docs plan D3 option A).
cargo build --release --locked -p qx-cli
mkdir -p "${python_project}/qianxing/_bin"
cp "${repository_root}/target/release/qx-cli" "${python_project}/qianxing/_bin/qx-cli"

mkdir -p "${wheel_output_directory}"
mkdir -p "${repository_root}/target/maturin"
(cd "${python_project}" && "${python_bin}" -m maturin build --release --locked \
    --out "${wheel_output_directory}")
