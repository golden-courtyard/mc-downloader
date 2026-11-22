# 参与开发

面向要改这个仓库的人（含 AI）。使用说明在 [README](README.md)，
改代码**不能踩什么**在 [AGENTS.md](AGENTS.md)，决策理由在 [.agents/notes/](.agents/notes/)。
本文只讲**怎么把改动做进来**。

## 1 环境

| 项 | 要求 |
|---|---|
| Rust | `rust-version = "1.99"`（`Cargo.toml` 里写着，CI 有 `msrv` job 守着） |
| 目标平台 | Windows 是发布物（`.exe`）；Linux/macOS 能编译能测，CI 也跑 |
| 外部工具 | 只有 `forge` 子命令会调用加载器的 `installer.jar`（需要 Java），其余子命令不需要 |

```bash
cargo build --release --locked      # 产物：target/release/mc-downloader[.exe]
```

## 2 四条门（缺一条就别提 PR）

```bash
cargo fmt --all -- --check
cargo clippy --all-targets --locked -- -D warnings
CARGO_INCREMENTAL=0 cargo test --all-targets --locked
cargo build --release --locked
```

`CARGO_INCREMENTAL=0` 不是洁癖：增量目录在某些沙箱里会留下删不动的文件，
下一轮 cargo 写入被拒 ⇒ `exit 101` + `error: could not compile ... os error 5`，
看起来像"代码改坏了"，其实是构建环境自锁。细节见 `AGENTS.md` 第三节。

提交前装一次钩子（暂存有 `.rs` 就跑 `cargo fmt --check`）：

```bash
git config core.hooksPath .githooks
```

## 3 代码地图

```
src/
  main.rs      子命令与全局参数（vanilla / run / pack / fetch / probe）
  settings.rs  配置层：--config / 环境变量 / 平台目录，合并优先级，[source] 解析
  config.rs    下载脚本 [[task]] 的结构与语义校验（含 schema = 1）
  download.rs  下载引擎（★）：client / one / one_split / one_stream / fetch_piece / StallGuard
  progress.rs  进度行与限速令牌桶
  scheduler.rs 任务调度；source.rs 多源池（EWMA）
  piece.rs     分片切片与续传状态（.part.state）
  vanilla.rs / fabric.rs / forge.rs / neoforge.rs / pack/   各装配流程
  pathsafe.rs  落点安全（check_name / check_rel / under_root）
  url_list.rs  URL 列表解析与「单 URL → Job」的唯一推导口
  httpname.rs  Content-Disposition 解析
  motw.rs      Windows Mark-of-the-Web
```

★ 标的是最容易改出问题的地方，动之前先读 `.agents/notes/`。

## 4 测试怎么写

| 层 | 放哪 | 管什么 |
|---|---|---|
| 单元测试 | `src/**/mod tests` | 具体行为与错误分支 |
| 参数化（rstest） | `tests/params_*.rs` | 同一断言的多组输入；加边界只加一行 `#[case]` |
| 属性（proptest） | `tests/props_*.rs` | 不变量；反例自动缩小 |
| HTTP 语义 | `tests/http_semantics.rs` | 自建假服务器（`Fault` 变体）造故障：截断、谎报长度、416、Range 回 200 |
| 回归 | `tests/*_order.rs`、`tests/cli_surface.rs` | 特定 bug 的最小复现；CLI 表面不许悄悄少东西 |

写测试的三条纪律（每条都是踩过才写的，详见 `AGENTS.md`）：

1. **不写死还在动的数值**（片大小公式这类），否则每次微调都要改测试；
2. **不引用实现里的常量**当判据 —— 那是循环论证；
3. **关掉 `failure_persistence`** —— 否则失败会往仓库根写 `proptest-regressions/`。

**边界单测要按平台断言**：盘符、大小写这类规则是 Windows 专属的，
写成跨平台断言会"本地绿、CI 红"（2026-11-23 真踩过，见 `src/httpname.rs` 的 `cfg!(windows)` 用法）。

## 5 要动下载引擎时

`download.rs` 只该类改动三条最容易出事的地方（都有实测记录，见 `AGENTS.md` 第五节）：

- **416 不能当"不支持 Range"**：它恰恰说明支持 Range，只是请求的区间给不出来；
- **Range 请求回 200 是要降级**，不是错误：`RangeUnsupported` 标记 → 退回 `one_stream`；
- **续传判据必须用同一个 `--piece-conns`**：片大小是 `conns` 的函数，判据错就会静默从头重下。

## 6 决策记录

持久决策（为什么这样、放弃了什么）写进 `.agents/notes/{lifecycle}/{class}/yyyy-mm-dd-topic.md`：

- 头三行固定：`# Agent Note: <title>` + 空行 + `Status: ...`
- 正文：`## Problem` → `## Decision` → `## Alternatives considered`（**强制**）→ `## Consequences`
- **不建 INDEX**（索引会腐烂，用文件夹 + 搜索当索引）

机械改动、纯重命名不用写。

## 7 提交与发布

- 提交信息中文，`type(scope): 一句话`，body 写**为什么**；一次提交别混两件不相关的事。
- 文档不用 emoji。
- 改了行为/参数，同一次提交里更新 `README.md` 的「验证状态」与 `docs/user/usage.md`。
- 发布流程见 [docs/dev/release.md](docs/dev/release.md)（含"本地绿不等于 CI 绿"这条）；
  版本号怎么跳、标签长什么样见 [docs/dev/versioning.md](docs/dev/versioning.md)；
  抬版本用 `scripts/bump-version.sh`，一致性由 `tests/version_sync.rs` 在 `cargo test` 里守。

## 8 已知的边界（别当 bug 修）

- **不内置下载源**：源由用户配置。看到"为什么不做自动探测/兜底"，答案是 `AGENTS.md` 铁律 1。
- **不做 GUI、不做浏览器插件、不启动游戏、不管理账号**：定位边界，理由在 `AGENTS.md` 第一节。
- **CurseForge 禁止第三方分发的模组只生成 `manual.txt`**：法律边界，不是技术限制。
