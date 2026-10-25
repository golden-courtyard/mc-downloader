use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use futures::StreamExt;
use mc_downloader::download::DownloadOpts;
use mc_downloader::{config, download, fabric, forge, pack, progress, url_list, vanilla, version};
use std::path::{Path, PathBuf};

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

    /// 全局总带宽上限，如 `5M` / `500K`（1024 进制；纯数字 = 字节/秒）
    #[arg(long, global = true)]
    max_speed: Option<String>,

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
/// **不要求服务器支持 Range** —— 直接 GET 读一段时间就断，最简单也最通用。
/// 多给 5 秒余量给建连（慢源建连可能就花掉 2-3 秒）。
async fn probe_one(client: &reqwest::Client, url: &str, secs: u64) -> Result<u64> {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(secs);
    let fut = async {
        let resp = client.get(url).send().await?.error_for_status()?;
        let mut stream = resp.bytes_stream();
        let mut total = 0u64;
        while let Some(c) = stream.next().await {
            total += c?.len() as u64;
            if std::time::Instant::now() >= deadline {
                break;
            }
        }
        Ok::<u64, anyhow::Error>(total)
    };
    match tokio::time::timeout(std::time::Duration::from_secs(secs + 5), fut).await {
        Ok(r) => r,
        Err(_) => anyhow::bail!("探测超时（{secs}s + 5s 余量）"),
    }
}

fn rate_str(bytes: u64, secs: u64) -> String {
    format!("{}/s", download::human_rate(bytes / secs.max(1)))
}

