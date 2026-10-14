#!/bin/bash
# 协议端到端测试：隔离目录起测试桥（8900），跑 test/protocol-e2e.mjs
set -e
cd "$(dirname "$0")/.."
source "$HOME/.cargo/env"
export CARGO_TARGET_DIR=/tmp/hc-target
cargo build 2>&1 | grep -E "^error" -A5 && exit 1 || true
export HUASHU_CHROME_HOME=/tmp/hc-test
rm -rf /tmp/hc-test && mkdir -p /tmp/hc-test
/tmp/hc-target/debug/linai-chrome-rs bridge --port 8900 > /tmp/hc-test/bridge.out 2>&1 &
BRIDGE_PID=$!
trap "kill $BRIDGE_PID 2>/dev/null || true" EXIT
sleep 2
NODE_PATH=/Users/samon/.npm/_npx/459c632c3c993076/node_modules WS_PATH=/Users/samon/.npm/_npx/459c632c3c993076/node_modules/ws node test/protocol-e2e.mjs
echo "--- 审计日志（最后 6 条）---"
tail -6 /tmp/hc-test/audit.jsonl
