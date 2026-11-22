# Agent Note: fetch 的落地名从 URL 推导，已存在默认拒绝

Status: implemented

> 对应 issue [#1](https://github.com/golden-courtyard/mc-downloader/issues/1)

## Problem

`fetch` 的 `-n/--name` 带 `default_value = "out.bin"`，于是：

1. **同目录连下两个文件会互相覆盖**。实测（2026-11-01）：先下 73 MB 的 `_text.pdf`，
   再下 2 MB 的 `_djvu.txt`，第二次把第一次盖掉 —— 而且**不报错**。
2. **两次的进度输出都只写 `out.bin`**，从日志里看不出这次下的是什么。

更糟的是第二半：`download::one` 的判定是

```rust
if dst.exists() {
    match &job.sha1 {
        None => return Ok((job.path.clone(), true)),   // ← fetch 走这条
        Some(expect) => if sha1_file(&dst).await? == *expect { return Ok(...) },
    }
}
```

`fetch` 的 `sha1` 恒为 `None` ⇒ **同名文件存在时直接被当成「已完成」跳过**，
汇总里记为"已存在 1 个"，退出码 0。用户会以为下好了。

这个工具的强项恰恰是"批量下多个文件"（分片、续传、退避重试都是为多文件长任务设计的），
而默认名让批量使用变成"每下一个都得记得改名" —— 正是它想替你省掉的那类事情。

## Decision

**两件事一起做**（issue 的建议原话是"两个一起做最安全"）：

1. **默认名从 URL 末段推导** —— 不新写规则，**复用 `url_list::filename_from_url`**：
   去 `#fragment` / `?query` → 取末段 → percent-decode → 推不出返回 `None`。
   推不出（以 `/` 结尾、只有主机名）时回落 `out.bin`，但**在输出里说明**（`name_or_out_bin` 的第二个返回值就是给这个用的）。
2. **目标已存在默认拒绝**（退出码 1），`--force` 才覆盖 —— 覆盖前先 `remove_file`，
   否则 `download::one` 会继续跳过。

## Alternatives considered

- **(a) 只改名，不加 `--force`** —— 名字对了，但"目标已存在"依然会被静默跳过。
  issue 里说得准：**改名只是"名字对了"，"存在就拒"才是"数据不会丢"**。只做一半等于修了一半。
- **(b) 默认覆盖**（存在就删了重下）—— 一个手抖的下错 URL 就能抹掉上一个文件。
  `fetch` 没有 sha1 兜底，覆盖是不可逆的。
- **(c) 默认跳过**（保持现状，只改名）—— 这正是当前 bug 的形态：跳过被记为"已存在"，退出码 0。
  不改它等于把"下好了"和"根本没下"混成一个状态。
- **(d) 先下临时名再原子改名**（`out.bin.part` → `rename`）—— 更彻底（覆盖也是原子的），
  但会改变 `download::one` 的落盘路径语义，牵动分片/续传状态文件（`.part.state`）的命名规则。
  **改动面远超 issue 的范围**，且现有 `.part` 机制已经能保证"半截文件不会冒充完成品"。
  若将来要做，单独立项。

## Consequences

- **行为变更（破坏性）**：以前 `fetch` 同名是**静默跳过**（退出码 0），现在**报错退出**（退出码 1）。
  已有的脚本如果依赖"重复跑没事"，会开始失败。这是有意的 —— 失败比假装成功好。
- 文件名规则现在**只有一份**（`url_list::filename_from_url`），`fetch` 与 `urls_file` 共用。
  改规则只改一处，两个入口不会分叉。
- `-n` 从"给了才正确"变成"可选"。`docs/user/usage.md` 三处（模板、警告、参数表）已同步。
- 单测 130 → 133（`url_list` 新增 3 条：正常推导 / query 里含斜杠 / 两种回落）。
- 没做 `-n` 的值校验：`-n "nul"`、`-n "../x"` 这类仍然会交给 OS 处理（`nul` 在 Windows 上会静默写进空设备）。
  **已知缺口**，真要防得加 sanitize —— 但那会同时改变 `urls_file` 的行为，不在本次范围。
