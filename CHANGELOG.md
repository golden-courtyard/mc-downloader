# 变更记录

本文件记录用户可见的变化。格式参考 Keep a Changelog，版本号遵循语义化版本。

## [0.6.0] - 2026-11-23

### 破坏性变更

- **不再内置任何下载源**（官方或第三方都不内置，也不做自动探测）。
  `vanilla` / `run` / `pack` 现在必须在配置文件的 `[source]` 段提供六个键：
  `metadata` / `assets` / `fabric` / `forge_meta` / `forge_maven` / `neoforge`。
  缺哪项报哪项，并说明该填在哪个文件里（`mc-downloader --print-config` 会打印它读的是哪个文件）。
  `fetch <url>` 不受影响 —— 你给地址，它只做传输。
  理由见 README 的「为什么源要你自己配」与 `.agents/notes/implemented/feature/2026-11-23-user-sources-cd-and-script-v2.md`。
- 下载脚本新增 `schema` 字段：省略按 `1` 处理，写别的数值会明确报「太新 / 太旧」。

### 新增

- `fetch` 认 `Content-Disposition` 当落地名（过 `pathsafe`）。优先级：
  `--name` > `Content-Disposition` > URL 末段 > `out.bin`。`--no-content-disposition` 可关。
- `--if-range`：分片带 `If-Range`，远端中途被换时当场降级成单流重下（默认关）。
- `--require-sha1`：没有校验和的文件拒绝下载（默认关）。
- `--print-config`：打印生效值与**来源层**（CLI / 配置文件 / 默认）。
- `--conns` / `--jobs`：`--piece-conns` / `--concurrency` 的别名。
- 脚本 v2：任意 `[[task]]` 可写 `retries` / `timeout_secs` / `max_speed` 覆盖全局；
  脚本级 `continue_on_error`（失败后继续，末尾汇总并以非零退出）。
- 416 的错误里带出**远端当前长度**（读 `Content-Range: bytes */N`）。
- 停滞检测：窗口内推进不足 4 KiB 即中断，不再被「每 0.5 秒吐 1 字节」的服务器挂死。
- Windows：给 `.exe` / `.dll` 打 Mark-of-the-Web（`--no-motw` 可关）。
- 拒绝「未经请求的压缩」响应（`Content-Encoding: gzip/br/...`）：本工具不解压，
  直接写盘会得到内容错误的文件，所以宁可报错。

### 修复

- 重试退避加抖动（`[0,500ms)`）：此前 `1 << attempt` 是固定值，多分片同时失败会在同一秒一起重试，
  瞬时并发翻倍（issue #17）。现在退避是纯函数 `backoff_delay(attempt, seed)`，可测。
- `--if-range` 打开时，服务器回 412 不再报成「服务器不支持 Range」，而是
  「远端文件在下载期间被替换 —— 我们发的验证器已过期」。

- `fetch --force` 会在校验输入**之前**删除文件，导致 `--name` 越界时删掉 `--dir` 之外的文件。
- `urls_file` 里一行坏 URL 会让整批中止（现在跳过并计数）。
- `run` 与 `fetch` 对同一个 URL 推出不同文件名（`run` 不剥 `?query`、不 percent-decode）。
- `--piece-conns` 与续传判据不一致时静默从头重下。
- SOCKS 代理被静默忽略并直连（现在启动期拒绝并给出两种替代方案）。

### 安全

- **TLS 后端换成 rustls**（`reqwest` 关闭默认特性、启用 `rustls-tls`）：
  此前是 native-tls（Windows 走 schannel、Linux 走 OpenSSL），依赖树里带着 `openssl-sys`。
- 新增 `cargo-deny` 门禁（`deny.toml` + CI 步骤）：许可证白名单、RustSec 公告、只从 crates.io 取。
  它当场抓到两条：`url 2.5.8` 有 RUSTSEC-2026-0592（已升 2.5.9）、`openssl-sys` 在树里（见上）。

- 依赖更新：`zip` 9.8.1（修 "Zip Cloaking"）、`tokio` 1.54.1（Dependabot 告警 #1–#4）。
- 路径安全：URL 末段、整合包清单、zip 条目名三处来源统一过 `pathsafe`，
  `PathBuf::join` 遇绝对路径整体替换这条也已被单测钉住。

### 已知残留

- `urls_file` 模式不读 `Content-Disposition`（给 N 个文件各发一次 HEAD 的账没算清）。
- 停滞会像普通失败一样进入重试，慢滴场景总耗时约为 3 倍窗口。
- 下载**中途**换对象（206 → 302 → 不同长度）未测；四种畸形 `Content-Length` 未重跑。
- `--if-range` 的「ETag 变了 ⇒ 200」目前只有手工端到端验证，未进 `FaultServer` 常驻用例。
