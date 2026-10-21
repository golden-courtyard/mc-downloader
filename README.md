# mc-downloader

> 从 Mojang 官方 CDN 下载 Minecraft 文件，或安装 Modrinth 格式整合包。
> 下载结果直接是标准 `.minecraft` 目录结构，可被 PCL / 官方启动器 / HMCL 识别。

---

## ⚠️ 免责声明（请先读）

**本工具只做三件事**：

1. 从 **Mojang 官方 CDN**（`piston-meta.mojang.com` / `resources.download.minecraft.net`）下载**公开可访问**的文件；
2. 从 **Fabric 官方 meta**（`meta.fabricmc.net`）拉取 profile 与库；
3. 从整合包 manifest 里列出的**作者提供的 URL** 下载模组（逐文件 SHA1 校验）。

**本工具不做**：

- ❌ 不包含、不分发任何游戏内容
- ❌ 不提供账号验证绕过、离线破解
- ❌ 不提供任何"非官方"下载源
- ❌ 不修改任何第三方软件

**使用前提**：你需自行确保对下载的内容拥有合法授权（例如持有正版 Minecraft 账号）。

本项目与 **Mojang / Microsoft / PCL / HMCL / Modrinth / CurseForge 均无关联**。

---

## 是什么

一个**单文件命令行工具**，解决两件事：

| 场景 | 命令 |
|---|---|
| 我只想要某个 MC 版本的完整文件 | `vanilla` |
| 我有一个 Modrinth 整合包，想铺成能启动的实例 | `pack` |

**不做启动器**——启动、模组管理、存档、账号，交给 PCL 这类启动器。

---

## 用法

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
| 结构相同的 `.zip`（只改了扩展名）| ✅ |
| Fabric 加载器 | ✅ |
| **Forge / NeoForge** | ❌ 明确报错退出 |
| **CurseForge**（`manifest.json`）| ❌ 不认 |
| **HMCL / MCBBS / MMC** | ❌ 不认 |

> 不支持不是"做不了"，是**没做**。加解析器即可，但会先确认需求。

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
