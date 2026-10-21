use anyhow::Result;
use clap::{Parser, Subcommand};
use mc_downloader::{config, download, modpack, vanilla, version};
use std::path::{Path, PathBuf};

#[derive(Parser)]
#[command(
    name = "mc-downloader",
    about = "从 Mojang 官方 CDN 下载 Minecraft，或安装 Modrinth 格式整合包"
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

    // CLI 显式给 > 默认（脚本模式下由 run 分支自行读取脚本里的值）
    let proxy_owned = args.proxy.clone();
    let proxy = proxy_owned.as_deref();
    let retries = args.retry.unwrap_or(3);
    let concurrency = args.concurrency.unwrap_or(download::DEFAULT_CONCURRENCY);
    let piece_conns = args.piece_conns.unwrap_or(download::DEFAULT_PIECE_CONNS);
    let adaptive = args.adaptive.unwrap_or(false);

    match args.cmd {
        Cmd::Vanilla { version, dir } => {
            println!("[*] 目标版本 : {version}");
            println!("[*] 输出目录 : {}", dir.display());
            println!("[*] 网络     : {}", proxy.unwrap_or("直连"));
            vanilla::install(&version, &dir, proxy, retries, concurrency, piece_conns, adaptive).await?;
            println!("[+] 完成。用 PCL「添加游戏目录」指向该目录即可。");
        }
        Cmd::Run { script, parallel } => {
            let cfg = config::Config::load(&script)?;
            let proxy = cfg.proxy_filtered();
            let retries = args.retry.or(cfg.network.retry).unwrap_or(3);
            let concurrency = args
                .concurrency
                .or(cfg.download.concurrency)
                .unwrap_or(download::DEFAULT_CONCURRENCY);
            let piece_conns = args
                .piece_conns
                .or(cfg.download.piece_conns)
                .unwrap_or(download::DEFAULT_PIECE_CONNS);
            let adaptive = args.adaptive.or(cfg.download.adaptive).unwrap_or(false);

            let total = cfg.tasks.len();
            println!("[*] 脚本: {} （{} 个任务）", script.display(), total);

            // 并行时把总并发均分，避免 N × concurrency 条连接炸掉
            let n = cfg.tasks.len();
            let per_task = if parallel {
                (concurrency / n).max(1)
            } else {
                concurrency
            };
            if parallel {
                println!(
                    "[*] 并行模式：{n} 个任务，每任务并发 {}（总并发 {}）",
                    per_task, concurrency
                );
            }

            if parallel {
                // 任务级并行：总并发已均分，各任务互不争抢
                let futs: Vec<_> = cfg
                    .tasks
                    .iter()
                    .map(|task| {
                        run_one_task(
                            task,
                            proxy.as_deref(),
                            retries,
                            per_task,
                            piece_conns,
                            adaptive,
                        )
                    })
                    .collect();
                futures::future::try_join_all(futs).await?;
            } else {
                for task in cfg.tasks.iter() {
                    run_one_task(
                        task,
                        proxy.as_deref(),
                        retries,
                        concurrency,
                        piece_conns,
                        adaptive,
                    )
                    .await?;
                }
            }
            println!("\n[+] 脚本执行完成：{} 个任务", total);
        }
        Cmd::Fetch { url, dir, size, name, mirrors } => {
            let client = download::client(proxy)?;
            let size = match size {
                Some(s) => s,
                None => client
                    .head(&url)
                    .send()
                    .await?
                    .content_length()
                    .ok_or_else(|| anyhow::anyhow!("服务器未返回 Content-Length，请用 --size 指定"))?,
            };
            println!("[*] URL  : {url}");
            println!("[*] 大小 : {size} bytes ({:.1} MB)", size as f64 / 1048576.0);
            println!("[*] 网络 : {}", proxy.unwrap_or("直连"));
            let job = version::Job {
                url,
                path: name,
                sha1: None,
                size: Some(size),
                mirrors: mirrors.clone(),
            };
            download::run(&[job], &dir, proxy, retries, concurrency, piece_conns, adaptive).await?;
            println!("[+] 完成 -> {}", dir.display());
        }
        Cmd::Pack { file, dir } => {
            println!("[*] 输出目录 : {}", dir.display());
            println!("[*] 网络     : {}", proxy.unwrap_or("直连"));
            modpack::install(&file, &dir, proxy, retries, concurrency, piece_conns, adaptive).await?;
        }
    }
    Ok(())
}

/// 执行单个任务（抽出来是为了能 join_all 并行）
async fn run_one_task(
    task: &config::TaskSpec,
    proxy: Option<&str>,
    retries: u32,
    concurrency: usize,
    piece_conns: usize,
    adaptive: bool,
) -> Result<()> {
    match task {
        config::TaskSpec::Mc { version, dir } => {
            println!("[mc] {version} -> {dir}");
            vanilla::install(
                version,
                Path::new(dir),
                proxy,
                retries,
                concurrency,
                piece_conns,
                adaptive,
            )
            .await?;
        }
        config::TaskSpec::Generic {
            url,
            dir,
            name,
            size,
            mirrors,
        } => {
            println!("[generic] {url}");
            let client = download::client(proxy)?;
            let size = match size {
                Some(s) => *s,
                None => client
                    .head(url)
                    .send()
                    .await?
                    .content_length()
                    .ok_or_else(|| {
                        anyhow::anyhow!("服务器未返回 Content-Length，请在脚本里显式给 size")
                    })?,
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
            download::run(
                &[job],
                Path::new(dir),
                proxy,
                retries,
                concurrency,
                piece_conns,
                adaptive,
            )
            .await?;
        }
        config::TaskSpec::Pack { file, dir } => {
            println!("[pack] {file} -> {dir}");
            modpack::install(
                Path::new(file),
                Path::new(dir),
                proxy,
                retries,
                concurrency,
                piece_conns,
                adaptive,
            )
            .await?;
        }
    }
    Ok(())
}
