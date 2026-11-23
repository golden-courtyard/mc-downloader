#!/usr/bin/env python3
"""决策记录（notes）吞吐 vs 发布节奏：对 deepseek-harness 的定量观察。

## 口径（**先写下来再测**，事后不改判据）

研究对象：任何被指定为 `--repo` 的研究仓库（本脚本不内置路径）；本仓库的用法见 `scripts/quant/README.md`。
只读，不修改被研究对象。

**三个问题**

Q1 每个标签之间，notes 里发生了什么？（新增 / 生命周期迁移 / 归档，按 lifecycle 与 class 计数）
Q2 预发布序列里，`-alpha` 段与 `-rc` 段携带的 note 类型有区别吗？
   （如果没区别，那"rc 只接受修复"就不是 dsh 的做法，而是我们自己的选择 —— 必须说清。）
Q3 一条决策记录从 `proposed` 走到 `implemented` 要多久？与标签节奏有没有关系？

**指标**

- `commits`：区间内提交数
- `notes_total`：区间内 `.agents/notes/` 下发生变更（A/M/D）的 md 数
- `added_<lifecycle>` / `removed_<lifecycle>`：按路径第 3 段判定（proposed / implemented / rejected / archived）
- `transitions`：同一文件名在同一区间内既有 `proposed/...` 删除、又有 `implemented/...` 新增 ⇒ 记一次迁移
- `impl_per_100_commits`：密度（0 提交时记 None）
- `bump_kind`：相对上一个标签的变化类型（major / minor / patch / prerelease-step / other）

**已知局限**（写在这里，不藏）

1. dsh 的 44 个标签**全是预发布**（`-rc.N` / `-alpha.N`），没有稳定版 ⇒ "minor 抬版携带多少 implemented"
   这类经典假设**在这份数据上测不了**，只能测"预发布段之间"的差异。
2. 一个提交区间里 notes 的 A/M/D 只反映**文件层面**的改动；一条 note 被就地改写（M）不算新决策。
3. 迁移判定靠**文件名相同**：改名会被记成 删除 + 新增，不记为迁移。
"""

import subprocess
import sys
from collections import Counter, defaultdict
from datetime import date
import csv
import re

# 路径一律由调用方给（--repo 或环境变量 DSH_REPO），仓库里不写任何本机路径。
import argparse
import os
import pathlib

_ap = argparse.ArgumentParser(description="notes 吞吐 vs 发布节奏的定量观察")
_ap.add_argument("--repo", default=os.environ.get("DSH_REPO", ""),
                 help="被研究的仓库路径（也可用环境变量 DSH_REPO）")
_args = _ap.parse_args()
if not _args.repo:
    raise SystemExit("请用 --repo <path> 或环境变量 DSH_REPO 指定被研究的仓库")

REPO = _args.repo
# 产物落在本仓库的 docs/reports/raw/ 下（相对脚本位置推导，不写字面路径）
OUT_CSV = str(pathlib.Path(__file__).resolve().parents[2]
              / "docs" / "reports" / "raw" / "versioning-vs-notes.csv")


def git(*args):
    return subprocess.run(
        ["git", "-C", REPO, *args], capture_output=True, text=True, encoding="utf-8", errors="replace"
    ).stdout


def tags():
    out = git("for-each-ref", "--sort=creatordate", "--format=%(refname:short)|%(objectname)|%(creatordate:short)", "refs/tags/dsh-v*")
    rows = []
    for line in out.splitlines():
        name, sha, day = line.split("|")
        rows.append({"tag": name, "sha": sha, "date": day})
    return rows


def parse_version(tag):
    m = re.match(r"dsh-v(\d+)\.(\d+)\.(\d+)(?:-(.+))?$", tag)
    if not m:
        return None
    return (int(m.group(1)), int(m.group(2)), int(m.group(3)), m.group(4))


def bump_kind(prev_tag, cur_tag):
    a, b = parse_version(prev_tag), parse_version(cur_tag)
    if not a or not b:
        return "other"
    if a[:3] != b[:3]:
        if b[0] != a[0]:
            return "major"
        if b[1] != a[1]:
            return "minor"
        return "patch"
    # 同一核心版本：预发布段变化
    return "prerelease-step"


def note_changes(prev_sha, cur_sha):
    """区间内 notes 的 A/M/D 明细。"""
    out = git("diff", "--name-status", prev_sha, cur_sha, "--", ".agents/notes")
    added, removed, modified = [], [], []
    for line in out.splitlines():
        parts = line.split("\t")
        if len(parts) < 2:
            continue
        status, path = parts[0], parts[-1]
        if status == "A":
            added.append(path)
        elif status == "D":
            removed.append(path)
        elif status.startswith("M") or status.startswith("R"):
            modified.append(path)
    return added, removed, modified


def lifecycle(path):
    parts = path.split("/")
    if len(parts) >= 4 and parts[0] == ".agents" and parts[1] == "notes":
        return parts[2]
    return "other"


def note_class(path):
    parts = path.split("/")
    if len(parts) >= 5:
        return parts[3]
    return "other"


