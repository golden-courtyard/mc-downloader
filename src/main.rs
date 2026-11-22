mod commands;

use anyhow::Result;
use clap::{Parser, Subcommand};
use mc_downloader::download::DownloadOpts;
use mc_downloader::{config, progress, settings};
use std::path::PathBuf;

#[derive(Parser)]
#[command(
    name = "mc-downloader",
    version,
    about = "从 Mojang 官方 CDN 下载 Minecraft，或安装整合包"
)]
struct Args {
    /// 代理地址，例如 http://127.0.0.1:7897
    ///
    /// 默认**直连**：Mojang CDN / Fabric meta / Modrinth CDN 在国内均直连可达。
    /// 只有直连拿不到（超时 / 000）时才加这个参数。
    #[arg(long, global = true)]
    proxy: Option<String>,

    /// 单个文件失败后的重试次数（指数退避 1s/2s/4s…）
    #[arg(long, global = true)]
    retry: Option<u32>,

    /// 同时下载的文件数（默认 16；别名 --jobs）
    #[arg(long, global = true, visible_alias = "jobs")]
    concurrency: Option<usize>,

    /// 单文件分片时的并发连接数（默认 4；别名 --conns）
    #[arg(long, global = true, visible_alias = "conns")]
    piece_conns: Option<usize>,

    /// 自适应连接数（AIMD：快则 +1，慢则减半；worker 上限为 2×piece-conns）
    #[arg(long, global = true)]
    adaptive: Option<bool>,

    /// 建连超时与单次读超时（秒，默认 30）。不是整请求总时限
    #[arg(long, global = true)]
    timeout: Option<u64>,

    /// 全局总带宽上限，如 `5M` / `500K`（1024 进制；纯数字 = 字节/秒）
    #[arg(long, global = true)]
    max_speed: Option<String>,

    /// 用户级配置文件路径（默认查平台配置目录；用 --print-config 看它读的是哪个）
    #[arg(long, global = true, value_name = "PATH")]
    config: Option<PathBuf>,

    /// 打印最终生效的配置（含每一项来自哪一层），然后退出
    #[arg(long, global = true)]
    print_config: bool,

    /// 不给可执行文件打 Mark-of-the-Web（默认会打，见 docs/usage.md）
    #[arg(long = "no-motw", global = true)]
    no_motw: bool,

    /// 子命令（`--print-config` 时可以不写）
    #[command(subcommand)]
    cmd: Option<Cmd>,
}

#[derive(Subcommand)]
enum Cmd {
    /// 下载原版（client jar + libraries + assets）
    Vanilla {
        /// 版本 id，例如 1.20.1 / 26.3
        version: String,
        /// 目标 .minecraft 目录
        #[arg(short, long, default_value = ".")]
        dir: PathBuf,
    },
    /// 按下载脚本执行（脚本里可以有多个 [[task]]）
    Run {
        /// 脚本路径（TOML）
        script: PathBuf,
        /// 任务并行执行（默认串行）
        #[arg(long)]
        parallel: bool,
    },
    /// 下载单个 URL 到目录（分片 + 续传 + 重试 + 限速；长度未知时自动单流）
    ///
    /// 这是「下一个文件」的默认入口，比裸 curl 多做四件事：大文件自动分片并行、
    /// 断点续传、失败指数退避重试、可限速。服务器不给 Content-Length（如 chunked）时
    /// 不再报错，自动退化为单流下到 EOF（只是没法分片，会慢一些）。
    Fetch {
        /// 完整 URL
        url: String,
        /// 目标目录
        #[arg(short, long, default_value = ".")]
        dir: PathBuf,
        /// 已知大小（可选）—— **只影响"分片还是单流"，不影响能否下载**
        ///
        /// 不给时会先发 HEAD 探测；**探测不到（如服务器用 `Transfer-Encoding: chunked`）就走单流**，
        /// 照常下到 EOF，只是没法分片（大文件会慢一些）。
        #[arg(short, long)]
        size: Option<u64>,
        /// 落地文件名（**默认从 URL 末段推导**，推不出时用 `out.bin`）
        ///
        /// 不给时取 URL 的最后一段（去掉 `?query` / `#fragment`、percent-decode）。
        /// 推不出（以 `/` 结尾、只有主机名）时才回落 `out.bin` 并提示。
        /// **别让默认值变成常量** —— 同目录连下多个文件时，`out.bin` 会互相覆盖。
        #[arg(short, long)]
        name: Option<String>,
        /// 目标已存在时覆盖
        ///
        /// **默认拒绝**：同名文件存在时直接报错退出，而不是覆盖或跳过。
        /// 跳过会让人以为"下好了"（sha1 为空时它就是这么判的），覆盖会丢上一个文件 ——
        /// 两种都不该是默认行为。
        #[arg(long)]
        force: bool,
        /// 额外镜像 URL（可重复，内容须与 url 相同）
        #[arg(long = "mirror")]
        mirrors: Vec<String>,
    },
    /// 探测多个 URL 的真实下载速度（串行各测 N 秒），用来决定 mirrors 填哪个
    ///
    /// **为什么不并发测**：并发会互相抢带宽，测出来的排名是假的。
    /// 串行慢一点，但每个数都是真的。
    Probe {
        /// 要对比的 URL（至少两个才有意义）
        urls: Vec<String>,
        /// 每个 URL 测几秒
        #[arg(long, default_value = "5")]
        seconds: u64,
    },
    /// 安装整合包（.mrpack，或结构相同的 .zip）
    Pack {
        /// 整合包文件路径
        file: PathBuf,
        /// 目标 .minecraft 目录
        #[arg(short, long, default_value = ".")]
        dir: PathBuf,
    },
}

