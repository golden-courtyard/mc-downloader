# 自己获取并填写下载源

本工具**不内置任何下载源**：六个键全部由你提供（理由见 README 的「为什么源要你自己配」与
[AGENTS.md](../../AGENTS.md) 铁律 1）。这篇讲三件事：**去哪拿、每个键该指到哪一层、怎么验证填对了**。

> 记住一条：这里出现的地址是**公开端点的事实**，不是工具的内置默认值。
> `src/` 里出现任何源地址，CI 会直接红（`grep` 闸门）——文档写地址、代码不写，是刻意的分工。

## 一、六个键分别是什么

配置文件里的样子（`[source]` 段）：

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
| `assets` | **基址**（不要结尾 `/`） | `{assets}/{hash 前两位}/{hash}` |
| `fabric` | **基址** | `{fabric}/v2/versions/loader/{mc}`、`…/{loader}/profile/json` |
| `forge_meta` | **基址**，要**含 `/net/minecraftforge/forge`** | `{forge_meta}/promotions_slim.json` |
| `forge_maven` | **maven 根** | `{forge_maven}/net/minecraftforge/forge/{ver}/forge-{ver}-installer.jar` 与库 |
| `neoforge` | **基址**，要**含 `/releases`** | `{neoforge}/net/neoforged/neoforge/…` |

`metadata` 与其余五个的区别最容易被搞错：**它是完整 URL，别的是基址**。

## 二、官方端点（2026-11-23 实测）

| 键 | 官方端点 | 实测 |
|---|---|---|
| `metadata` | `https://piston-meta.mojang.com/mc/game/version_manifest_v2.json` | GET 200，279726 字节（**HEAD 会被拒，验证请用 GET**） |
| `assets` | `https://resources.download.minecraft.net` | 根路径 404 属正常（内容寻址，只有具体对象存在） |
| `fabric` | `https://meta.fabricmc.net` | `GET /v2/versions/loader/1.20.1` → 200 |
| `forge_meta` | `https://files.minecraftforge.net/net/minecraftforge/forge` | `GET …/promotions_slim.json` → 200 |
| `forge_maven` | `https://maven.minecraftforge.net` | `GET /net/minecraftforge/forge/maven-metadata.xml` → 200 |
| `neoforge` | `https://maven.neoforged.net/releases` | `GET /net/neoforged/neoforge/maven-metadata.xml` → 200 |

于是最小可用配置就是：

```toml
[source]
metadata    = "https://piston-meta.mojang.com/mc/game/version_manifest_v2.json"
assets      = "https://resources.download.minecraft.net"
fabric      = "https://meta.fabricmc.net"
forge_meta  = "https://files.minecraftforge.net/net/minecraftforge/forge"
forge_maven = "https://maven.minecraftforge.net"
neoforge    = "https://maven.neoforged.net/releases"
```

> 这些是**上游发布方自己的地址**，可能变。变了就按上面那张"指到哪一层"的表自己改 ——
> 这正是"源由你配"的意义：改配置，不用等发版。

## 三、验证填对了没有

三条，从便宜到贵：

1. `mc-downloader --print-config` —— 打印它**找过哪些路径**、每个键的生效值与来源层。
   先确认它读的是你以为的那个文件。
2. `mc-downloader probe <六个地址里的任意几个>` —— 测真实吞吐，用来在官方与镜像之间选。
3. 直接跑一次 `mc-downloader vanilla 1.20.1 -d <空目录>`。**缺哪个键就报哪个键**，文案里带配置文件路径。

**填错层级是最常见的错**，症状很好认：

| 症状 | 常见原因 |
|---|---|
| `forge_meta` 拿到 404 | 少了 `/net/minecraftforge/forge`（只写到域名） |
| `neoforge` 拿到 404 | 少了 `/releases` |
| `assets` 全是 404 | 结尾多写了 `/`，拼出 `//ab/…` |
| `metadata` 解析失败 | 写成了基址（少了 `/mc/game/version_manifest_v2.json`） |
| 403 | 对方要求 UA 或限流；换镜像或稍后再试 |

## 四、用镜像或自建

三类来源，责任不同：

| 来源 | 注意什么 |
|---|---|
| **官方端点** | 最稳；国内可能慢，用 `probe` 量一下再决定并发与分片 |
| **公共镜像** | 它自己可能滞后（新版本 manifest 还没同步）。镜像必须如实转发 **Range** 与 `Content-Range`，否则分片会降级成单流 |
| **自己反代**（nginx 等） | 必须：① 透传 `Range`/`Content-Range`；② **不要**加 `Content-Encoding`（本工具不解压，会直接拒绝）；③ 保留 `ETag` 或 `Last-Modified`（`--if-range` 依赖它） |

自建时最容易踩的两条，我们都在别处吃过亏：**压缩**（`src/download.rs` 的 `reject_unrequested_encoding`）
与**强验证器缺失**（`--if-range` 会退化，远端被换时只能靠 416/SHA1 发现）。

## 五、边界（写清楚，免得误会）

- 这里列地址**只是为了让你少查一次**。工具本身零地址、零探测、零兜底 —— 判据是 CI 的 `grep` 闸门。
- 只下载**公开可访问**的文件；不涉及账号、授权、DRM。见 README 的「免责声明与法律定位」。
- 第三方镜像的合规性由**你**判断；本工具不替你选源，也不替你背书。
