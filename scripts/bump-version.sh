#!/bin/sh
# 抬版本 + 冻结 CHANGELOG（对标 dsh 的 `scripts/release/bump.ts`）。
#
# 为什么要有这个脚本：版本号是**写在仓库里**的（`Cargo.toml` + `CHANGELOG.md` + `Cargo.lock`），
# 而不是 CI 里算出来的。这样"发出去的版本"永远能在仓库里读到，标签只是给它一个名字。
#
# 用法：
#   scripts/bump-version.sh minor          # 0.6.0 -> 0.7.0（破坏性 / 只加功能）
#   scripts/bump-version.sh patch          # 0.6.0 -> 0.6.1（只修 bug）
#   scripts/bump-version.sh 0.7.0-rc.1     # 显式版本（预发布）
#   scripts/bump-version.sh minor --dry-run
#
# 它**不**提交、**不**打标签、**不**推送 —— 出问题时要能一眼看出改了什么。
# 打完版本后由人执行：
#   git commit -am "release(mc): x.y.z"
#   git tag mc-vx.y.z        # 轻量标签，与 Cargo.toml 的版本必须一致
#
# 版本规则见 docs/dev/versioning.md；一致性由 tests/version_sync.rs 守。

set -eu

usage() {
    echo "用法: $0 <minor|patch|x.y.z[-预发布]> [--dry-run]" >&2
    exit 2
}

[ $# -ge 1 ] || usage
SPEC=$1
DRY=0
[ "${2:-}" = "--dry-run" ] && DRY=1

ROOT=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
cd "$ROOT"

[ -f Cargo.toml ] && [ -f CHANGELOG.md ] || { echo "不在仓库根目录：$ROOT" >&2; exit 1; }

CUR=$(sed -n 's/^version *= *"\(.*\)"/\1/p' Cargo.toml | head -n 1)
[ -n "$CUR" ] || { echo "读不出 Cargo.toml 的 version" >&2; exit 1; }

# 只取核心三段（预发布段不参与算术）
CORE=$(printf '%s' "$CUR" | sed 's/-.*//')
MAJOR=$(printf '%s' "$CORE" | cut -d. -f1)
MINOR=$(printf '%s' "$CORE" | cut -d. -f2)
PATCH=$(printf '%s' "$CORE" | cut -d. -f3)

case "$SPEC" in
    major*) NEW="$((MAJOR + 1)).0.0" ;;
    minor*) NEW="$MAJOR.$((MINOR + 1)).0" ;;
    patch*) NEW="$MAJOR.$MINOR.$((PATCH + 1))" ;;
    *)
        NEW=$SPEC
        # 形状校验：x.y.z 或 x.y.z-预发布
        printf '%s' "$NEW" | grep -Eq '^[0-9]+\.[0-9]+\.[0-9]+(-[0-9A-Za-z.-]+)?$' \
            || { echo "不是合法版本号：$NEW" >&2; exit 1; }
        ;;
esac

TODAY=$(date +%Y-%m-%d)
echo "版本: $CUR -> $NEW （$TODAY）"

if [ "$DRY" = "1" ]; then
    echo "[dry-run] 将改写 Cargo.toml、CHANGELOG.md，并刷新 Cargo.lock"
    exit 0
fi

# 1) Cargo.toml
sed -i.bak "s/^version *= *\"$CUR\"/version = \"$NEW\"/" Cargo.toml && rm -f Cargo.toml.bak

# 2) CHANGELOG：把 [Unreleased] 收成本版本，并在最上面补一个新的 [Unreleased]
if grep -q '^## \[Unreleased\]' CHANGELOG.md; then
    # 用临时文件，避免 sed -i 的跨平台差异
    awk -v new="$NEW" -v today="$TODAY" '
        BEGIN { done = 0 }
        /^## \[Unreleased\]/ && !done {
            print "## [Unreleased]";
            print "";
            print "（下一次发布前的新增改动记在这里；发布时整体改标题为 `[x.y.z] - YYYY-MM-DD`，内容不洗。）";
            print "";
            print "## [" new "] - " today;
            done = 1;
            next;
        }
        { print }
    ' CHANGELOG.md > CHANGELOG.md.tmp && mv CHANGELOG.md.tmp CHANGELOG.md
else
    echo "注意：CHANGELOG.md 里没有 [Unreleased] 节，只改了版本号" >&2
fi

# 3) 刷新 Cargo.lock 里本包那一行
cargo build --quiet || { echo "cargo build 失败 —— 版本改了但锁文件没刷新" >&2; exit 1; }

echo
echo "下一步（人来做）："
echo "  git diff Cargo.toml Cargo.lock CHANGELOG.md"
echo "  cargo test --test version_sync --locked"
echo "  git commit -am \"release(mc): $NEW\""
echo "  git tag mc-v$NEW && git push origin main mc-v$NEW"
