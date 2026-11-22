# Agent Note: 源由用户自备、落地名认 Content-Disposition、脚本 v2、If-Range 与停滞检测

Status: implemented

## Problem

2026-11-23 一批 issue 指向同一个模式的四个面：**工具替用户做了本不该替它做的决定**。

1. **#15 内置下载源**：2026-10-20 PCL2 在 v2.99.0.0 移除了 MC 下载功能（更新日志原文：
   「为遵守相关法律法规」）。原因不是被举报，而是「第三方工具内置下载功能客观上存在绕过官方正版验证的可能」。
   本工具与它相邻：`src/` 里写死了 11 处源地址。这既是合规问题，也让"换镜像"必须改代码。
2. **#16 落地名**：我们**完全不读** `Content-Disposition`，落点只从 URL 末段推。于是浏览器落
   `报告-v2.zip`、我们落 `download`/`out.bin`；而同名会让第二次下载被当成「已存在」跳过（sha1 为空时尤其）。
3. **脚本没有版本与局部覆盖**：改结构时老脚本只会得到"某个字段静默不生效"；一次运行里想给某个大文件
   单独放松重试，只能改全局。
4. **#7B 远端中途被换**：只能等最后 SHA1 失败才发现，且错误不说差多少。
5. **#10 slowloris**：`read_timeout` 防"卡住"不防"慢慢喂"（实测挂 7 分钟以上）。

## Decision

- **源全部交给用户**：`[source]` 六项（`metadata`/`assets`/`fabric`/`forge_meta`/`forge_maven`/`neoforge`）
  全必填，缺哪项报哪项并给出"只想下单个文件就用 `fetch`"的出路；代码里零兜底、零探测；
  CI 用 `grep` 守（`src/` 里出现任一源地址即红）。Forge 的库 host 白名单**由配置推出**，
  空白名单 = 什么都不放行。
- **落地名优先级**：`--name` > `Content-Disposition` > URL 末段 > `out.bin`。
  只在没给 `--name` 时才为它发一次 HEAD；`filename` 走 `pathsafe::check_name`（服务器给的名字同样不可信）；
  只认 UTF-8 的 `filename*=` 与 ASCII `filename`，其它 charset 放弃；`--no-content-disposition` 可关。
- **脚本 v2**：`schema = 1`（省略按 1，其它值报"太新/太旧"）；三种 `kind` 都可写
  `retries`/`timeout_secs`/`max_speed`，启动期校验；`continue_on_error` 放在**脚本级**
  （"要不要继续"是这一次运行的属性，不是某个任务的属性），为 true 时末尾汇总并以非零退出。
- **If-Range**：`--if-range` 默认关；验证器**按源**缓存；只认强 ETag，弱 ETag 退 `Last-Modified`，
  都没有就不发这个头。命中失配 ⇒ 服务器回 200 ⇒ 既有降级路径重下整份。
- **停滞检测**：窗口 `max(10s, 2×timeout)` 内推进不足 4 KiB ⇒ 报「传输停滞」。不引入总时限。

## Alternatives considered

- **保留"官方源"作为内置默认，只砍第三方镜像**：否。issue #15 的判据明确是"官方或第三方都不内置"，
  因为合规争点不在镜像而在"提供获取通道"本身；而且留一个默认值就等于留一处 `unwrap_or`。
- **`[source]` 放进下载脚本而不是用户配置**：否。源是"这台机器上有什么"，不是"这一次干什么"；
  放脚本里会导致每个脚本都要抄一遍源，且 `vanilla`（无脚本）无处可读。
- **Content-Disposition 一律优先于 `--name`**（"服务器更准"）：否。显式参数永远最高，
  否则用户没法覆盖一个胡说八道的服务器。**也不默认给所有扩展名都读**：只在没给名字时读一次 HEAD。
- **`continue_on_error` 做成任务级**：否。会变成"第 2 个失败时，第 3 个怎么办取决于第 2 个自己的配置"。
- **`If-Range` 默认开**：否。多镜像各自算 ETag，拿 A 的去请求 B 必不匹配 ⇒ 回 200 ⇒ 把
  "能用但慢"的镜像判死。默认开等于默认惩罚多源用户。
- **用 `ClientBuilder::timeout` 治 slowloris**：否。那是整请求总时限，3 GB 文件在慢链路上必然被掐死
  （Cargo.toml 与 `docs/design/archive/2026-10-task-book-v0.4.md` 都有这条取舍）。停滞检测只看"还在不在动"。

## Consequences

**变好的**：

- 合规争点从"我们提供通道"变成"用户自备通道"；换镜像 = 改配置，不改代码。
- 同一个 URL 在 `fetch` 与 `run` 下**落同一个名字**（`url_list::job_from_url` 是唯一推导口，
  此前 `run` 自己写了一份不剥 query / 不 percent-decode 的实现）。
- 远端中途被换 ⇒ 416 直接报「远端当前 N 字节，我们按 M 字节在切」；开了 `--if-range` 则当场降级重下。
- 慢滴服务器不再能挂死进程（实测 10.0 s 内失败），而 4 MiB 的正常下载不受影响。

**代价 / 已知残留**：

- **`urls_file` 模式不读 `Content-Disposition`** —— 给 N 个文件各发一次 HEAD 会让请求数翻倍，
  这笔账没算清之前不做。
- **停滞会走普通重试**（默认 3 次）⇒ 慢滴场景总耗时 ≈ 3×窗口。要标成"不可重试"需要新的标记类型，
  与 `RangeUnsupported`/`SourcesExhausted` 同一套机制，留给下一轮。
- **#10 的两条没测完**：下载**中途**换对象（206→302→不同长度）；四种畸形 `Content-Length` 那组重跑。
- `--if-range` 的"ETag 变了 ⇒ 200"目前只有手工 E2E（本地假服务器），还没进 `FaultServer` 常驻用例。
