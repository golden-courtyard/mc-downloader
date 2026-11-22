//! `pack` —— 装整合包。

use std::path::PathBuf;

use anyhow::Result;

use crate::commands::Ctx;
use mc_downloader::pack;
use mc_downloader::settings::Sources;

pub async fn run(file: PathBuf, dir: PathBuf, ctx: &Ctx) -> Result<()> {
    println!("[*] 输出目录 : {}", dir.display());
    println!("[*] 网络     : {}", ctx.net);
    let src = Sources::resolve(&ctx.source, ctx.config_path.as_deref())?;
    pack::install(&file, &dir, &ctx.opts, &src).await?;
    Ok(())
}
