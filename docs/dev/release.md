# 发布清单

给"要发一个版本"的人（多半是我自己）。每一步都有判据，**判据不过就不要发**。

## 1 代码状态

```bash
cargo fmt --all -- --check
cargo clippy --all-targets --locked -- -D warnings
CARGO_INCREMENTAL=0 cargo test --all-targets --locked
cargo build --release --locked
```

- [ ] 三条全绿（本地绿 **不等于** CI 绿，见第 4 步）
- [ ] `mc-downloader --print-config` 能跑（它需要一个子命令以外的路径，别只在 `cargo test` 里验过）
- [ ] `src/` 里零下载源地址：`grep -rnE 'piston-meta|launchermeta|bmclapi|download\.minecraft' src/` 无输出

## 2 文档与版本

- [ ] `README.md` 的「验证状态」表更新到本次日期，**结构对 / 真的能跑**分开写
- [ ] `CHANGELOG.md` 新增本次版本一节，**已知残留**必须写（没做的比做了的更需要写下来）
- [ ] `Cargo.toml` 版本号按语义化版本跳：
  - 破坏性（用户必须改配置/改脚本）→ minor
  - 只加功能 → minor
  - 只修 bug → patch
- [ ] `Cargo.lock` 跟着更新（`cargo build` 一次即可，diff 应只有本包那一行）
- [ ] `docs/user/usage.md` 里新增/变更的参数都有条目
- [ ] 文档 emoji 扫描为 0（仓库规矩）
- [ ] `.agents/notes/` 里本次的持久决策已写（没有就说明这次只是机械改动）

## 3 打标签

```bash
git tag -a v0.6.0 -m "v0.6.0"
git push origin v0.6.0
```

标签名与 `Cargo.toml` 的版本号**必须一致** —— 不一致的发布物无法追溯是哪个提交编出来的。

## 4 推送后真的看 CI

```bash
gh run watch
```

- [ ] `check` / `check-windows` / `msrv` 三个 job 全绿
- [ ] 测试条数下限那步没掉（`0 tests` 与 passed 同外观，只能靠数量守）

**不要把「push 成功」当成「CI 通过」。** 本仓库已经栽过：本地绿、Linux 上红（平台专属单测）、
以及只跑了一半检查就推。

## 5 发布物

- [ ] Windows：`target/release/mc-downloader.exe`，签名/校验和（SHA256）附在发布说明里
- [ ] Linux：同一条 `cargo build --release --locked` 产物
- [ ] 发布说明直接引用 `CHANGELOG.md` 对应章节，**不要现编**
- [ ] 明确写出本次的**破坏性变更与迁移办法**（例：0.6.0 起必须自己配 `[source]`）

## 6 发布后

- [ ] 用一个**干净目录**按 README 的「五分钟上手」走一遍（不要用开发目录，那是已验证过的环境）
- [ ] `vanilla` 在**没有配源**的机器上应给出「未配置下载源」的指引，而不是别的错误
- [ ] 关闭本次发布覆盖的 issue；没关的写清为什么留着

## 暂时不做的动作（有理由的，不是忘了）

- **不加 release 工作流**（tag 自动构建并上传 `.exe`）：等**安装器**做完再加。
  现在加了只会多一个没人维护、又在安装器出现后要重写的 YAML。
  所以当前打标签后，发布物是手工 `cargo build --release --locked` 产出、手工附 SHA256。
