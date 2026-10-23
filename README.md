# mc-downloader

> 从 Mojang 官方 CDN 下载 Minecraft 文件，或安装 Modrinth 格式整合包。
> 下载结果直接是标准 `.minecraft` 目录结构，可被 PCL / 官方启动器 / HMCL 识别。

---

## ⚠️ 免责声明与法律定位

**本工具属于通用下载工具，具有实质性非侵权用途。**

它只做三件事：

1. 从 **Mojang 官方 CDN**（`piston-meta.mojang.com` / `resources.download.minecraft.net`）下载**公开可访问**的文件；
2. 从 **Fabric 官方 meta**（`meta.fabricmc.net`）拉取 profile 与库；
3. 从整合包 manifest 里列出的**作者公开提供的 URL** 下载模组（逐文件 SHA1 校验）。

**它不做的**：

- ❌ 不包含、不分发任何游戏内容
- ❌ 不提供账号验证绕过、离线破解
- ❌ 不提供任何"非官方"下载源
- ❌ 不修改任何第三方软件
- ❌ 不针对特定侵权用途设计任何功能

### 相关法律条文（陈述条文本身，不构成本项目对自身合法性的判定）

| 依据 | 要点 |
|---|---|
| 《民法典》第 1197 条 | 网络服务提供者**知道或应当知道**侵权、**未采取必要措施**的，承担连带责任 |
| 法释〔2020〕19 号 第 7 条 | **明知或应知**侵权仍提供技术支持等帮助的，构成**帮助侵权** |

### 本项目的客观事实

以下只陈述可核验的事实，**不附法律结论**：

- **下载来源**：`piston-meta.mojang.com`、`resources.download.minecraft.net`、`meta.fabricmc.net`、
  `maven.minecraftforge.net`、`api.curseforge.com`。Forge 的库与 CurseForge 的 302 目标另有
  **域名白名单校验**，不在名单内的来源会被拒绝或跳过。
- **不包含、不分发任何游戏内容**；
- **不提供账号验证绕过或离线破解**；
- **不启动游戏、不管理账号**——启动交给 PCL / HMCL 这类启动器；
- **对 CurseForge 标记为禁止第三方分发的模组，只生成人工下载清单**（`manual.txt`），不取直链。

**是否构成侵权，由使用者、权利人及司法机关依法认定。本项目不作此判定。**

**使用前提**：使用者应自行确保对下载内容拥有合法授权。因使用本工具产生的任何法律责任，由使用者自行承担。

本项目与 **Mojang / Microsoft / PCL / HMCL / Modrinth / CurseForge 均无关联**。

---

## 是什么

一个**单文件命令行工具**，解决四件事：

| 场景 | 命令 |
|---|---|
| 我要某个 MC 版本的完整文件 | `vanilla` |
| 我要原版 + 加载器（Fabric / Forge），一步装好 | `run`（配置里写 `loader`）|
| 我有整合包（Modrinth / CurseForge），想铺成能启动的实例 | `pack` |
| 我要下单个文件（任意 URL，可分片 / 续传） | `fetch` |

**不做启动器**——启动、模组管理、存档、账号，交给 PCL 这类启动器。

### 验证状态（2026-10-23）

| 项 | 状态 |
|---|---|
| 1.20.1 + **Forge 47.4.10** → 目录结构 | ✅ 与官方 installer 产物逐项一致（`mainClass` / patched client / libraries 数）|
| **1.20.1 + Forge 47.4.10 → 实际启动** | ✅ **用户用 PCL 实测，可进入游戏** |
| CurseForge 整合包 188 个 mod 对账 | ✅ 三个包全都对得上（`mods/` + `manual.txt` = manifest 条数）|
| Fabric 装载 | ⚠️ **结构已验证**（7 项与 PCL 实例一致），**未实测启动** |
| NeoForge | ❌ 未实现 |

> ⚠️ 这张表**区分「结构对」和「真的能跑」**。只有标「实测」的那条是人工确认过的；
> 其余是文件级比对 —— **结构一致不等于能启动**。

---

## 用法

> **用于下载你有权下载的文件。** 本工具不判断使用者是否持有相应授权，这一点由使用者自行负责。

### 下载原版

```bash
mc-downloader vanilla 26.3 -d "D:/Games/.minecraft"
```

做：

```
versions/26.3/26.3.json      版本描述（启动器靠它识别版本）
versions/26.3/26.3.jar       客户端
libraries/...                依赖库（含 natives-windows）
assets/indexes + objects     资源
```

### 安装整合包

```bash
mc-downloader pack "xxx.mrpack" -d "D:/Games/.minecraft"
```

做：

```
① 读 modrinth.index.json → 取 minecraft / fabric-loader 版本
② vanilla::install 下原版
③ fabric::install 拉 Fabric profile（落盘 + 下 libraries）
④ 按 files[] 下模组（SHA1 校验，跳过 env.client == unsupported）
⑤ 复制 overrides / client-overrides 到实例目录
```

产物是 `versions/fabric-loader-<loader>-<mc>/`，PCL「添加游戏目录」指向 `-d` 的那个目录即可启动。

---

## 支持范围

| 输入 | 支持 |
|---|---|
| 原版任意版本 | ✅ |
| **Modrinth 整合包**（`modrinth.index.json`）| ✅ |
| **CurseForge 整合包**（`manifest.json`）| ✅ 需 `CURSEFORGE_API_KEY`；禁分发的模组生成 `manual.txt` |
| 结构相同的 `.zip`（只改了扩展名）| ✅ |
| **Fabric** 加载器 | ✅ |
| **Forge** 加载器（`loader = "forge"` / `"forge:47.4.10"`）| ✅ 需 Java；下载由本工具做，processor 交给官方 installer。**1.20.1 已实测可启动** |
| **NeoForge** 加载器 | ❌ 未做（结构类似 Forge，可照搬）|
| **HMCL / MCBBS / MMC** | ❌ 不认 |

> 不支持不是"做不了"，是**没做**。加解析器即可，但会先确认需求。

> ⚠️ **加载器只保证目录结构**（`versions/<id>/<id>.json` + `libraries/`）。
> **是否真能启动，本工具不验证**——请用 PCL / HMCL 这类启动器实际打开一次。

---

## 特性

- **并发 16** 下载（`buffer_unordered`），tokio 多线程运行时
- **逐文件 SHA1 校验**（Modrinth 的 `hashes.sha1` / 原版 manifest 的 `sha1`）
- **断点续传**：已存在且哈希匹配的文件直接跳过
- **平台过滤**：`libraries` 里按 `rules` 只下 Windows 的，native 走 `classifiers["natives-windows*"]`
- **纯本地**：无账号、无遥测、无网络请求以外的副作用

---

## 构建

```bash
cargo build --release
# 产物: target/release/mc-downloader.exe
```

依赖：Rust 1.80+（`let-else` / `LazyLock` 等）。

---

## 为什么不用启动器的下载功能

不针对任何项目。本工具的定位是：

> **"把文件放到正确的位置"这一件事**，做成一个可脚本化、可复现、可审计的命令。

它不替代启动器，也不依赖启动器的内部实现。

---

## License

MIT，见 [LICENSE](LICENSE)。
