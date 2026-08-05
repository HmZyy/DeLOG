#!/usr/bin/env bash
set -euo pipefail

PBS_TAG="20240814"                 # python-build-standalone release tag
PY_VERSION="3.12.5"
NUMPY_VERSION="2.1.1"
SCIPY_VERSION="1.14.1"
BOTTLENECK_VERSION="1.4.0"
CFFI_VERSION="1.17.1"
DEST="${1:?usage: fetch-python-standalone.sh <dest-dir>}"

url="https://github.com/astral-sh/python-build-standalone/releases/download/${PBS_TAG}/cpython-${PY_VERSION}+${PBS_TAG}-x86_64-unknown-linux-gnu-install_only.tar.gz"

tmp="$(mktemp -d)"
curl -fL "$url" -o "$tmp/py.tar.gz"
tar -xzf "$tmp/py.tar.gz" -C "$tmp"          # extracts a `python/` dir
mkdir -p "$(dirname "$DEST")"  # CI passes <repo>/staging/python; ensure the parent exists before the move.
rm -rf "$DEST"
mv "$tmp/python" "$DEST"

py="$DEST/bin/python3"
"$py" -m pip install --no-cache-dir \
    "numpy==${NUMPY_VERSION}" \
    "scipy==${SCIPY_VERSION}" \
    "bottleneck==${BOTTLENECK_VERSION}" \
    "cffi==${CFFI_VERSION}" >&2

# Trim: remove bytecode caches, the stdlib test suites, pip/ensurepip, and the
# bundled SciPy test tree (large, never imported at runtime). Scoped to SciPy
# only - Bottleneck imports its own `tests` package at load time.
py_minor="${PY_VERSION%.*}"   # e.g. 3.12
site="$DEST/lib/python${py_minor}/site-packages"
find "$DEST" -depth -name '__pycache__' -type d -exec rm -rf {} +
rm -rf "$DEST/lib/python${py_minor}/test" "$DEST/lib/python${py_minor}/tests"
rm -rf "$site/pip" "$DEST/lib/python${py_minor}/ensurepip"
find "$site/scipy" -depth -type d -name tests -exec rm -rf {} +

echo "$py"
