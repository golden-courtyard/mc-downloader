#!/usr/bin/env python3
"""Markdown 链接检查：断链 ⇒ 失败；指向仓库外 ⇒ 失败（视为路径泄露）。

只管**相对链接**：`http(s)`、`mailto:`、纯锚点一律跳过（那些由别的办法管）。

为什么把"仓库外"也算错：本仓库是公开仓，文档里出现 `../../某私有目录` 或绝对路径，
等于把本机目录结构写进公开历史。判据不是"链接能不能打开"，而是"它有没有指向仓里"。

用法：
    python scripts/check-md-links.py                 # 检查所有受控 .md
    python scripts/check-md-links.py a.md docs/b.md  # 只查指定的

退出码：0 = 全部通过；1 = 有断链或越界。
"""

import pathlib
import re
import subprocess
import sys
import urllib.parse

ROOT = pathlib.Path(__file__).resolve().parents[1]
LINK = re.compile(r"\]\(([^)]+)\)")
FENCE = re.compile(r"^\s*(```|~~~)")


def tracked_markdown() -> list[pathlib.Path]:
    out = subprocess.run(
        ["git", "ls-files", "*.md"], cwd=ROOT, capture_output=True, text=True, check=True
    ).stdout
    return [ROOT / p for p in out.splitlines() if p.strip()]


def check(path: pathlib.Path) -> list[str]:
    problems: list[str] = []
    in_fence = False
    try:
        text = path.read_text(encoding="utf-8")
    except UnicodeDecodeError:
        return [f"{path.relative_to(ROOT)}: 不是 UTF-8，跳过"]

    for lineno, line in enumerate(text.splitlines(), start=1):
        if FENCE.match(line):
            in_fence = not in_fence
            continue
        if in_fence:
            continue
        # 行内代码（`...`）里的链接是"文档里写的例子"，不是真链接
        line = re.sub(r"`[^`]*`", "", line)
        for m in LINK.finditer(line):
            target = m.group(1).strip().split(" ")[0]
            if not target or target.startswith(("http://", "https://", "mailto:", "#")):
                continue
            raw = urllib.parse.unquote(target.split("#")[0])
            if not raw:
                continue
            resolved = (path.parent / raw).resolve()
            where = f"{path.relative_to(ROOT).as_posix()}:{lineno}: {target}"
            try:
                resolved.relative_to(ROOT)
            except ValueError:
                problems.append(f"{where}  ← 指向仓库外（路径泄露）")
                continue
            if not resolved.exists():
                problems.append(f"{where}  ← 断链")
    return problems


def main(argv: list[str]) -> int:
    files = [pathlib.Path(a).resolve() for a in argv[1:]] or tracked_markdown()
    problems: list[str] = []
    for f in files:
        if f.suffix.lower() != ".md" or not f.exists():
            continue
        problems.extend(check(f))
    if problems:
        print(f"[md-links] {len(problems)} 处问题：")
        for p in problems:
            print("  " + p)
        return 1
    print(f"[md-links] OK（{len(files)} 个文件）")
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
