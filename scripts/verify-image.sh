#!/usr/bin/env bash
#
# 容器内端到端验证：启动镜像 → health → profiles → submit echo/python/node 任务断言
#
# 用法:
#   bash scripts/verify-image.sh [IMAGE]
#   IMAGE 默认 lv-sandbox:0.3.0
#
# 前提: 当前用户在 docker 组（重新登录生效，或用 `sg docker -c "bash scripts/verify-image.sh"`）
# /sandboxes 用 tmpfs（验证用，无需 host 目录/chown）；生产部署用 host 卷，见 docs/usage.md
set -euo pipefail

IMAGE="${1:-${IMAGE:-lv-sandbox:0.3.0}}"
HOST_PORT="${HOST_PORT:-18080}"
NAME="sandbox-verify-$$"

if ! docker ps >/dev/null 2>&1; then
    echo "✗ docker unavailable. Ensure the current user is in the docker group (re-login)," >&2
    echo "  or run: sg docker -c \"bash scripts/verify-image.sh\"" >&2
    exit 1
fi

cleanup() { docker rm -f "$NAME" >/dev/null 2>&1 || true; }
trap cleanup EXIT

echo "==> starting $IMAGE (127.0.0.1:$HOST_PORT)"
docker run -d --name "$NAME" -p "$HOST_PORT:8080" \
    --read-only --tmpfs /tmp:rw,nosuid,nodev,size=1g \
    --tmpfs /sandboxes:rw,nosuid,nodev,size=100m,uid=10000,gid=10000 \
    --cap-drop=ALL --security-opt no-new-privileges \
    --pids-limit=1000 --memory=4g --cpus=4 \
    --user 10000:10000 \
    "$IMAGE" >/dev/null

echo "==> waiting for server to be ready"
ok=0
for i in $(seq 1 15); do
    if curl -sf "http://127.0.0.1:$HOST_PORT/health" >/dev/null 2>&1; then ok=1; break; fi
    sleep 1
done
if [ "$ok" != 1 ]; then
    echo "✗ server not ready"
    docker logs "$NAME" 2>&1 | tail -20
    exit 1
fi

echo "==> /health"
curl -sf "http://127.0.0.1:$HOST_PORT/health" >/dev/null && echo "   ok"

echo "==> /api/v1/profiles"
prof=$(curl -s "http://127.0.0.1:$HOST_PORT/api/v1/profiles")
echo "   $prof"
echo "$prof" | grep -q '"shell"' || { echo "✗ missing shell profile"; exit 1; }

echo "==> /api/v1/jobs (echo, async submit)"
resp=$(curl -s -X POST "http://127.0.0.1:$HOST_PORT/api/v1/jobs" \
    -H 'content-type: application/json' \
    -d '{"job_id":"verify-1","argv":["/bin/echo","hello sandbox"],"profile_name":"shell","timeout":"5s","custom_env":{}}')
echo "   $resp"
echo "$resp" | grep -q '"status":"Running"' || { echo "✗ submit did not return Running"; exit 1; }

echo "==> polling GET /api/v1/jobs/verify-1"
ok=0
for i in $(seq 1 50); do
    resp=$(curl -s "http://127.0.0.1:$HOST_PORT/api/v1/jobs/verify-1")
    if echo "$resp" | grep -q '"status":"Completed"'; then ok=1; break; fi
    sleep 0.1
done
[ "$ok" = 1 ] || { echo "✗ job not Completed: $resp"; exit 1; }
echo "   $resp"
echo "$resp" | grep -q '"exit_code":0'       || { echo "✗ exit_code not 0"; exit 1; }
echo "$resp" | grep -q 'hello sandbox'       || { echo "✗ stdout missing expected output"; exit 1; }

echo "==> /api/v1/jobs (python: import requests/httpx)"
curl -s -X POST "http://127.0.0.1:$HOST_PORT/api/v1/jobs" \
    -H 'content-type: application/json' \
    -d '{"job_id":"verify-py","argv":["/usr/bin/python3","-c","import requests,httpx;print(\"py-ok\")"],"profile_name":"python","timeout":"15s","custom_env":{}}' >/dev/null
for i in $(seq 1 50); do
    resp=$(curl -s "http://127.0.0.1:$HOST_PORT/api/v1/jobs/verify-py")
    echo "$resp" | grep -qE '"status":"(Completed|Error|Killed)"' && break
    sleep 0.2
done
echo "$resp" | grep -q 'py-ok' || { echo "✗ python did not run (import requests/httpx): $resp"; exit 1; }

echo "==> /api/v1/jobs (node)"
curl -s -X POST "http://127.0.0.1:$HOST_PORT/api/v1/jobs" \
    -H 'content-type: application/json' \
    -d '{"job_id":"verify-node","argv":["/usr/bin/node","-e","console.log(\"node-ok\")"],"profile_name":"node","timeout":"15s","custom_env":{}}' >/dev/null
for i in $(seq 1 50); do
    resp=$(curl -s "http://127.0.0.1:$HOST_PORT/api/v1/jobs/verify-node")
    echo "$resp" | grep -qE '"status":"(Completed|Error|Killed)"' && break
    sleep 0.2
done
echo "$resp" | grep -q 'node-ok' || { echo "✗ node did not run: $resp"; exit 1; }

echo ""
echo "✅ in-container end-to-end verification passed: $IMAGE"
