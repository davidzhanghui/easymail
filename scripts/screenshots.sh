#!/bin/bash
# 生成 README / 落地页用的截图：以演示用户拉起 GreenMail → 投放演示邮件（examples/seed_demo.rs）
# → 用脚本场景 EASYMAIL_SCRIPT=shots 启动真实窗口截图 → 缩放后写入 docs/screenshots/。
#
# 用法：scripts/screenshots.sh        需要图形会话与 Java 11+；不会碰你自己的数据目录
#   SHOTS_RAW=<目录>  另存一份未缩放的 Retina 原图（宣传片素材用）
#   SCENE=promo       改跑宣传片素材场景（新邮件到达、逐字搜索、十套配色），只输出到 SHOTS_RAW
set -euo pipefail
cd "$(dirname "$0")/.."
step() { printf '\n\033[1;34m▸ %s\033[0m\n' "$*"; }

[ -f tools/greenmail-standalone.jar ] || scripts/fetch-greenmail.sh
WORK=$(mktemp -d "${TMPDIR:-/tmp}/easymail-shots.XXXXXX")
GM_PID=""
cleanup() { [ -n "$GM_PID" ] && kill "$GM_PID" 2>/dev/null; rm -rf "$WORK"; }
trap cleanup EXIT

step "拉起 GreenMail（演示用户）"
# 端口上已有的 GreenMail（比如测试留下的）没有演示用户，先停掉
pids=$(lsof -ti tcp:3143 2>/dev/null || true)
[ -n "$pids" ] && { kill $pids; sleep 1; }
java -Dgreenmail.setup.test.all -Dgreenmail.users.login=email \
  -Dgreenmail.users=linxia:pw@example.com,lin:pw@studio.example.com \
  -jar tools/greenmail-standalone.jar >/dev/null 2>&1 &
GM_PID=$!
for _ in $(seq 1 100); do nc -z 127.0.0.1 3143 2>/dev/null && break; sleep 0.3; done

step "投放演示邮件并同步"
cargo run --quiet --example seed_demo -- "$WORK/data"

step "启动真实窗口截图"
cargo build --quiet
SCENE=${SCENE:-shots}
EASYMAIL_DATA_DIR="$WORK/data" EASYMAIL_SCRIPT="$SCENE" EASYMAIL_FORCE_DRAW=1 EASYMAIL_NO_NOTIFY=1 \
  EASYMAIL_IMPORT_FROM="$WORK/none" SHOT_DIR="$WORK/shots" target/debug/easymail | grep -E "SHOT|RESULT|PROMO|CHECK"

if [ "$SCENE" = shots ]; then
  step "缩放写入 docs/screenshots/"
  rm -rf docs/screenshots && mkdir -p docs/screenshots
  for f in "$WORK"/shots/[0-9]*.png; do  # x-* 是回归检查图，不进文档
    sips -Z 2000 "$f" --out "docs/screenshots/$(basename "$f")" >/dev/null
  done
fi
if [ -n "${SHOTS_RAW:-}" ]; then mkdir -p "$SHOTS_RAW" && cp "$WORK"/shots/*.png "$SHOTS_RAW"/; echo "原图：$SHOTS_RAW"; fi
