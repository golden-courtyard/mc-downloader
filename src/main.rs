mod download;
mod fabric;
mod manifest;
mod modpack;
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
    match Args::parse().cmd {
        Cmd::Vanilla { version, dir } => {
            println!("[*] 目标版本 : {version}");
            println!("[*] 输出目录 : {}", dir.display());
            vanilla::install(&version, &dir).await?;
            println!("[+] 完成。用 PCL「添加游戏目录」指向该目录即可。");
        }
        Cmd::Pack { file, dir } => {
            println!("[*] 输出目录 : {}", dir.display());
            modpack::install(&file, &dir).await?;
        }
    }
    Ok(())
}
