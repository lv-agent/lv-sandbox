#!/usr/bin/env bash
#
# 构建分发产物：Docker 镜像 + glibc 二进制 tar.gz
#
# 用法:
#   bash scripts/build-release.sh [VERSION]
#
# 产物:
#   - 镜像 lv-sandbox:<VERSION> / lv-sandbox:latest
#   - dist/lv-sandbox-<VERSION>-<arch>-gnu.tar.gz
#
# 运行前提: 当前用户在 docker 组（否则 docker 命令需 sudo，或用 `sg docker -c "$0"`）
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
cd "$ROOT_DIR"

# 版本：参数 > Cargo.toml [workspace.package].version > 默认 0.4.0
VERSION="${1:-}"
if [ -z "$VERSION" ]; then
    VERSION="$(awk -F'"' '/^version[[:space:]]*=/{print $2; exit}' Cargo.toml)"
fi
VERSION="${VERSION:-0.4.0}"

IMAGE="${IMAGE:-lv-sandbox}"
ARCH="$(uname -m)"
DIST="dist"
TARBALL="${DIST}/${IMAGE}-${VERSION}-${ARCH}-gnu.tar.gz"

echo "==> building image ${IMAGE}:${VERSION} / ${IMAGE}:latest"
DOCKER_BUILDKIT=1 docker build -t "${IMAGE}:${VERSION}" -t "${IMAGE}:latest" .

echo "==> extracting binaries from image"
rm -rf "${DIST}"
mkdir -p "${DIST}"
docker rm -f extract >/dev/null 2>&1 || true
docker create --name extract "${IMAGE}:${VERSION}" >/dev/null
docker cp extract:/usr/local/bin/sandbox-server "${DIST}/"
docker cp extract:/usr/local/bin/sandbox-mcp  "${DIST}/"
docker rm extract >/dev/null

# 附带示例配置 + 快速说明
cp docker/config.yaml "${DIST}/config.yaml.example"
cat > "${DIST}/README-quickstart.txt" <<EOF
lv-sandbox ${VERSION} (${ARCH}, glibc)

Runtime dependency: libseccomp2
  Debian/Ubuntu: sudo apt install libseccomp2
  RHEL/Fedora:   sudo dnf install libseccomp

Start:
  ./sandbox-server --config config.yaml.example

Requires host Linux kernel >= 5.13 (Landlock).
Full usage: see docs/usage.md.
EOF

echo "==> packing ${TARBALL}"
tar -czf "${TARBALL}" -C "${DIST}" \
    sandbox-server sandbox-mcp config.yaml.example README-quickstart.txt

echo ""
echo "✅ done"
echo "   image: ${IMAGE}:${VERSION}, ${IMAGE}:latest"
echo "   archive: ${TARBALL}"
ls -lh "${TARBALL}"