def main():
    rows = tags()
    if not rows:
        print("no dsh-v* tags", file=sys.stderr)
        return 1

    records = []
    for i, cur in enumerate(rows):
        prev_sha = rows[i - 1]["sha"] if i else git("rev-list", "--max-parents=0", "HEAD").split()[0]
        prev_tag = rows[i - 1]["tag"] if i else "(root)"
        commits = int(git("rev-list", "--count", f"{prev_sha}..{cur['sha']}").strip() or 0)
        added, removed, modified = note_changes(prev_sha, cur["sha"])

        add_lc = Counter(lifecycle(p) for p in added)
        rem_lc = Counter(lifecycle(p) for p in removed)
        add_cls = Counter(note_class(p) for p in added if lifecycle(p) == "implemented")

        # 迁移：同一 basename 既有 proposed 删除、又有 implemented 新增
        proposed_names = {p.rsplit("/", 1)[-1] for p in removed if lifecycle(p) == "proposed"}
        impl_names = {p.rsplit("/", 1)[-1] for p in added if lifecycle(p) == "implemented"}
        transitions = len(proposed_names & impl_names)

        records.append(
            {
                "tag": cur["tag"],
                "date": cur["date"],
                "prev_tag": prev_tag,
                "bump_kind": bump_kind(prev_tag, cur["tag"]),
                "commits": commits,
                "notes_added": len(added),
                "notes_removed": len(removed),
                "notes_modified": len(modified),
                "add_proposed": add_lc.get("proposed", 0),
                "add_implemented": add_lc.get("implemented", 0),
                "add_archived": add_lc.get("archived", 0),
                "add_rejected": add_lc.get("rejected", 0),
                "rm_proposed": rem_lc.get("proposed", 0),
                "transitions": transitions,
                "impl_feature": add_cls.get("feature", 0),
                "impl_bugfix": add_cls.get("bug-fix", 0),
                "impl_arch": add_cls.get("architecture", 0),
                "impl_process": add_cls.get("process", 0),
                "impl_simplify": add_cls.get("simplification", 0),
                "impl_per_100_commits": round(add_lc.get("implemented", 0) * 100 / commits, 2) if commits else "",
            }
        )

    import os

    os.makedirs(os.path.dirname(OUT_CSV), exist_ok=True)
    with open(OUT_CSV, "w", newline="", encoding="utf-8") as f:
        w = csv.DictWriter(f, fieldnames=list(records[0].keys()))
        w.writeheader()
        w.writerows(records)
    print(f"wrote {OUT_CSV}  ({len(records)} rows)")

    # ---- 摘要 ----
    def stat(key):
        vals = [r[key] for r in records]
        return min(vals), sum(vals) / len(vals), max(vals)

    print(f"tags: {len(records)}  期间: {records[0]['date']} .. {records[-1]['date']}")
    print(f"commits/tag  min/avg/max = {stat('commits')}")
    print(f"implemented/tag min/avg/max = {stat('add_implemented')}")
    print(f"proposed（新增）/tag min/avg/max = {stat('add_proposed')}")
    print(f"transitions/tag min/avg/max = {stat('transitions')}")

    by_kind = defaultdict(list)
    for r in records:
        by_kind[r["bump_kind"]].append(r)
    print("\n按 bump_kind 分组（均值）：")
    print(f"{'kind':16}{'n':>4}{'commits':>10}{'impl':>8}{'prop+':>8}{'trans':>8}{'impl/100c':>12}")
    for kind, rs in sorted(by_kind.items(), key=lambda kv: -len(kv[1])):
        n = len(rs)
        print(
            f"{kind:16}{n:>4}"
            f"{sum(x['commits'] for x in rs)/n:>10.1f}"
            f"{sum(x['add_implemented'] for x in rs)/n:>8.1f}"
            f"{sum(x['add_proposed'] for x in rs)/n:>8.1f}"
            f"{sum(x['transitions'] for x in rs)/n:>8.1f}"
            f"{sum(x['impl_per_100_commits'] for x in rs if x['impl_per_100_commits'] != ''):>12.1f}"
        )

    # alpha 段 vs rc 段：看 note 类型构成
    def segment(tag):
        if "-alpha." in tag:
            return "alpha"
        if "-rc." in tag:
            return "rc"
        return "stable"

    seg = defaultdict(list)
    for r in records:
        seg[segment(r["tag"])].append(r)
    print("\nalpha vs rc（每标签均值）：")
    for k, rs in seg.items():
        n = len(rs)
        print(
            f"  {k:7} n={n:<3} commits={sum(x['commits'] for x in rs)/n:>6.1f}"
            f" impl={sum(x['add_implemented'] for x in rs)/n:>5.1f}"
            f" feature={sum(x['impl_feature'] for x in rs)/n:>5.1f}"
            f" bugfix={sum(x['impl_bugfix'] for x in rs)/n:>5.1f}"
            f" arch={sum(x['impl_arch'] for x in rs)/n:>5.1f}"
            f" process={sum(x['impl_process'] for x in rs)/n:>5.1f}"
        )
    return 0


if __name__ == "__main__":
    sys.exit(main())
