//! `fetch` —— 下载单个 URL 到目录。

use std::path::PathBuf;

use anyhow::Result;

use crate::commands::Ctx;
use mc_downloader::{download, version};

/// 下载单个 URL。
///
/// 长度只用来决定「分片还是单流」，**不是下载的前提**：
/// 拿不到（chunked 传输、HEAD 不给长度）=> `size = None` => 走 `one_stream` 流式写到 EOF。
pub async fn run(
    url: String,
    dir: PathBuf,
    size: Option<u64>,
    name: String,
    mirrors: Vec<String>,
    ctx: &Ctx,
) -> Result<()> {
    let client = download::client(&ctx.opts)?;
    let size = match size {
        Some(s) => Some(s),
        None => {
            let resp = client.head(&url).send().await?;
            download::header_content_length(&resp)
        }
    };
    println!("[*] URL  : {url}");
    match size {
        Some(n) => println!("[*] 大小 : {n} bytes ({:.1} MB)", n as f64 / 1048576.0),
        None => {
            println!("[*] 大小 : 未知（服务器未给 Content-Length，如 chunked）=> 单流下载，不分片")
        }
    }
    println!("[*] 网络 : {}", ctx.net);
    let job = version::Job {
        url,
        path: name,
        sha1: None,
        size,
        mirrors,
    };
    download::run(&[job], &dir, &ctx.opts).await?;
    println!("[+] 完成 -> {}", dir.display());
    Ok(())
}
