//! `fetch` —— 下载单个 URL 到目录。

use std::path::PathBuf;

use anyhow::{Context, Result, bail};

use crate::commands::Ctx;
use mc_downloader::version::SizeHint;
use mc_downloader::{download, url_list};

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
///
/// `fetch` 的全部入参。收成结构体是因为 clippy 的 `too_many_arguments` 已经报警（8/7）。
///
/// 收成结构体是因为 clippy 的 `too_many_arguments` 已经报警（8/7），**不是**为了好看 ——
/// 再加一个开关时这层不会继续膨胀。
pub struct FetchArgs {
    pub url: String,
    pub dir: PathBuf,
    pub size: Option<u64>,
    pub name: Option<String>,
    pub force: bool,
    pub mirrors: Vec<String>,
    pub no_content_disposition: bool,
}

pub async fn run(a: FetchArgs, ctx: &Ctx) -> Result<()> {
    let FetchArgs {
        url,
        dir,
        size,
        name,
        force,
        mirrors,
        no_content_disposition,
    } = a;
    let client = download::client(&ctx.opts)?;

    // 名字优先级（issue #16）：`--name` > `Content-Disposition` > URL 末段 > `out.bin`
    //
    // 为什么要多发一次 HEAD：`Content-Disposition` 是"服务器决定文件名"的标准手段，
    // 而我们此前**完全不读**它。只在**用户没给 `--name`** 时才去问这一次（给了名字就没必要），
    // 而 `--size` 也没给时本来就要发 HEAD —— 两件事共用一个响应。
    let mut cd_name: Option<String> = None;
    let mut head_size: Option<u64> = None;
    if name.is_none() && (!no_content_disposition || size.is_none()) {
        match client.head(&url).send().await {
            Ok(resp) => {
                if size.is_none() {
                    head_size = download::header_content_length(&resp);
                }
                if !no_content_disposition {
                    cd_name = resp
                        .headers()
                        .get(reqwest::header::CONTENT_DISPOSITION)
                        .and_then(|v| v.to_str().ok())
                        .and_then(mc_downloader::httpname::filename_from_content_disposition);
                }
            }
            // HEAD 不支持（405/501）或网络抖动都不该让下载失败：回落 URL 推导
            Err(e) => eprintln!("[!] HEAD 探测失败（{e}），落地名按 URL 推导"),
        }
    }
    let size_from_url = size.or(head_size);

    // 名字推导只有一份实现（lib 的 `job_from_url`），`run` 也走它
    let (job, fell_back) = url_list::job_from_url(
        &url,
        name.as_deref().or(cd_name.as_deref()),
        size_from_url.map_or(SizeHint::Unknown, SizeHint::Hint),
        mirrors.clone(),
    );
    let name = job.path.clone();
    if cd_name.is_some() && name == *cd_name.as_deref().unwrap_or("") {
        println!("[*] 落地名取自 Content-Disposition（服务器给的；--name 可以覆盖它）");
    }
    if fell_back {
        eprintln!("[!] 这个 URL 推不出文件名（以 `/` 结尾，或只有主机名）");
        eprintln!("[!] 落地名回落为 out.bin —— 想指定名字请加 --name");
    }

    // 落点必须在 `--dir` 之下，而且要在**碰文件系统之前**判：下面有 `--force` 的删除逻辑，
    // 先 join 再删就等于「拿一个没校验过的路径去删文件」——
    // 红队实测 `--force -n "..\victim.txt"` 会删掉 dir 之外的文件（2026-11-22）。
    let dst = resolve_dst(&dir, &name)?;
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

    let size = size_from_url;
    println!("[*] URL  : {url}");
    println!("[*] 落地 : {}", dst.display());
    match size {
        Some(n) => println!("[*] 大小 : {n} bytes ({:.1} MB)", n as f64 / 1048576.0),
        None => {
            println!("[*] 大小 : 未知（服务器未给 Content-Length，如 chunked）=> 单流下载，不分片")
        }
    }
    println!("[*] 网络 : {}", ctx.net);
    download::run(&[job], &dir, &ctx.opts).await?;
    println!("[+] 完成 -> {}", dst.display());
    Ok(())
}

/// 把「名字」变成一个**校验过**的落点。
///
/// 单独提出来是为了：① 调用点必须在 `--force` 的删除**之前**用它；② 它能被单测覆盖
/// （`commands` 住在 bin target 里，集成测试摸不到）。
fn resolve_dst(dir: &std::path::Path, name: &str) -> Result<PathBuf> {
    mc_downloader::pathsafe::under_root(dir, std::path::Path::new(name))
        .with_context(|| format!("落地路径不安全，拒绝下载: {name}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_dst_accepts_plain_and_subdir_names() {
        let dir = std::env::temp_dir().join("mcd-fetch-name");
        assert!(resolve_dst(&dir, "a.bin").unwrap().ends_with("a.bin"));
        // `--name sub/x.bin` 是既有用法，不能因为加校验就砍掉
        assert!(
            resolve_dst(&dir, "sub/x.bin")
                .unwrap()
                .ends_with("sub/x.bin")
        );
    }

    #[test]
    fn resolve_dst_rejects_traversal_before_touching_fs() {
        let dir = std::env::temp_dir().join("mcd-fetch-name2");
        for bad in ["../victim.txt", "..\\victim.txt", "/etc/passwd"] {
            let e = resolve_dst(&dir, bad).unwrap_err();
            assert!(
                format!("{e:#}").contains("落地路径不安全"),
                "应报落地路径不安全: {bad} -> {e:#}"
            );
        }
    }
}
