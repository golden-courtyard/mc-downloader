use anyhow::Result;
use clap::{Parser, Subcommand};
use mc_downloader::download::DownloadOpts;
use mc_downloader::{config, download, pack, vanilla, version};
use std::path::{Path, PathBuf};

#[derive(Parser)]
#[command(
    name = "mc-downloader",
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

    /// 同时下载的文件数（默认 16）
    #[arg(long, global = true)]
    concurrency: Option<usize>,

    /// 单文件分片时的并发连接数（默认 4）
    #[arg(long, global = true)]
    piece_conns: Option<usize>,

    /// 自适应连接数（AIMD：快则 +1，慢则减半；worker 上限为 2×piece-conns）
    #[arg(long, global = true)]
    adaptive: Option<bool>,

    /// 建连超时与单次读超时（秒，默认 30）。不是整请求总时限
    #[arg(long, global = true)]
    timeout: Option<u64>,

    #[command(subcommand)]
    cmd: Cmd,
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
    /// 下载单个 URL（验证分片用；不给 size 时先 HEAD 探测）
    Fetch {
        /// 完整 URL
        url: String,
        /// 目标目录
        #[arg(short, long, default_value = ".")]
        dir: PathBuf,
        /// 已知大小（可选）
        #[arg(short, long)]
        size: Option<u64>,
        /// 落地文件名
        #[arg(short, long, default_value = "out.bin")]
        name: String,
        /// 额外镜像 URL（可重复，内容须与 url 相同）
        #[arg(long = "mirror")]
        mirrors: Vec<String>,
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

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();

    // CLI 显式给 > 内置默认（脚本模式下，`run` 分支再叠加配置文件的值）
    let dflt = DownloadOpts::default();
    let cli = DownloadOpts {
        proxy: args.proxy.clone().filter(|p| !p.trim().is_empty()),
        retries: args.retry.unwrap_or(dflt.retries),
        concurrency: args.concurrency.unwrap_or(dflt.concurrency),
        piece_conns: args.piece_conns.unwrap_or(dflt.piece_conns),
        adaptive: args.adaptive.unwrap_or(dflt.adaptive),
        timeout_secs: args.timeout.unwrap_or(dflt.timeout_secs),
    };
    let net = cli.proxy.as_deref().unwrap_or("直连");

    match args.cmd {
        Cmd::Vanilla { version, dir } => {
            println!("[*] 目标版本 : {version}");
            println!("[*] 输出目录 : {}", dir.display());
            println!("[*] 网络     : {net}");
            vanilla::install(&version, &dir, &cli).await?;
            println!("[+] 完成。用 PCL「添加游戏目录」指向该目录即可。");
        }
        Cmd::Run { script, parallel } => {
            let cfg = config::Config::load(&script)?;

            // 优先级：CLI 显式 > 配置文件 > 内置默认
            let opts = DownloadOpts {
                proxy: cli
                    .proxy
                    .clone()
                    .or_else(|| cfg.proxy_filtered())
                    .filter(|p| !p.trim().is_empty()),
                retries: args.retry.or(cfg.network.retry).unwrap_or(dflt.retries),
                concurrency: args
                    .concurrency
                    .or(cfg.download.concurrency)
                    .unwrap_or(dflt.concurrency),
                piece_conns: args
                    .piece_conns
                    .or(cfg.download.piece_conns)
                    .unwrap_or(dflt.piece_conns),
                adaptive: args.adaptive.or(cfg.download.adaptive).unwrap_or(dflt.adaptive),
                timeout_secs: args
                    .timeout
                    .or(cfg.network.timeout_secs)
                    .unwrap_or(dflt.timeout_secs),
            };

            let total = cfg.tasks.len();
            println!("[*] 脚本: {} （{} 个任务）", script.display(), total);

            if parallel {
                // 并行时把总并发均分，避免 N × concurrency 条连接炸掉
                let per = opts.split_for(total);
                println!(
                    "[*] 并行模式：{total} 个任务，每任务并发 {}（总并发 {}）",
                    per.concurrency, opts.concurrency
                );
                let futs: Vec<_> = cfg
                    .tasks
                    .iter()
                    .map(|task| run_one_task(task, &per))
                    .collect();
                futures::future::try_join_all(futs).await?;
            } else {
                for task in cfg.tasks.iter() {
                    run_one_task(task, &opts).await?;
                }
            }
            println!("\n[+] 脚本执行完成：{total} 个任务");
        }
        Cmd::Fetch {
            url,
            dir,
            size,
            name,
            mirrors,
        } => {
            let client = download::client(&cli)?;
            let size = match size {
                Some(s) => s,
                None => {
                    let resp = client.head(&url).send().await?;
                    download::header_content_length(&resp).ok_or_else(|| {
                        anyhow::anyhow!("服务器未返回 Content-Length，请用 --size 指定")
                    })?
                }
            };
            println!("[*] URL  : {url}");
            println!("[*] 大小 : {size} bytes ({:.1} MB)", size as f64 / 1048576.0);
            println!("[*] 网络 : {net}");
            let job = version::Job {
                url,
                path: name,
                sha1: None,
                size: Some(size),
                mirrors,
            };
            download::run(&[job], &dir, &cli).await?;
            println!("[+] 完成 -> {}", dir.display());
        }
        Cmd::Pack { file, dir } => {
            println!("[*] 输出目录 : {}", dir.display());
            println!("[*] 网络     : {net}");
            pack::install(&file, &dir, &cli).await?;
        }
    }
    Ok(())
}

/// 执行单个任务（抽出来是为了能 join_all 并行）
async fn run_one_task(task: &config::TaskSpec, opts: &DownloadOpts) -> Result<()> {
    match task {
        config::TaskSpec::Mc { version, dir } => {
            println!("[mc] {version} -> {dir}");
            vanilla::install(version, Path::new(dir), opts).await?;
        }
        config::TaskSpec::Generic {
            url,
            dir,
            name,
            size,
            mirrors,
        } => {
            println!("[generic] {url}");
            let client = download::client(opts)?;
            let size = match size {
                Some(s) => *s,
                None => {
                    let resp = client.head(url).send().await?;
                    download::header_content_length(&resp).ok_or_else(|| {
                        anyhow::anyhow!("服务器未返回 Content-Length，请在脚本里显式给 size")
                    })?
                }
            };
            let name = name
                .clone()
                .unwrap_or_else(|| url.rsplit('/').next().unwrap_or("download.bin").to_string());
            let job = version::Job {
                url: url.clone(),
                path: name,
                sha1: None,
                size: Some(size),
                mirrors: mirrors.clone(),
            };
            download::run(&[job], Path::new(dir), opts).await?;
        }
        config::TaskSpec::Pack { file, dir } => {
            println!("[pack] {file} -> {dir}");
            pack::install(Path::new(file), Path::new(dir), opts).await?;
        }
    }
    Ok(())
}
