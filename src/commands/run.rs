//! `run` —— 按脚本执行任务（串行或并行）。
//!
//! **参数合并（CLI > 配置文件 > 默认）不在这里**，它留在 `main.rs`：
//! 那一段要同时用 `Args`（clap 类型）和 `DownloadOpts::default()`，
//! 搬进来要么把 `Args` 也搬走、要么把默认值一路传进来，两种都比"留在入口层"更绕。
//!
//! **分界**：`main` 负责"参数怎么解读"，本文件负责"任务怎么调度"。

use std::path::Path;

use anyhow::{Context, Result};

use mc_downloader::download::DownloadOpts;
use mc_downloader::{config, download, fabric, forge, neoforge, pack, url_list, vanilla, version};

/// 执行脚本里的全部任务。
pub async fn execute(
    tasks: &[config::TaskSpec],
    parallel: bool,
    opts: &DownloadOpts,
) -> Result<()> {
    let total = tasks.len();

    if parallel {
        // 并行时把总并发均分，避免 N × concurrency 条连接炸掉
        let per = opts.split_for(total);
        println!(
            "[*] 并行模式：{total} 个任务，每任务并发 {}（总并发 {}）",
            per.concurrency, opts.concurrency
        );
        let futs: Vec<_> = tasks.iter().map(|task| run_one_task(task, &per)).collect();
        futures::future::try_join_all(futs).await?;
    } else {
        for task in tasks.iter() {
            run_one_task(task, opts).await?;
        }
    }
    println!("\n[+] 脚本执行完成：{total} 个任务");
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
                    // NeoForge：同一个思路（下载归我们、构建归官方 installer），
                    // 但 installer URL 与版本映射都不同（见 neoforge.rs 文件头）
                    "neoforge" => {
                        let nv = match want_ver {
                            Some(v) if !v.is_empty() => v,
                            _ => neoforge::latest(version, opts).await?,
                        };
                        neoforge::install(version, &nv, root, opts).await?
                    }
                    other => anyhow::bail!(
                        "未知的 loader: \"{other}\"（支持：fabric / forge / neoforge）"
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
                download::run(&jobs, root, &download::batch_opts(opts)).await?;
            } else {
                let url = url.as_ref().expect("校验已保证 url 或 urls_file 有一个");
                println!("[generic] {url}");
                let client = download::client(opts)?;
                // 同 fetch：长度只决定分片与否；没有就走单流（chunked 也没问题）
                let size = match size {
                    Some(s) => Some(*s),
                    None => {
                        let resp = client.head(url).send().await?;
                        download::header_content_length(&resp)
                    }
                };
                let name = name.clone().unwrap_or_else(|| {
                    url.rsplit('/').next().unwrap_or("download.bin").to_string()
                });
                let job = version::Job {
                    url: url.clone(),
                    path: name,
                    sha1: None,
                    size,
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
