#!/bin/bash
# EasyMail · 迷你邮件 一键打包：检查 → release 编译（arm64 + x86_64 通用二进制）→ 组装 .app（ad-hoc 签名）→ DMG。
#
# 用法：scripts/build.sh [选项]
#   --fast        跳过 scripts/check.sh，只编译打包
#   --native      只编译本机架构（更快，不做通用二进制）
#   --dev         正式包里保留开发功能（EASYMAIL_SCRIPT 脚本测试）
#   --no-dmg      只生成 dist/迷你邮件.app
#   --debug       开发构建：debug 编译、本机架构、跳过检查、不打 DMG、保留开发功能（scripts/restart.sh 用）
#   --run         打包后启动（已在运行的实例先退出）
#   -h, --help    显示本帮助
#
# 产物：dist/迷你邮件.app、dist/EasyMail-<版本>.dmg（通用二进制，同时支持 Apple Silicon 与 Intel）
set -euo pipefail
cd "$(dirname "$0")/.."

FAST=0 NATIVE=0 DEV=0 DMG=1 RUN=0 DEBUG=0
for arg in "$@"; do
  case "$arg" in
    --fast) FAST=1 ;;
    --native) NATIVE=1 ;;
    --dev) DEV=1 ;;
    --no-dmg) DMG=0 ;;
    --debug) DEBUG=1 FAST=1 NATIVE=1 DEV=1 DMG=0 ;;
    --run) RUN=1 ;;
    -h | --help) sed -n '2,13p' "$0" | sed 's/^# \{0,1\}//'; exit 0 ;;
    *) echo "未知选项：${arg}（--help 查看用法）" >&2; exit 2 ;;
  esac
done

step() { printf '\n\033[1;34m▸ %s\033[0m\n' "$*"; }
fail() { printf '\033[1;31m✘ %s\033[0m\n' "$*" >&2; exit 1; }
START=$(date +%s)

step "检查构建环境"
[ "$(uname -s)" = "Darwin" ] || fail "只支持 macOS"
command -v cargo >/dev/null || fail "没有找到 cargo"
xcode-select -p >/dev/null 2>&1 || fail "没有 Xcode 命令行工具：xcode-select --install"

