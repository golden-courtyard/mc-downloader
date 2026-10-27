//! 子命令实现。
//!
//! 每个文件只做三件事：**读参数 → 调库 → 打印**。
//! 业务逻辑（拼 Job、算 size、选源）留在 `src/` 的库里。
//!
//! 为什么从 `main.rs` 里拆出来：`main.rs` 的 `match` 分支一度膨胀到 158 行，
//! 加一个子命令就要动那个大文件；而每个分支本身是独立的、互不相干的。
//! 拆完之后 `main.rs` 只剩「clap 定义 + 构造 Ctx + 分发」。

pub mod fetch;
pub mod pack;
pub mod probe;
// run 的 189 行单独一批（含 run_one_task），搬完再启用
pub mod run;
pub mod vanilla;

use mc_downloader::download::DownloadOpts;

/// 命令执行的共用上下文。
///
/// `opts` 是真正传给下载引擎的参数；`net` 只用于终端显示（"直连" 或代理地址）。
/// **两者都来自同一份 CLI 参数**，但一个是行为、一个是文案 —— 分开存，
/// 免得每个命令各自从 `Args` 里再拼一遍。
pub struct Ctx {
    pub opts: DownloadOpts,
    pub net: String,
}
