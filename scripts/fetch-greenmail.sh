#!/bin/bash
# 下载端到端测试用的 GreenMail standalone jar（固定版本 + sha256 校验），放到 tools/。
# jar 有 10 MB，不放进 git；tests/*.rs 与 scripts/check.sh 需要时调用本脚本。
set -euo pipefail
cd "$(dirname "$0")/.."
VERSION=2.1.3
SHA256=9457fdaf45ded6c87bf84a321a5ced4b4b5e72b1d03686b778df381378afc4c8
URL="https://repo1.maven.org/maven2/com/icegreen/greenmail-standalone/$VERSION/greenmail-standalone-$VERSION.jar"
DEST=tools/greenmail-standalone.jar

if [ -f "$DEST" ] && [ "$(shasum -a 256 "$DEST" | cut -d' ' -f1)" = "$SHA256" ]; then
  echo "GreenMail $VERSION 已就绪：$DEST"
  exit 0
fi
mkdir -p tools
tmp=$(mktemp "tools/.greenmail.XXXXXX")
trap 'rm -f "$tmp"' EXIT
echo "下载 GreenMail $VERSION …"
curl -fSL --retry 3 -o "$tmp" "$URL"
got=$(shasum -a 256 "$tmp" | cut -d' ' -f1)
if [ "$got" != "$SHA256" ]; then
  echo "校验和不匹配：期望 $SHA256，实际 $got" >&2
  exit 1
fi
mv "$tmp" "$DEST"
trap - EXIT
echo "已保存到 $DEST"
