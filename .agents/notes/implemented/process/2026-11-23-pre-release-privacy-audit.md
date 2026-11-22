# Agent Note: 发布前隐私审计（仓库内外的本机信息）

Status: implemented

## Problem

发布前需要确认：文档、脚本、实验产物、提交历史里有没有泄露本机/内网信息。
不只是"看起来像密码"的东西——**绝对路径、内网 IP、ssh 私钥位置、用户名**都算。

做法是机器扫，不靠眼看：对 142 个受控文件与全部 209 条提交（`git log -p` 含 diff 与 commit message）
跑同一组正则：Windows 绝对路径、`/home/*`、私有网段 IP、MAC、非 noreply 邮箱、真实姓名、
以及 11 个内置源主机名。

## Decision

**已处理的三处**（都在 `60b43e9`）：

| 位置 | 内容 | 处置 |
|---|---|---|
| `.codeartsdoer/specs/net_faults/*.md` | 内网 VM 地址 `172.19.x.x`（本机实测的具体值已抹去）、`ssh -i ~/.ssh/<key> kali@…` | 取消跟踪 + 进 `.gitignore`（工具工件不该入库） |
| `tools/baseline_{main,run,run_parallel}.txt` | `C:/Users/<user>/AppData/Local/Temp/…` 绝对路径 + 真实源 URL | 删除（无任何文档引用） |
| `README.md` 法律定位段 | 仍写着"下载来源：…（旧的内置源列表）" | 改写为"全部由用户在 `[source]` 提供" |

**判定为可接受、保留不动的**：

- `src/pathsafe.rs` 里的 `D:\tmp\out`、`C:\Windows\Temp\evil.bin` —— 通用测试夹具，不含用户名；
- `scripts/quant/raw/*` 里的 `/home/kali` —— 通用 VM 用户名，且这些是报告引用的原始数据；
- `docs/dev/release.md` 的 `grep` 命令与 CI 的黑名单 —— **黑名单必须列出主机名才能 grep**，
  与"内置下载源"是两回事；
- 提交作者 `68047429+liumiaoqi@users.noreply.github.com` —— GitHub noreply，无真实邮箱。

**没做：重写历史。** 旧提交里仍有那 11 处内置源地址（约 200 处，全是公开主机名，无凭据）。
理由：① 合规争点在**发布物的默认行为**，二进制里已零命中；② 历史是"为什么删掉"的审计证据，
改成"从没存在过"反而削弱可追溯性；③ 就算重写，CI 黑名单与 `docs/dev/release.md` 仍需写出这些主机名，
仓库里不可能"零出现"。

**若日后要重写**：`git filter-repo --replace-text` + 强推；代价是所有哈希改变、已有克隆/分叉要重拉。
现在没有 tag、没有发布、单作者，是成本最低的时刻——真要做就趁现在。

## Alternatives considered

- **把 `.codeartsdoer/` 整目录删掉**：否。它是本地工具的产物，删掉下次还会生成；取消跟踪 + ignore 才是正确处置。
- **把 `scripts/quant/raw/` 里的 `/home/kali` 全部脱敏**：否。这些是定量报告的原始数据，
  脱敏后就无法复现；`kali` 是发行版默认用户名，不含个人信息。
- **重写历史**：见上，保留。

## Consequences

**变好的**：受控文件里不再有内网 IP、ssh 私钥路径、机器绝对路径；README 的法律事实与代码一致。

**代价 / 已知残留**：

- **旧提交仍带那 11 个主机名**（见上）。
- 扫描是**基于模式**的：它能抓到形如 `X:\...`、`/home/...`、私网 IP 的东西，
  抓不到"用自然语言描述的本机信息"。发布前仍建议人工读一遍 `docs/reports/` 里的实验记录。
- `.gitignore` 里新增了 `.codeartsdoer/`；如果以后想把它并入仓库，先得脱敏。
