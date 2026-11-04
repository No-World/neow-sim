#!/usr/bin/env bash
# 通过本地代理（7897）推送，带重试。
# 用法: ./push.sh [max_retries]
set -uo pipefail
ROOT="$(cd "$(dirname "$0")" && pwd)"
cd "$ROOT" || exit 1
GIT="/mnt/d/Program Files/Git/cmd/git.exe"
MAX="${1:-12}"
PROXY="http://127.0.0.1:7897"

for i in $(seq 1 "$MAX"); do
    echo "=== attempt $i/$MAX ==="
    out=$("$GIT" -c http.version=HTTP/1.1 \
                -c http.proxy="$PROXY" -c https.proxy="$PROXY" \
                -c http.postBuffer=524288000 \
                push origin master 2>&1)
    rc=$?
    echo "$out" | tail -3
    if [ $rc -eq 0 ] || echo "$out" | grep -qi "up-to-date"; then
        echo "PUSH OK"
        exit 0
    fi
    sleep $((i * 5))
done

echo "PUSH FAILED after $MAX attempts"
exit 1
