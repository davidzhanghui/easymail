#!/bin/bash
# 创建并推送版本 tag，触发 GitHub Actions 的 Release 工作流（构建通用 DMG 并发布）。
#
# 用法：scripts/push-tag.sh [tag]
#   不带参数时用 Cargo.toml 的版本号，如 v1.0.0
set -euo pipefail
cd "$(dirname "$0")/.."

tag=${1:-v$(grep -m1 '^version' Cargo.toml | cut -d'"' -f2)}
[ -z "$(git status --porcelain)" ] || { echo "工作区有未提交的改动，先提交" >&2; exit 1; }
grep -q "^## \[${tag#v}\]" CHANGELOG.md || echo "提示：CHANGELOG.md 里没有 [${tag#v}] 一节" >&2
if git rev-parse -q --verify "refs/tags/$tag" >/dev/null; then
  echo "tag $tag 已存在；重打先删：git tag -d $tag && git push origin :refs/tags/$tag" >&2
  exit 1
fi
git tag -a "$tag" -m "EasyMail $tag"
git push origin "$tag"
echo "✔ 已推送 $tag → GitHub Actions 开始构建，稍后见 Releases 页面"
