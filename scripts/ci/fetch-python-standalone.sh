#!/usr/bin/env bash
set -euo pipefail

PBS_TAG="20240814"                 # python-build-standalone release tag
PY_VERSION="3.12.5"
NUMPY_VERSION="2.1.1"
DEST="${1:?usage: fetch-python-standalone.sh <dest-dir>}"

url="https://github.com/astral-sh/python-build-standalone/releases/download/${PBS_TAG}/cpython-${PY_VERSION}+${PBS_TAG}-x86_64-unknown-linux-gnu-install_only.tar.gz"

tmp="$(mktemp -d)"
curl -fL "$url" -o "$tmp/py.tar.gz"
tar -xzf "$tmp/py.tar.gz" -C "$tmp"          # extracts a `python/` dir
mkdir -p "$(dirname "$DEST")"  # CI passes <repo>/staging/python; ensure the parent exists before the move.
rm -rf "$DEST"
mv "$tmp/python" "$DEST"

py="$DEST/bin/python3"
"$py" -m pip install --no-cache-dir "numpy==${NUMPY_VERSION}" >&2

# Trim: remove bytecode caches, the stdlib test suites, and pip/ensurepip.
py_minor="${PY_VERSION%.*}"   # e.g. 3.12
find "$DEST" -depth -name '__pycache__' -type d -exec rm -rf {} +
rm -rf "$DEST/lib/python${py_minor}/test" "$DEST/lib/python${py_minor}/tests"
rm -rf "$DEST/lib/python${py_minor}/site-packages/pip" "$DEST/lib/python${py_minor}/ensurepip"

echo "$py"
