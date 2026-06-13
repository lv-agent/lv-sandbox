#!/usr/bin/env bash
#
# 容器内端到端验证：启动镜像 → health → profiles → submit echo 任务断言 Completed
#
# 用法:
#   bash scripts/verify-image.sh [IMAGE]
#   IMAGE 默认 lv-sandbox:0.1.0
#
# 前提: 当前用户在 docker 组（重新登录生效，或用 `sg docker -c "bash scripts/verify-image.sh"`）
# /sandboxes 用 tmpfs（验证用，无需 host 目录/chown）；生产部署用 host 卷，见 docs/usage.md
set -euo pipefail

IMAGE="${1:-${IMAGE:-lv-sandbox:0.1.0}}"
HOST_PORT="${HOST_PORT:-18080}"
NAME="sandbox-verify-$$"

if ! docker ps >/dev/null 2>&1; then
    echo "✗ docker 不可用。请确保当前用户在 docker 组（重新登录生效），" >&2
    echo "  或用: sg docker -c \"bash scripts/verify-image.sh\"" >&2
    exit 1
fi

cleanup() { docker rm -f "$NAME" >/dev/null 2>&1 || true; }
trap cleanup EXIT

echo "==> 启动 $IMAGE（127.0.0.1:$HOST_PORT）"
docker run -d --name "$NAME" -p "$HOST_PORT:8080" \
    --read-only --tmpfs /tmp:rw,nosuid,nodev,size=1g \
    --tmpfs /sandboxes:rw,nosuid,nodev,size=100m,uid=10000,gid=10000 \
    --cap-drop=ALL --security-opt no-new-privileges \
    --pids-limit=1000 --memory=4g --cpus=4 \
    --user 10000:10000 \
    "$IMAGE" >/dev/null

echo "==> 等待 server 就绪"
ok=0
for i in $(seq 1 15); do
    if curl -sf "http://127.0.0.1:$HOST_PORT/health" >/dev/null 2>&1; then ok=1; break; fi
    sleep 1
done
if [ "$ok" != 1 ]; then
    echo "✗ server 未就绪"
    docker logs "$NAME" 2>&1 | tail -20
    exit 1
fi

echo "==> /health"
curl -sf "http://127.0.0.1:$HOST_PORT/health" >/dev/null && echo "   ok"

echo "==> /api/v1/profiles"
prof=$(curl -s "http://127.0.0.1:$HOST_PORT/api/v1/profiles")
echo "   $prof"
echo "$prof" | grep -q '"shell"' || { echo "✗ 缺 shell profile"; exit 1; }

echo "==> /api/v1/submit (echo)"
resp=$(curl -s -X POST "http://127.0.0.1:$HOST_PORT/api/v1/submit" \
    -H 'content-type: application/json' \
    -d '{"job_id":"verify-1","argv":["/bin/echo","hello sandbox"],"profile_name":"shell","timeout":"5s","custom_env":{}}')
echo "   $resp"
echo "$resp" | grep -q '"status":"Completed"' || { echo "✗ 任务未 Completed"; exit 1; }
echo "$resp" | grep -q '"exit_code":0'       || { echo "✗ exit_code 非 0"; exit 1; }
echo "$resp" | grep -q 'hello sandbox'       || { echo "✗ stdout 不含预期输出"; exit 1; }

echo ""
echo "✅ 容器内端到端验证通过：$IMAGE"
