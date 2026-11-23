# 发布清单

给"要发一个版本"的人（多半是我自己）。每一步都有判据，**判据不过就不要发**。

## 1 代码状态

```bash
cargo fmt --all -- --check
cargo deny check                                      # 许可证 / RustSec 公告 / 只从 crates.io 取
cargo clippy --all-targets --locked -- -D warnings
CARGO_INCREMENTAL=0 cargo test --all-targets --locked
cargo build --release --locked
```

- [ ] 三条全绿（本地绿 **不等于** CI 绿，见第 4 步）
- [ ] `mc-downloader --print-config` 能跑（它需要一个子命令以外的路径，别只在 `cargo test` 里验过）
- [ ] `src/` 里零下载源地址：`grep -rnE 'piston-meta|launchermeta|bmclapi|download\.minecraft|meta\.fabricmc|files\.minecraftforge|maven\.minecraftforge|maven\.neoforged|libraries\.minecraft|resources\.download' src/` 无输出

## 2 文档与版本

- [ ] `README.md` 的「验证状态」表更新到本次日期，**结构对 / 真的能跑**分开写
- [ ] `CHANGELOG.md` 新增本次版本一节，**已知残留**必须写（没做的比做了的更需要写下来）
- [ ] **首次公开或大改之前先发 `-rc.N`**：契约冻结、只修不改；一轮反馈没有问题再打正式版
      （`tests/version_sync.rs` 会核对标签与 `Cargo.toml` 一致，`-rc.N` 也一样）
- [ ] `Cargo.toml` 版本号按 [versioning.md](versioning.md) §四**从 notes 推导**：
      改契约的 `implemented` ⇒ minor；只有 `bug-fix` ⇒ patch；只有 `proposed` ⇒ 不发版；**不跳号**
- [ ] `release(mc): x.y.z` 提交的正文**列出本次包含的 note 路径**（三行内），让"为什么跳这一位"可查
- [ ] `Cargo.lock` 跟着更新：用 `scripts/bump-version.sh <minor|patch|x.y.z>` 抬版本
      （它一次改掉 `Cargo.toml`、冻结 CHANGELOG、刷新 `Cargo.lock`），然后
      `cargo test --test version_sync --locked` 通过（版本 / CHANGELOG / 标签三者一致）
- [ ] `docs/user/usage.md` 里新增/变更的参数都有条目
- [ ] 文档 emoji 扫描为 0（仓库规矩）
- [ ] `.agents/notes/` 里本次的持久决策已写（没有就说明这次只是机械改动）

## 3 打标签

```bash
git tag mc-v0.6.0            # 轻量标签（与 dsh 同形：<包简写>-v<semver>）
git push origin mc-v0.6.0
```

- 标签形态与跳号规则见 [versioning.md](versioning.md)：`mc-v<x.y.z>`，预发布加 `-alpha.N` / `-rc.N`；
- 标签里的版本号与 `Cargo.toml` 的 `version` **必须一致** —— 不一致的发布物无法追溯是哪个提交编出来的。

### 改工作流之前先本地解析

```bash
python -c "import yaml; yaml.safe_load(open('.github/workflows/ci.yml'))"
```

GitHub 的报错只有一句 "This run likely failed because of a workflow file issue"，不指行；
而 YAML 缩进错一级（例如把 `services:` 写成 6 空格而 job 的键是 4 空格）本地一解析就能看见。
2026-11-23 因为这一条连推两次红。


### 静态扫描（CodeQL）：已启用

2026-11-23 加过、失败过、又加回来了，过程值得记：

1. 私有仓上两次失败，最终原因：`Advanced Security must be enabled for this repository to use code scanning.`
   —— 代码扫描要 GitHub Advanced Security，**公开仓免费、私有仓要授权**；
2. 于是给 workflow 加了一个按仓库可见性开的门：`if: ${{ !github.event.repository.private }}`，
   私有期间整 job 跳过（run 仍是绿色，不是假门禁）；
3. 仓库转公开后手动触发一次：**`analyze => success`**，门自动打开。

现在它是第四类检查（安全），与 `cargo-deny` 互补：CodeQL 看**代码**，deny 看**依赖树**。
触发：push / PR / 每周一 / 手动。

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
