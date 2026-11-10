//! `fetch` —— 下载单个 URL 到目录。

use std::path::PathBuf;

use anyhow::{Result, bail};

use crate::commands::Ctx;
use mc_downloader::{download, url_list, version};

/// 下载单个 URL。
///
/// 长度只用来决定「分片还是单流」，**不是下载的前提**：
/// 拿不到（chunked 传输、HEAD 不给长度）=> `size = None` => 走 `one_stream` 流式写到 EOF。
///
/// **落地名与覆盖策略**（issue #1）：
///
/// - 不给 `--name` 时从 URL 末段推导 —— 规则在 `url_list::filename_from_url`（与 `urls_file` 共用一份）
/// - 推不出（以 `/` 结尾、只有主机名）才回落 `out.bin`，而且要在输出里**说明**，不静默
/// - 目标已存在时默认**拒绝**：`sha1` 为空时 `download::one` 会把"同名已存在"当成
///   "已完成"直接跳过，用户会以为下好了 —— 跳过与覆盖两种都不该是默认行为
pub async fn run(
    url: String,
    dir: PathBuf,
    size: Option<u64>,
    name: Option<String>,
    force: bool,
    mirrors: Vec<String>,
    ctx: &Ctx,
) -> Result<()> {
    let (name, fell_back) = match name {
        Some(n) => (n, false),
        None => url_list::name_or_out_bin(&url),
    };
    if fell_back {
        eprintln!("[!] 这个 URL 推不出文件名（以 `/` 结尾，或只有主机名）");
        eprintln!("[!] 落地名回落为 out.bin —— 想指定名字请加 --name");
    }

    let dst = dir.join(&name);
    if dst.exists() {
        if !force {
            bail!(
                "目标已存在：{}\n\
                 \x20 默认不覆盖 —— sha1 为空时它还会被当成「已完成」直接跳过，容易误判成下好了。\n\
                 \x20 要覆盖请加 --force；要换个名字请用 --name <名字>",
                dst.display()
            );
        }
        tokio::fs::remove_file(&dst).await?;
    }

    let client = download::client(&ctx.opts)?;
    let size = match size {
        Some(s) => Some(s),
        None => {
            let resp = client.head(&url).send().await?;
            download::header_content_length(&resp)
        }
    };
    println!("[*] URL  : {url}");
    println!("[*] 落地 : {}", dst.display());
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
    println!("[+] 完成 -> {}", dst.display());
    Ok(())
}
