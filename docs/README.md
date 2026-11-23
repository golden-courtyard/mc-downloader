# 文档索引

这个仓库的文档按**读者**分三层，每个事实只有一个家，别在两个地方各写一遍。

| 你想干什么 | 看哪里 |
|---|---|
| 装好它、把游戏目录搞对 | [README](../README.md)（五分钟上手） |
| 查某个参数/配置键的准确含义 | [user/usage.md](user/usage.md) |
| 配 `[source]`（**必须先做**） | [user/usage.md](user/usage.md) 的「源：必须先自己配」 |
| 去哪拿官方源、每个键该填到哪一层、怎么验证 | [user/sources.md](user/sources.md) |
| 出错了找原因 | [user/usage.md](user/usage.md) 的「出错了先看这三处」；仍不行看 [reports/](reports/) |
| 改代码（构建、测试、提交门） | [CONTRIBUTING.md](../CONTRIBUTING.md) |
| 改代码前不能踩什么（铁律、坑、判据） | [AGENTS.md](../AGENTS.md) |
| 为什么是现在这样（放弃了什么） | [.agents/notes/](../.agents/notes/) |
| 架构与取舍的全局视图 | [design/architecture.md](design/architecture.md) |
| 接下来打算做什么 | [design/archive/](design/archive/) |
| 某个版本改了什么 | [CHANGELOG.md](../CHANGELOG.md) |
| 要发一个版本 | [dev/release.md](dev/release.md) |
| 版本号怎么跳、标签怎么打 | [dev/versioning.md](dev/versioning.md) |
| 平台范围（Windows / Linux / macOS） | [design/platform-scope.md](design/platform-scope.md) |
| 测试设施（云服务器 / Kali / Docker） | [dev/testing-infrastructure.md](dev/testing-infrastructure.md) |
| 攻防靶场（只打自己的靶） | [dev/attack-defense-lab.md](dev/attack-defense-lab.md) |
| 网络故障实测记录 | [reports/net_faults-report-20261021.md](reports/net_faults-report-20261021.md) |

## 目录

```
docs/
  user/        给使用者的：怎么用、参数含义、排错
  dev/         给维护者的：发布流程、测试设施、环境排查
  design/      架构（architecture.md）与已完稿的任务书归档（archive/）
  reports/     实验与审查产物（定量报告、攻击面、平台能力、代码审查）
```

## 三类产物的分工（别混）

| 产物 | 回答什么 | 判据 |
|---|---|---|
| `docs/user/` | 怎么用 | 用户照着做能成 |
| `AGENTS.md` | 改的时候不能踩什么 | 每条都有"为什么"和可查的判据 |
| `.agents/notes/` | 为什么选这个而不是那个 | 头三行格式固定；`Alternatives considered` 强制 |
| `docs/reports/` | 数是量出来的 | 口径先写后测；每个数可回溯到 raw → 脚本 → 图/数 |

## 写文档的三条规矩

1. **不用 emoji**（勾叉、警告、星、图形符号都算）——写文字。
2. **事实只有一个家**：参数含义只在 `user/usage.md`，铁律只在 `AGENTS.md`，理由只在 notes。
   别在 README 里再抄一份参数表，抄了就会腐烂。
3. **改完代码顺手改文档**：改了默认值/参数/行为，同一次提交里把上面这种表更新掉。
   README 的「验证状态」表尤其不许含糊 —— 它区分「结构对」和「真的能跑」。