# 通用二进制需要同一个工具链同时带 aarch64 / x86_64 两套标准库：
# 优先当前 cargo，其次 rustup 的工具链（Homebrew 的 rust 只有本机架构）
CARGO=cargo
UNI_RUSTC=""  # 通用二进制专用的 rustc；只在 release 编译时导出，别污染 check.sh（clippy-driver 版本须与 RUSTC 一致）
# 判定：两个目标的 libcore 都在，且 x86_64 的 libcore 是当前 rustc 编出来的（Homebrew 偶有残留的旧版 x86_64 目录）
has_targets() {
  local sr; sr=$("$1" --print sysroot 2>/dev/null) || return 1
  ls "$sr"/lib/rustlib/aarch64-apple-darwin/lib/libcore-*.rlib >/dev/null 2>&1 || return 1
  ls "$sr"/lib/rustlib/x86_64-apple-darwin/lib/libcore-*.rlib >/dev/null 2>&1 || return 1
  local d; d=$(mktemp -d); echo 'fn main(){}' | "$1" --target x86_64-apple-darwin --emit=metadata --out-dir "$d" - >/dev/null 2>&1; local r=$?; rm -rf "$d"; return $r
}
if [ "$NATIVE" = 0 ]; then
  if ! has_targets rustc; then
    CARGO=""
    for tc in "$HOME"/.rustup/toolchains/stable-* "$HOME"/.rustup/toolchains/*; do
      [ -x "$tc/bin/cargo" ] && has_targets "$tc/bin/rustc" && { CARGO="$tc/bin/cargo"; UNI_RUSTC="$tc/bin/rustc"; break; }
    done
    [ -n "$CARGO" ] || fail "找不到同时带 aarch64 / x86_64 标准库的工具链：rustup target add x86_64-apple-darwin aarch64-apple-darwin（或加 --native）"
  fi
fi
printf '  macOS %s · %s · cargo: %s\n' "$(sw_vers -productVersion)" "$(uname -m)" "$CARGO"

VERSION=$(grep -m1 '^version' Cargo.toml | cut -d'"' -f2)
NAME="迷你邮件"
APP="dist/${NAME}.app"
FEATURES=(--no-default-features)
[ "$DEV" = 1 ] && FEATURES=()

if [ "$FAST" = 0 ]; then
  step "质量检查（scripts/check.sh）"
  scripts/check.sh
fi

mkdir -p dist
if [ "$DEBUG" = 1 ]; then
  step "编译 debug ${VERSION}（本机架构，增量）"
  cargo build ${FEATURES[@]+"${FEATURES[@]}"}
  BIN=target/debug/easymail
  CLI=target/debug/easymail-cli
elif [ "$NATIVE" = 1 ]; then
  step "编译 release ${VERSION}（本机架构）"
  cargo build --release ${FEATURES[@]+"${FEATURES[@]}"}
  BIN=target/release/easymail
  CLI=target/release/easymail-cli
else
  export CARGO_TARGET_DIR=target/universal
  if [ -n "$UNI_RUSTC" ]; then export RUSTC="$UNI_RUSTC"; fi
  for t in aarch64-apple-darwin x86_64-apple-darwin; do
    step "编译 release ${VERSION} · ${t}"
    # strip 交给系统 strip（有的 rustup 工具链自带 rust-objcopy 缺 libLLVM）
    CARGO_PROFILE_RELEASE_STRIP=false MACOSX_DEPLOYMENT_TARGET=12.0 "$CARGO" build --release --target "$t" ${FEATURES[@]+"${FEATURES[@]}"}
  done
  step "合并通用二进制（lipo）"
  BIN=target/universal/easymail-universal
  lipo -create -output "$BIN" target/universal/aarch64-apple-darwin/release/easymail target/universal/x86_64-apple-darwin/release/easymail
  strip -x "$BIN"
  lipo -info "$BIN" | sed 's/^/  /'
  CLI=target/universal/easymail-cli-universal
  lipo -create -output "$CLI" target/universal/aarch64-apple-darwin/release/easymail-cli target/universal/x86_64-apple-darwin/release/easymail-cli
  strip -x "$CLI"
fi

step "组装 ${APP}"
rm -rf "$APP"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"
cp "$BIN" "$APP/Contents/MacOS/EasyMail"
# 命令行 + 只读 MCP 服务器（见 README「命令行与 MCP」）
cp "$CLI" "$APP/Contents/MacOS/easymail-cli"
cp assets/AppIcon.icns "$APP/Contents/Resources/AppIcon.icns"
cat >"$APP/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleName</key><string>${NAME}</string>
  <key>CFBundleDisplayName</key><string>${NAME}</string>
  <key>CFBundleIdentifier</key><string>app.easymail</string>
  <key>CFBundleExecutable</key><string>EasyMail</string>
  <key>CFBundleIconFile</key><string>AppIcon</string>
  <key>CFBundlePackageType</key><string>APPL</string>
  <key>CFBundleShortVersionString</key><string>${VERSION}</string>
  <key>CFBundleVersion</key><string>${VERSION}</string>
  <key>CFBundleDevelopmentRegion</key><string>zh_CN</string>
  <key>LSMinimumSystemVersion</key><string>12.0</string>
  <key>LSApplicationCategoryType</key><string>public.app-category.productivity</string>
  <key>NSHighResolutionCapable</key><true/>
  <key>NSSupportsAutomaticGraphicsSwitching</key><true/>
  <key>NSHumanReadableCopyright</key><string>EasyMail · 迷你邮件 —— 本地优先的多账号邮件客户端</string>
</dict>
</plist>
PLIST

step "ad-hoc 签名"
codesign --force --deep --sign - "$APP" >/dev/null 2>&1
codesign --verify "$APP" || fail "签名校验失败"

if [ "$DMG" = 1 ]; then
  step "制作 DMG"
  DMG_PATH="dist/EasyMail-${VERSION}.dmg"
  STAGE=$(mktemp -d)
  trap 'rm -rf "$STAGE"' EXIT
  cp -R "$APP" "$STAGE/"
  ln -s /Applications "$STAGE/Applications"
  rm -f "$DMG_PATH"
  hdiutil create -volname "${NAME}" -srcfolder "$STAGE" -ov -format UDZO "$DMG_PATH" >/dev/null
fi

step "完成（用时 $(($(date +%s) - START)) 秒）"
du -sh "$APP" | sed 's/^/  /'
[ "$DMG" = 1 ] && du -sh "dist/EasyMail-${VERSION}.dmg" | sed 's/^/  /'

if [ "$RUN" = 1 ]; then
  if pgrep -x EasyMail >/dev/null; then
    echo "  迷你邮件正在运行，先退出旧进程"
    osascript -e 'quit app id "app.easymail"' >/dev/null 2>&1 || true
    for _ in 1 2 3 4 5 6 7 8 9 10; do pgrep -x EasyMail >/dev/null || break; sleep 0.5; done
    pkill -x EasyMail 2>/dev/null || true
  fi
  open "$APP"
  echo "  已启动 $APP"
fi
