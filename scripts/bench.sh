#!/usr/bin/env bash
# 大库性能基准：scripts/bench.sh [邮件封数，默认 100000]
set -euo pipefail
cd "$(dirname "$0")/.."
cargo run --release --quiet --example bench_library -- "${1:-100000}"
