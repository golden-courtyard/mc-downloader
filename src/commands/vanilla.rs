//! `vanilla` —— 只装原版。

use std::path::PathBuf;

use anyhow::Result;

use crate::commands::Ctx;
use mc_downloader::settings::Sources;
use mc_downloader::vanilla;

pub async fn run(version: String, dir: PathBuf, ctx: &Ctx) -> Result<()> {
    println!("[*] 目标版本 : {version}");
    println!("[*] 输出目录 : {}", dir.display());
    println!("[*] 网络     : {}", ctx.net);
    let src = Sources::resolve(&ctx.source, ctx.config_path.as_deref())?;
    vanilla::install(&version, &dir, &ctx.opts, &src).await?;
    println!("[+] 完成。用 PCL「添加游戏目录」指向该目录即可。");
    Ok(())
}
