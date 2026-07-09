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
rm -rf "$DEST"
mv "$tmp/python" "$DEST"

py="$DEST/bin/python3"
"$py" -m pip install --no-cache-dir "numpy==${NUMPY_VERSION}"

# Trim: remove bytecode caches, the stdlib test suites, and pip/ensurepip.
find "$DEST" -depth -name '__pycache__' -type d -exec rm -rf {} +
rm -rf "$DEST"/lib/python3.12/test "$DEST"/lib/python3.12/tests
rm -rf "$DEST"/lib/python3.12/site-packages/pip "$DEST"/lib/python3.12/ensurepip

echo "$py"
