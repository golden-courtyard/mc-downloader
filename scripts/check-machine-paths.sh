#!/usr/bin/env bash
# 公开仓不许出现本机绝对路径。CI 与 .githooks/pre-commit **共用这一份**，
# 免得两处模式各写一遍、各自踩各自的坑（2026-11-25 前后红了两次，都是这么来的）。
#
# 用法：
#   bash scripts/check-machine-paths.sh            # 查所有受控文件
#   bash scripts/check-machine-paths.sh a.md b.rs  # 只查给定的
#
# 退出码：0 = 干净；1 = 有命中（打印 文件:行:内容）。
#
# 注意：本文件自身必须排除，否则模式串会命中它自己。

set -eu

SELF='scripts/check-machine-paths.sh'
PATTERN='[A-Za-z]:[\\/]Users[\\/][^<]|/Users/[A-Za-z][A-Za-z0-9._-]*/|C:[\\/]hub|lmq-mcd-target|Academic/(research|tools)'

if [ "$#" -gt 0 ]; then
    FILES="$*"
else
    FILES=$(git ls-files)
fi

HITS=$(git grep -nE "$PATTERN" -- $FILES ":!$SELF" 2>/dev/null || true)

if [ -n "$HITS" ]; then
    echo "发现本机路径（公开仓不许）："
    echo "$HITS"
    exit 1
fi

echo "no machine-specific paths OK"
