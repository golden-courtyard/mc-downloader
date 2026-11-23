# 自己获取并填写下载源

本工具**不内置任何下载源**：六个键全部由你提供（理由见 README 的「为什么源要你自己配」与
[AGENTS.md](../../AGENTS.md) 铁律 1）。这篇讲的是**契约**（每个键必须指到哪一层）、
**去哪查权威信息**、以及**怎么验证填对了**。

## 为什么这里不给一张"现成地址表"

三个理由，每个都能反驳"贴一张表更省事"：

1. **上游地址会变**。写死在我的文档里，等于把"可能会过期的东西"伪装成事实；写进配置就不一样 ——
   变了你自己改一行，不用等我们发版。
2. **大多数端点没有官方文档**。Mojang 的 `piston-meta` 没有公开 API 规范（社区逆出来的）；
   Forge 也没有。把它们排成表格，会让它们显得比实际更权威。
3. **工具的判据是"代码里没有源"**（CI 的 `grep` 闸门查 `src/`）。文档如果变成一张可直接抄的地址表，
   只是把"内置"从代码搬到了文档 —— 那不是这条规矩想要的结果。

所以：**契约我们给准，地址请你按上游的权威来源自己填。**

## 一、六个键各自必须指到哪一层（这是我们和你的契约）

`[source]` 里的样子：

```toml
[source]
metadata    = ""
assets      = ""
fabric      = ""
forge_meta  = ""
forge_maven = ""
neoforge    = ""
```

| 键 | 必须指到哪一层 | 我们拿它拼什么 |
|---|---|---|
| `metadata` | **完整 URL**（指向 `version_manifest_v2.json` 本身） | 直接 GET，不再拼路径 |
| `assets` | **基址**（结尾不要 `/`） | `{assets}/{hash 前两位}/{hash}` |
| `fabric` | **基址** | `{fabric}/v2/versions/loader/{mc}`、`…/{loader}/profile/json` |
| `forge_meta` | **基址**，要含 `/net/minecraftforge/forge` | `{forge_meta}/promotions_slim.json` |
| `forge_maven` | **maven 根** | `{forge_maven}/net/minecraftforge/forge/{ver}/…installer.jar` 与库 |
| `neoforge` | **基址**，要含 `/releases` | `{neoforge}/net/neoforged/neoforge/…` |

`metadata` 与其余五个的区别最容易搞错：**它是完整 URL，别的是基址**。

## 二、权威信息在哪（2026-11-23 核对）

| 上游 | 有没有官方文档 | 该看什么 |
|---|---|---|
| Mojang（原版清单 / assets） | **没有公开 API 规范** | 社区与第三方整理（例如 [Mojang-Api-Meta](https://github.com/NovaStepStudios/Mojang-Api-Meta) 这类汇总）。字段含义以 `version.json` 自解释为准；assets 是**内容寻址**（`{前两位}/{hash}`） |
| Fabric | **有** meta API 说明 | [fabric-meta 的 README](https://github.com/modmuss50/fabric-meta) 逐个列出 `/v2/versions/loader`、`/v2/versions/loader/:game_version` 等端点 |
| Forge | **没有** API 文档 | 只有事实：`promotions_slim.json` 存在于 maven 的 `net/minecraftforge/forge` 下；库与 installer 走标准 Maven 布局 |
| NeoForge | **有机器可读的镜像清单** | [neoforged.net/mirrorlist.json](https://neoforged.net/mirrorlist.json) 给出 maven 根（含 `/releases`）；构建配置也写 `maven.neoforged.net/releases` |

**怎么自己找**：任何 Maven 仓库都可以先探 `…/<groupId 路径>/maven-metadata.xml`；
清单类端点则看它有没有 `version_manifest_v2.json` / `promotions_slim.json` 这种"已知文件名"。
找不到就别猜 —— 换个源，或者问上游。

## 三、验证填对了没有

三条，从便宜到贵：

1. `mc-downloader --print-config` —— 打印它**找过哪些路径**、每个键的生效值与来源层。先确认读的是你以为的文件。
2. `mc-downloader probe <几个候选地址>` —— 测真实吞吐，在官方与镜像之间选。
3. 直接跑一次 `mc-downloader vanilla 1.20.1 -d <空目录>`。**缺哪个键就报哪个键**，文案里带配置文件路径。

**填错层级是最常见的错**，症状好认：

| 症状 | 常见原因 |
|---|---|
| `forge_meta` 404 | 少了 `/net/minecraftforge/forge`（只写到域名） |
| `neoforge` 404 | 少了 `/releases` |
| `assets` 全 404 | 结尾多写了 `/`，拼出 `//ab/…` |
| `metadata` 解析失败 | 写成了基址（少了 `/mc/game/version_manifest_v2.json`） |
| 403 | 对方要 UA 或限流；换镜像或稍后再试 |
| 全都很慢但能过 | 用 `probe` 量一下，再决定 `--piece-conns` 与 `--max-speed` |

## 四、用镜像或自建

| 来源 | 注意什么 |
|---|---|
| **上游端点** | 最稳；国内可能慢，先 `probe` |
| **公共镜像** | 可能滞后（新版本清单还没同步）。必须如实转发 **Range** 与 `Content-Range`，否则分片会降级成单流 |
| **自己反代**（nginx 等） | ① 透传 `Range`/`Content-Range`；② **不要**加 `Content-Encoding`（本工具不解压，会直接拒绝）；③ 保留 `ETag` 或 `Last-Modified`（`--if-range` 依赖它） |

后两条对应我们修过的两个缺陷：未经请求的压缩会被拒（`reject_unrequested_encoding`），
缺强验证器时 `--if-range` 会退化、远端被换只能靠 416 / SHA1 发现。

## 五、边界

- 只下载**公开可访问**的文件；不涉及账号、授权、DRM。见 README 的「免责声明与法律定位」。
- 第三方镜像的合规性由**你**判断；本工具不选源、不探测、不背书。
- 本文引用的第三方链接只是"去哪儿查"的线索，内容以对方为准；我们不复制、不承诺其准确性。