fn truncate(s: &str, n: usize) -> String {
    if s.chars().count() <= n {
        s.to_string()
    } else {
        format!("{}…", s.chars().take(n).collect::<String>())
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();

    // CLI 显式给 > 内置默认（脚本模式下，`run` 分支再叠加配置文件的值）
    let dflt = DownloadOpts::default();

    // 限速：CLI 的非法写法要在启动时立刻报错，不能静默当成"不限速"
    let cli_max_speed = match args.max_speed.as_deref() {
        Some(s) => Some(progress::parse_speed(s).ok_or_else(|| {
            anyhow::anyhow!("--max-speed 解析失败：`{s}`（例：5M / 500K / 1048576）")
        })?),
        None => None,
    };
    let cli = DownloadOpts {
        proxy: args.proxy.clone().filter(|p| !p.trim().is_empty()),
        retries: args.retry.unwrap_or(dflt.retries),
        concurrency: args.concurrency.unwrap_or(dflt.concurrency),
        piece_conns: args.piece_conns.unwrap_or(dflt.piece_conns),
        adaptive: args.adaptive.unwrap_or(dflt.adaptive),
        timeout_secs: args.timeout.unwrap_or(dflt.timeout_secs),
        continue_on_error: false,
        max_speed: cli_max_speed,
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
                adaptive: args
                    .adaptive
                    .or(cfg.download.adaptive)
                    .unwrap_or(dflt.adaptive),
                timeout_secs: args
                    .timeout
                    .or(cfg.network.timeout_secs)
                    .unwrap_or(dflt.timeout_secs),
                // 配置文件不暴露这个开关：它是 main.rs 按任务类型决定的
                // （urls_file 批量任务在下面单点覆盖成 true）
                continue_on_error: false,
                // 优先级：CLI > 配置文件；两边都没有 = 不限速
                max_speed: match cli.max_speed {
                    Some(v) => Some(v),
                    None => match cfg.download.max_speed.as_deref() {
                        Some(s) => Some(progress::parse_speed(s).ok_or_else(|| {
                            anyhow::anyhow!(
                                "配置里的 download.max_speed 解析失败：`{s}`\n                                   写法：5M / 500K / 纯数字（字节/秒）"
                            )
                        })?),
                        None => None,
                    },
                },
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
            println!(
                "[*] 大小 : {size} bytes ({:.1} MB)",
                size as f64 / 1048576.0
            );
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
        Cmd::Probe { urls, seconds } => {
            if urls.len() < 2 {
                anyhow::bail!(
                    "probe 至少给两个 URL 才有意义（现在 {} 个）\n  \
                     用法：mc-downloader probe <url1> <url2> [--seconds 5]",
                    urls.len()
                );
            }
            let secs = seconds.clamp(2, 60);
            println!("[*] 探测 {secs} 秒/个（串行，避免互相抢带宽）");
            let client = download::client(&cli)?;
            let mut results: Vec<(String, Option<u64>)> = Vec::new();
            for (i, u) in urls.iter().enumerate() {
                print!("  [{}/{}] {} ... ", i + 1, urls.len(), truncate(u, 58));
                match probe_one(&client, u, secs).await {
                    Ok(n) => {
                        println!("{}", rate_str(n, secs));
                        results.push((u.clone(), Some(n)));
                    }
                    Err(e) => {
                        println!("失败: {e}");
                        results.push((u.clone(), None));
                    }
                }
            }
            // 按吞吐降序，失败的排最后
            results.sort_by(|a, b| b.1.unwrap_or(0).cmp(&a.1.unwrap_or(0)));
            println!("\n[*] 排名:");
            for (i, (u, n)) in results.iter().enumerate() {
                let r = match n {
                    Some(v) => rate_str(*v, secs),
                    None => "失败".to_string(),
                };
                println!("  {}. {:>12}  {u}", i + 1, r);
            }
            println!(
                "\n[i] 把快的填进 `mirrors = [\"...\"]`；下载时 SourcePool 会按实测吞吐分配分片"
            );
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
        config::TaskSpec::Mc {
            version,
            dir,
            loader,
        } => {
            println!("[mc] {version} -> {dir}");
            let root = Path::new(dir);
            vanilla::install(version, root, opts).await?;

            // 加载器：复用 fabric.rs，不在这里重写安装逻辑
            if let Some(spec) = loader.as_deref() {
                let (kind, want_ver) = match spec.split_once(':') {
                    Some((k, v)) => (k.trim(), Some(v.trim().to_string())),
                    None => (spec.trim(), None),
                };
                let id = match kind {
                    "fabric" => {
                        let lv = match want_ver {
                            Some(v) if !v.is_empty() => v,
                            _ => fabric::latest_stable_loader(version, opts).await?,
                        };
                        fabric::install(version, &lv, root, opts).await?
                    }
                    // Forge：下载归我们（install_profile 的 libraries），构建归官方 installer（--offline）
                    "forge" => {
                        let fv = match want_ver {
                            Some(v) if !v.is_empty() => v,
                            _ => forge::latest_forge(version, opts).await?,
                        };
                        forge::install(version, &fv, root, opts).await?
                    }
                    other => anyhow::bail!(
                        "未知的 loader: \"{other}\"（支持：fabric / forge；NeoForge 尚未实现）"
                    ),
                };
                println!("[+] 加载器装好 -> versions/{id}/");
                println!("[i] 用启动器选 {id} 即可（本工具只保证目录结构，未验证能否启动）");
            }
        }
        config::TaskSpec::Generic {
            url,
            urls_file,
            dir,
            name,
            size,
            mirrors,
        } => {
            let root = Path::new(dir);
            // 配置层已校验过「url / urls_file 二选一」，这里按存在哪个走
            if let Some(list_path) = urls_file {
                let content = tokio::fs::read_to_string(list_path)
                    .await
                    .with_context(|| format!("读列表文件失败: {list_path}"))?;
                let parsed = url_list::parse(&content)
                    .with_context(|| format!("解析列表文件失败: {list_path}"))?;
                if parsed.skipped > 0 {
                    println!(
                        "[!] 跳过了 {} 行非 URL 内容（aria2 的 dir=/out= 之类选项行）",
                        parsed.skipped
                    );
                }
                println!(
                    "[generic:list] {} 个 URL <- {list_path}",
                    parsed.entries.len()
                );
                // 一个进程内并发：连接池 / DNS / TLS 只建一次（这是 urls_file 相对
                // shell 循环的根本优势）
                let jobs: Vec<version::Job> = parsed
                    .entries
                    .into_iter()
                    .map(|e| version::Job {
                        url: e.url,
                        path: e.file_name,
                        sha1: None,
                        size: None, // 未知 ⇒ 走单流；不为此多发 N 次 HEAD
                        mirrors: Vec::new(),
                    })
                    .collect();
                // 批量独立文件：一个挂了不该拖累其余
                let mut batch = opts.clone();
                batch.continue_on_error = true;
                download::run(&jobs, root, &batch).await?;
            } else {
                let url = url.as_ref().expect("校验已保证 url 或 urls_file 有一个");
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
                let name = name.clone().unwrap_or_else(|| {
                    url.rsplit('/').next().unwrap_or("download.bin").to_string()
                });
                let job = version::Job {
                    url: url.clone(),
                    path: name,
                    sha1: None,
                    size: Some(size),
                    mirrors: mirrors.clone(),
                };
                download::run(&[job], root, opts).await?;
            }
        }
        config::TaskSpec::Pack { file, dir } => {
            println!("[pack] {file} -> {dir}");
            pack::install(Path::new(file), Path::new(dir), opts).await?;
        }
    }
    Ok(())
}
