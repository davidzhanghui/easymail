#!/bin/bash
# 重启迷你邮件（开发用）：退出在跑的实例 →（默认）debug 增量构建 → 启动开发副本 dist/迷你邮件.app
#
# 用法：scripts/restart.sh [选项]
#   -n, --no-build   不构建，只重启（没改代码时用）
#   -f, --fg         前台运行，日志直接打在终端（Ctrl+C 退出）
#   -h, --help       显示本帮助
#
# 环境变量照常生效，例如 EASYMAIL_DATA_DIR=/tmp/em-demo scripts/restart.sh 用独立数据目录试跑。
set -euo pipefail
cd "$(dirname "$0")/.."

APP="dist/迷你邮件.app"
BIN="$APP/Contents/MacOS/EasyMail"
BUILD=1 FG=0
for arg in "$@"; do
  case "$arg" in
    -n | --no-build) BUILD=0 ;;
    -f | --fg | --foreground) FG=1 ;;
    -h | --help) sed -n '2,9p' "$0" | sed 's/^# \{0,1\}//'; exit 0 ;;
    *) echo "未知选项：${arg}（--help 查看用法）" >&2; exit 2 ;;
  esac
done

# 1) 退出旧实例：打包版（进程名 EasyMail）与 cargo run 起的 target/*/easymail 都算，避免两个实例抢同一个数据库
running() { pgrep -x EasyMail || pgrep -f "/target/[^ ]*/easymail( |$)" || true; }
if [ -n "$(running)" ]; then
  echo "▸ 退出正在运行的迷你邮件…"
  osascript -e 'quit app id "app.easymail"' >/dev/null 2>&1 || true
  for _ in $(seq 1 30); do [ -z "$(running)" ] && break; sleep 0.1; done
  if [ -n "$(running)" ]; then
    pkill -x EasyMail 2>/dev/null || true
    pkill -f "/target/[^ ]*/easymail( |$)" 2>/dev/null || true
    sleep 0.5
    [ -n "$(running)" ] && { running | xargs kill -9 2>/dev/null || true; sleep 0.3; }
  fi
fi

# 2) 构建
[ "$BUILD" = 1 ] && scripts/build.sh --debug
[ -x "$BIN" ] || { echo "找不到 ${BIN}，先执行 scripts/build.sh --debug" >&2; exit 1; }

# 3) 启动
if [ "$FG" = 1 ]; then
  echo "▸ 前台运行（Ctrl+C 退出）…"
  exec "$BIN"
fi
open "$APP"
for _ in $(seq 1 50); do
  pid=$(pgrep -x EasyMail || true)
  [ -n "$pid" ] && { echo "✔ 迷你邮件已启动（pid ${pid}）"; exit 0; }
  sleep 0.1
done
echo "启动后没等到进程，改用 scripts/restart.sh --fg 看日志" >&2
exit 1