/// 探测单个 URL：读 `secs` 秒，返回读到的字节数。
///

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();

    // CLI 显式给 > 脚本（仅 run）> 用户配置 > 内置默认
    let dflt = DownloadOpts::default();

    // 用户级配置（持久默认值）。**不读 cwd**，搜索链见 `settings::locate`。
    let loaded = settings::load(args.config.as_deref())?;

    // 把 CLI 也表达成同一个 `Settings`，三层用同一套合并逻辑
    let cli_layer = settings::Settings {
        proxy: args.proxy.clone(),
        retries: args.retry,
        concurrency: args.concurrency,
        piece_conns: args.piece_conns,
        adaptive: args.adaptive,
        timeout_secs: args.timeout,
        max_speed: args.max_speed.clone(),
        motw: args.no_motw.then_some(false),
        // 命令行与脚本都不承载源：源属于"这台机器上有什么"（issue #15）
        source: settings::SourceSettings::default(),
    };
    let merged = settings::merge(&[("cli", &cli_layer), ("config", &loaded.settings)], &dflt);
    if args.print_config {
        let src = match &loaded.path {
            Some(p) => p.display().to_string(),
            None => "（没有配置文件，全走默认）".to_string(),
        };
        println!("[*] 配置文件: {src}");
        println!("{:<14} {:<24} 来源", "键", "生效值");
        for (k, v, s) in &merged.rows {
            println!("{k:<14} {v:<24} {s}");
        }
        return Ok(());
    }

    // 限速：非法写法要在启动时立刻报错，不能静默当成"不限速"（两层都要查）
    for s in [
        args.max_speed.as_deref(),
        loaded.settings.max_speed.as_deref(),
    ]
    .into_iter()
    .flatten()
    {
        progress::parse_speed(s).ok_or_else(|| {
            anyhow::anyhow!(
                "--max-speed / 配置里的 max_speed 解析失败：`{s}`（例：5M / 500K / 1048576）"
            )
        })?;
    }
    let net = merged.opts.proxy.as_deref().unwrap_or("直连");
    // 给拆出去的子命令用：opts 是行为、net 是文案（同一个来源，分开存）
    let ctx = commands::Ctx {
        opts: merged.opts.clone(),
        net: net.to_string(),
    };

    let Some(cmd) = args.cmd else {
        anyhow::bail!(
            "需要子命令（vanilla / run / fetch / probe / pack）；`--help` 看用法，`--print-config` 看配置"
        );
    };
    match cmd {
        Cmd::Vanilla { version, dir } => {
            commands::vanilla::run(version, dir, &ctx).await?;
        }
        Cmd::Run { script, parallel } => {
            let cfg = config::Config::load(&script)?;

            // 优先级：CLI 显式 > 脚本 > 用户配置 > 内置默认
            let script_layer = settings::Settings {
                proxy: cfg.proxy_filtered(),
                retries: cfg.network.retry,
                concurrency: cfg.download.concurrency,
                piece_conns: cfg.download.piece_conns,
                adaptive: cfg.download.adaptive,
                timeout_secs: cfg.network.timeout_secs,
                max_speed: cfg.download.max_speed.clone(),
                motw: None,
                source: settings::SourceSettings::default(),
            };
            let mut opts = settings::merge(
                &[
                    ("cli", &cli_layer),
                    ("script", &script_layer),
                    ("config", &loaded.settings),
                ],
                &dflt,
            )
            .opts;
            // 脚本不暴露这个开关：它是 main.rs 按任务类型决定的
            // （urls_file 批量任务在下面单点覆盖成 true）
            opts.continue_on_error = false;

            let total = cfg.tasks.len();
            println!("[*] 脚本: {} （{} 个任务）", script.display(), total);
            commands::run::execute(&cfg.tasks, parallel, &opts).await?;
        }
        Cmd::Fetch {
            url,
            dir,
            size,
            name,
            force,
            mirrors,
        } => {
            commands::fetch::run(url, dir, size, name, force, mirrors, &ctx).await?;
        }
        Cmd::Probe { urls, seconds } => {
            commands::probe::run(urls, seconds, &ctx).await?;
        }
        Cmd::Pack { file, dir } => {
            commands::pack::run(file, dir, &ctx).await?;
        }
    }
    Ok(())
}
