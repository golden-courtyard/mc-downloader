# 参与开发

**这是一个个人工具，不是社区项目。** 需求由作者自己定，接受范围很窄——先读这一页，再看
[README](README.md)（使用说明）、[AGENTS.md](AGENTS.md)（改代码不能踩什么）、
[.agents/notes/](.agents/notes/)（决策理由）。

## 收什么，不收什么

| 类型 | 收不收 | 走哪里 |
|---|---|---|
| **Bug 报告**（有复现命令、版本、预期与实际） | 收 | [Discussions](https://github.com/golden-courtyard/mc-downloader/discussions) |
| **使用问题**（怎么配源、报错看不懂） | 尽量答 | Discussions |
| **功能建议** | **不收 PR**；可在 Discussions 里说，作者自行判断 | Discussions |
| **安全类问题** | 走私密渠道，不要开公开讨论 | [SECURITY.md](SECURITY.md)（GitHub 私密漏洞报告） |
| **直接发 PR** | 只在作者明确要求时可以 | — |

理由很简单：个人工具的维护成本全部由作者承担，**每一个被接受的功能都会变成长期义务**。
所以默认不开、默认不做，除非它解决的是作者自己踩到的问题。

## 两条硬边界（PR 与讨论都适用）

1. **不内置任何下载源**：不接受"加一个默认源""自动探测可用源"这类改动。理由见
   [AGENTS.md](AGENTS.md) 铁律 1。
2. **不碰账号、不启动游戏、不做破解**：这类改动一律不讨论。

## 想自己改

Fork 之后随便改，你自己的 fork 不需要按本仓的规则来。要合回本仓才需要满足：

```bash
cargo fmt --all -- --check
cargo clippy --all-targets --locked -- -D warnings
cargo test --lib --locked
```

三条都过才有谈的前提。文档里不许出现本机绝对路径（CI 有机器路径门）。
代码 MIT、原创文档 CC BY-NC-4.0、第三方材料不授权，见 [README 的许可段](README.md#license)。
