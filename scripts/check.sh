#!/bin/bash
# 质量检查：格式 → clippy（零警告）→ 单元测试 → GreenMail 核心端到端 → 界面端到端（真实窗口）。
#
# 用法：scripts/check.sh [--no-ui]
#   --no-ui   跳过需要图形会话的界面端到端测试（ssh / CI 无桌面时用）
#
# GreenMail（Java，非 docker）没在跑时由测试自动用 tools/greenmail-standalone.jar 拉起；
# jar 不在仓库里，缺了就先用 scripts/fetch-greenmail.sh 下载（固定版本 + 校验和）。
set -euo pipefail
cd "$(dirname "$0")/.."
UI=1
for a in "$@"; do
  case "$a" in
    --no-ui) UI=0 ;;
    -h | --help) sed -n '2,7p' "$0" | sed 's/^# \{0,1\}//'; exit 0 ;;
    *) echo "未知选项：$a" >&2; exit 2 ;;
  esac
done
step() { printf '\n\033[1;34m▸ %s\033[0m\n' "$*"; }
fail() { printf '\033[1;31m✘ %s\033[0m\n' "$*" >&2; exit 1; }

step "格式（cargo fmt --check）"
cargo fmt --check || fail "格式不一致：先执行 cargo fmt"

step "静态检查（cargo clippy -D warnings）"
cargo clippy --all-targets --quiet -- -D warnings || fail "clippy 未通过"

step "单元测试"
cargo test --quiet --lib --bins || fail "单元测试未通过"

[ -f tools/greenmail-standalone.jar ] || scripts/fetch-greenmail.sh || fail "下载 GreenMail 失败"

# 先把输出存下来、单独取 cargo test 的退出码，再做过滤。
# 不能写成 `cargo test | grep … || true` 再读 PIPESTATUS：一旦走到 `|| true`，PIPESTATUS 就成了 true 的状态。
run_test() {
  local out code
  set +e
  out=$(cargo test --quiet "$@" 2>&1)
  code=$?
  set -e
  printf '%s\n' "$out" | grep -E 'CHECK|RESULT|test result|panicked|FAILED|error' || true
  return "$code"
}

step "GreenMail 核心端到端（同步 / 标记 / 移动 / 删除 / 发信 / IDLE / 导入导出）"
EASYMAIL_REQUIRE_GREENMAIL=1 run_test --test greenmail -- --nocapture || fail "GreenMail 端到端未通过"

if [ "$UI" = 1 ]; then
  step "界面端到端（真实 GPUI 窗口 + WKWebView，截图在 target/tmp/ui-shots）"
  EASYMAIL_REQUIRE_GREENMAIL=1 run_test --test ui_e2e -- --nocapture || fail "界面端到端未通过"
fi
printf '\n\033[1;32m✔ 全部检查通过\033[0m\n'
