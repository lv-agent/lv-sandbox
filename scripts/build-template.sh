#!/usr/bin/env bash
# cr-025: Pre-install a template's Python packages into a read-only dir.
#
# A "template" is a profile that bundles a pre-baked package set (this dir)
# plus baseline env vars (PYTHONPATH) so the runtime finds them. Run this once
# when building the worker image, then reference the dir from a profile.
#
# Usage: build-template.sh <name> "<pip install args...>"
#   e.g.  build-template.sh data-science "pandas numpy scikit-learn"
#
# Then in config.yaml:
#   profiles:
#     data-science:
#       extra_readonly_paths: ["/opt/templates/data-science"]
#       env: { PYTHONPATH: "/opt/templates/data-science" }
set -euo pipefail

if [ "$#" -lt 2 ]; then
    echo "usage: $0 <name> <pip install args...>" >&2
    echo "  e.g. $0 data-science \"pandas numpy scikit-learn\"" >&2
    exit 2
fi

name="$1"
shift
target="/opt/templates/${name}"

echo "[build-template] installing into ${target}"
pip install --break-system-packages --target "${target}" "$@"

echo "[build-template] done. reference from a profile:"
echo "  extra_readonly_paths: [\"${target}\"]"
echo "  env: { PYTHONPATH: \"${target}\" }"
