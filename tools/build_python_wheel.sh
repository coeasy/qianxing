#!/usr/bin/env bash
set -euo pipefail

python_bin="${PYTHON:-python3}"
output_directory="${OUTPUT_DIRECTORY:-dist}"
repository_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
python_project="${repository_root}/python"

# Fail before building if this interpreter cannot run the PEP 517 wheel frontend.
if ! "${python_bin}" -m pip --version >/dev/null 2>&1; then
    echo "[failure] ${python_bin} has no pip; install pip or use uv build --wheel" >&2
    exit 1
fi
if ! "${python_bin}" -m maturin --version >/dev/null 2>&1; then
    echo "[failure] ${python_bin} has no maturin; install it with uv pip install --python ${python_bin} maturin" >&2
    exit 1
fi

mkdir -p "${repository_root}/${output_directory}"
"${python_bin}" -m pip wheel --no-deps --no-build-isolation "${python_project}" \
    --wheel-dir "${repository_root}/${output_directory}"
