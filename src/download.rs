use anyhow::{Context, Result};
use futures::stream::{self, StreamExt};
use sha1::{Digest, Sha1};
use std::path::Path;
use tokio::io::AsyncWriteExt;

use crate::version::Job;

const CONCURRENCY: usize = 16;

/// 统一构造 HTTP 客户端。
///
/// **默认直连**：Mojang CDN / Fabric meta / Modrinth CDN 在国内均直连可达
/// （实测 26.3、1.7.10、Fabric profile、48 个模组都未走代理）。
/// 只有显式传 `--proxy` 时才走代理 —— 与「代理按域名判，不是一律加」一致。
pub fn client(proxy: Option<&str>) -> Result<reqwest::Client> {
    let mut b = reqwest::Client::builder().user_agent("mc-downloader/0.3");
    b = match proxy {
        Some(p) => b.proxy(
            reqwest::Proxy::all(p).with_context(|| format!("代理地址无效: {p}"))?,
        ),
        None => b.no_proxy(),
    };
    Ok(b.build()?)
}

/// 在 blocking 线程池里算文件 SHA1，避免阻塞 tokio worker
async fn sha1_file(path: &Path) -> Result<String> {
    let p = path.to_path_buf();
    tokio::task::spawn_blocking(move || -> Result<String> {
        use std::io::Read;
        let mut f = std::fs::File::open(&p)?;
        let mut h = Sha1::new();
        let mut buf = vec![0u8; 128 * 1024];
        loop {
            let n = f.read(&mut buf)?;
            if n == 0 {
                break;
            }
            h.update(&buf[..n]);
        }
        Ok(hex::encode(h.finalize()))
    })
    .await?
}

pub async fn run(jobs: &[Job], root: &Path, proxy: Option<&str>, retries: u32) -> Result<()> {
    let client = client(proxy)?;

    let total = jobs.len();
    let stream = stream::iter(jobs.iter().cloned().map(|job| {
        let client = client.clone();
        let root = root.to_path_buf();
        async move { one_with_retry(&client, &job, &root, retries).await }
    }))
    .buffer_unordered(CONCURRENCY);

    futures::pin_mut!(stream);

    let mut done = 0usize;
    let mut cached = 0usize;

    while let Some(r) = stream.next().await {
        let (path, was_cached) = r.with_context(|| "下载中断")?;
        done += 1;
        if was_cached {
            cached += 1;
        } else {
            println!("  [{done}/{total}] {path}");
        }
    }

    println!(
        "[*] 新下载 {} 个，已存在 {} 个",
        done.saturating_sub(cached),
        cached
    );
    Ok(())
}

/// 带重试：失败后指数退避重试（1s, 2s, 4s…）
/// 参照 aria2 的 --max-tries / --retry-wait 思路 —— 48 个模组里一个抖动不该让整批失败。
async fn one_with_retry(
    client: &reqwest::Client,
    job: &Job,
    root: &Path,
    retries: u32,
) -> Result<(String, bool)> {
    let mut attempt = 0u32;
    loop {
        match one(client, job, root).await {
            Ok(v) => return Ok(v),
            Err(e) => {
                if attempt >= retries {
                    return Err(e.context(format!(
                        "{} 重试 {retries} 次仍失败",
                        job.path
                    )));
                }
                let wait = 1u64 << attempt; // 1, 2, 4, 8…
                tokio::time::sleep(std::time::Duration::from_secs(wait)).await;
                attempt += 1;
            }
        }
    }
}

/// 单文件：**流式写盘 + 边写边算 SHA1**，不再把整个文件读进内存。
///
/// 先写 `<file>.part`，校验通过再 rename —— 中断时不会留下一个
/// 看起来"完整"但实际是半截的文件。
async fn one(client: &reqwest::Client, job: &Job, root: &Path) -> Result<(String, bool)> {
    let dst = root.join(&job.path);

    // 已存在且 sha1 匹配 => 跳过（断点续传）
    if dst.exists() {
        match &job.sha1 {
            Some(expect) => {
                if sha1_file(&dst).await? == *expect {
                    return Ok((job.path.clone(), true));
                }
            }
            None => return Ok((job.path.clone(), true)),
        }
    }

    let resp = client
        .get(&job.url)
        .send()
        .await?
        .error_for_status()?;

    if let Some(parent) = dst.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }

    let tmp = dst.with_extension(format!(
        "{}part",
        dst.extension()
            .and_then(|e| e.to_str())
            .map(|e| format!("{e}."))
            .unwrap_or_default()
    ));

    let mut file = tokio::fs::File::create(&tmp)
        .await
        .with_context(|| format!("创建临时文件失败: {}", tmp.display()))?;

    let mut hasher = Sha1::new();
    let mut stream = resp.bytes_stream();

    while let Some(chunk) = stream.next().await {
        let chunk = chunk.with_context(|| format!("读取响应流失败: {}", job.url))?;
        hasher.update(&chunk);
        file.write_all(&chunk).await?;
    }
    file.flush().await?;
    drop(file);

    if let Some(expect) = &job.sha1 {
        let got = hex::encode(hasher.finalize());
        if got != *expect {
            let _ = tokio::fs::remove_file(&tmp).await;
            anyhow::bail!("sha1 不匹配 {}: got {} expect {}", job.path, got, expect);
        }
    }

    tokio::fs::rename(&tmp, &dst)
        .await
        .with_context(|| format!("落盘失败: {}", dst.display()))?;

    Ok((job.path.clone(), false))
}
