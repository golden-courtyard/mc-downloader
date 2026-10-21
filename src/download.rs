use anyhow::{Context, Result};
use futures::stream::{self, StreamExt};
use sha1::{Digest, Sha1};
use std::path::Path;

use crate::version::Job;

const CONCURRENCY: usize = 16;

pub async fn run(jobs: &[Job], root: &Path) -> Result<()> {
    let client = reqwest::Client::builder()
        .user_agent("mc-downloader/0.1")
        .build()?;

    let total = jobs.len();
    let stream = stream::iter(jobs.iter().cloned().map(|job| {
        let client = client.clone();
        let root = root.to_path_buf();
        async move { one(&client, &job, &root).await }
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

async fn one(client: &reqwest::Client, job: &Job, root: &Path) -> Result<(String, bool)> {
    let dst = root.join(&job.path);

    // 已存在且 sha1 匹配 => 跳过
    if dst.exists() {
        if let Some(expect) = &job.sha1 {
            let data = tokio::fs::read(&dst).await?;
            if hex::encode(Sha1::digest(&data)) == *expect {
                return Ok((job.path.clone(), true));
            }
        } else {
            return Ok((job.path.clone(), true));
        }
    }

    let bytes = client
        .get(&job.url)
        .send()
        .await?
        .error_for_status()?
        .bytes()
        .await?;

    if let Some(expect) = &job.sha1 {
        let got = hex::encode(Sha1::digest(&bytes));
        if got != *expect {
            anyhow::bail!("sha1 不匹配 {}: got {} expect {}", job.path, got, expect);
        }
    }

    if let Some(parent) = dst.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }
    tokio::fs::write(&dst, &bytes).await?;

    Ok((job.path.clone(), false))
}
