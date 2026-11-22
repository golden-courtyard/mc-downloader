//! `run` —— 按脚本执行任务（串行或并行）。
//!
//! **参数合并（CLI > 配置文件 > 默认）不在这里**，它留在 `main.rs`：
//! 那一段要同时用 `Args`（clap 类型）和 `DownloadOpts::default()`，
//! 搬进来要么把 `Args` 也搬走、要么把默认值一路传进来，两种都比"留在入口层"更绕。
//!
//! **分界**：`main` 负责"参数怎么解读"，本文件负责"任务怎么调度"。

use std::path::Path;

use anyhow::{Context, Result};

use mc_downloader::config::Tune;
use mc_downloader::download::DownloadOpts;
use mc_downloader::settings::{SourceSettings, Sources};
use mc_downloader::version::SizeHint;
use mc_downloader::{config, download, fabric, forge, neoforge, pack, url_list, vanilla, version};

/// 执行脚本里的全部任务。
///
/// `continue_on_error` 是**脚本级**开关（`continue_on_error = true`）：某个任务失败时是否继续。
/// 为 true 时末尾汇总失败任务并以非零退出 —— 绝不"跳过失败还报成功"。
pub async fn execute(
    tasks: &[config::TaskSpec],
    parallel: bool,
    continue_on_error: bool,
    opts: &DownloadOpts,
    src_cfg: &SourceSettings,
    cfg_path: Option<&std::path::Path>,
) -> Result<()> {
    let total = tasks.len();
    // (第几个任务, 错误)；只在 continue_on_error 时收集
    let mut failed: Vec<(usize, String)> = Vec::new();

    if parallel {
        // 并行时把总并发均分，避免 N × concurrency 条连接炸掉
        let per = opts.split_for(total);
        println!(
            "[*] 并行模式：{total} 个任务，每任务并发 {}（总并发 {}）",
            per.concurrency, opts.concurrency
        );
        let futs: Vec<_> = tasks
            .iter()
            .enumerate()
            .map(|(i, task)| {
                let per = per.clone();
                async move { (i + 1, run_one_task(task, &per, src_cfg, cfg_path).await) }
            })
            .collect();
        for (idx, r) in futures::future::join_all(futs).await {
            if let Err(e) = r {
                if !continue_on_error {
                    return Err(e);
                }
                eprintln!("[!] 第 {idx} 个任务失败（continue_on_error 已跳过）: {e:#}");
                failed.push((idx, format!("{e:#}")));
            }
        }
    } else {
        for (i, task) in tasks.iter().enumerate() {
            if let Err(e) = run_one_task(task, opts, src_cfg, cfg_path).await {
                if !continue_on_error {
                    return Err(e);
                }
                eprintln!(
                    "[!] 第 {} 个任务失败（continue_on_error 已跳过）: {e:#}",
                    i + 1
                );
                failed.push((i + 1, format!("{e:#}")));
            }
        }
    }
    if !failed.is_empty() {
        anyhow::bail!(
            "{}/{} 个任务失败（第 {} 个起；首个错误见上面）",
            failed.len(),
            total,
            failed[0].0
        );
    }
    println!("\n[+] 脚本执行完成：{total} 个任务");
    Ok(())
}

/// 执行单个任务（抽出来是为了能 join_all 并行）
async fn run_one_task(
    task: &config::TaskSpec,
    opts: &DownloadOpts,
    src_cfg: &SourceSettings,
    cfg_path: Option<&std::path::Path>,
) -> Result<()> {
    // **按需解析**：generic 任务（url / urls_file）不需要任何源，
    // 没有配置的机器上它们必须照常能跑（issue #15 的验收之一）
    let need_src = || Sources::resolve(src_cfg, cfg_path);
    // 逐任务覆盖：只影响这一个任务（优先级 CLI > 逐任务 > 脚本全局 > 用户配置 > 默认）
    let tuned = apply_tune(opts, &task.tune())?;
    let opts = &tuned;
    match task {
        config::TaskSpec::Mc {
            version,
            dir,
            loader,
            ..
        } => {
            println!("[mc] {version} -> {dir}");
            let src = need_src()?;
            let root = Path::new(dir);
            vanilla::install(version, root, opts, &src).await?;

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
                            _ => fabric::latest_stable_loader(version, opts, &src.fabric).await?,
                        };
                        fabric::install(version, &lv, root, opts, &src.fabric).await?
                    }
                    // Forge：下载归我们（install_profile 的 libraries），构建归官方 installer（--offline）
                    "forge" => {
                        let fv = match want_ver {
                            Some(v) if !v.is_empty() => v,
                            _ => forge::latest_forge(version, opts, &src.forge_meta).await?,
                        };
                        forge::install(version, &fv, root, opts, &src).await?
                    }
                    // NeoForge：同一个思路（下载归我们、构建归官方 installer），
                    // 但 installer URL 与版本映射都不同（见 neoforge.rs 文件头）
                    "neoforge" => {
                        let nv = match want_ver {
                            Some(v) if !v.is_empty() => v,
                            _ => neoforge::latest(version, opts, &src.neoforge).await?,
                        };
                        neoforge::install(version, &nv, root, opts, &src).await?
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
            ..
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
                        "[!] 跳过 {} 行：非 URL 内容（aria2 的 dir=/out= 之类）/ 推不出文件名 / 重名。逐行原因见上面的警告",
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
                        size: SizeHint::Unknown, // 未知 ⇒ 走单流；不为此多发 N 次 HEAD
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
                // 与 `fetch` 共用同一份推导（审查 A2）：不剥 query / 不 percent-decode 的旧实现在这里
                let (job, fell_back) = url_list::job_from_url(
                    url,
                    name.as_deref(),
                    size.map_or(version::SizeHint::Unknown, version::SizeHint::Hint),
                    mirrors.clone(),
                );
                if fell_back {
                    eprintln!(
                        "[!] 这个 URL 推不出文件名，落地名回落为 out.bin —— 要指定名字请给任务加 name"
                    );
                }
                download::run(&[job], root, opts).await?;
            }
        }
        config::TaskSpec::Pack { file, dir, .. } => {
            let src = need_src()?;
            println!("[pack] {file} -> {dir}");
            pack::install(Path::new(file), Path::new(dir), opts, &src).await?;
        }
    }
    Ok(())
}

/// 把逐任务覆盖叠到全局参数上。`max_speed` 在启动期解析，非法写法直接报错（不静默当"不限速"）。
fn apply_tune(base: &DownloadOpts, t: &Tune<'_>) -> Result<DownloadOpts> {
    let mut o = base.clone();
    if let Some(r) = t.retries {
        o.retries = r;
    }
    if let Some(s) = t.timeout_secs {
        o.timeout_secs = s;
    }
    if let Some(s) = t.max_speed {
        o.max_speed = Some(mc_downloader::progress::parse_speed(s).ok_or_else(|| {
            anyhow::anyhow!("任务里的 max_speed 解析失败：`{s}`（例：5M / 500K / 1048576）")
        })?);
    }
    Ok(o)
}
