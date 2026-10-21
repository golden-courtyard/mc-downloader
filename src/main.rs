mod download;
mod fabric;
mod manifest;
mod modpack;
mod piece;
mod progress;
mod scheduler;
mod source;
mod vanilla;
mod version;

use anyhow::Result;
use clap::{Parser, Subcommand};
use std::path::PathBuf;

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
    #[arg(long, global = true, default_value_t = 3)]
    retry: u32,

    /// 同时下载的文件数（默认 16）
    #[arg(long, global = true, default_value_t = download::DEFAULT_CONCURRENCY)]
    concurrency: usize,

    /// 单文件分片时的并发连接数（默认 4）
    #[arg(long, global = true, default_value_t = download::DEFAULT_PIECE_CONNS)]
    piece_conns: usize,

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
    let proxy = args.proxy.as_deref();
    let retries = args.retry;
    let concurrency = args.concurrency;
    let piece_conns = args.piece_conns;

    match args.cmd {
        Cmd::Vanilla { version, dir } => {
            println!("[*] 目标版本 : {version}");
            println!("[*] 输出目录 : {}", dir.display());
            println!("[*] 网络     : {}", proxy.unwrap_or("直连"));
            vanilla::install(&version, &dir, proxy, retries, concurrency, piece_conns).await?;
            println!("[+] 完成。用 PCL「添加游戏目录」指向该目录即可。");
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
            download::run(&[job], &dir, proxy, retries, concurrency, piece_conns).await?;
            println!("[+] 完成 -> {}", dir.display());
        }
        Cmd::Pack { file, dir } => {
            println!("[*] 输出目录 : {}", dir.display());
            println!("[*] 网络     : {}", proxy.unwrap_or("直连"));
            modpack::install(&file, &dir, proxy, retries, concurrency, piece_conns).await?;
        }
    }
    Ok(())
}
