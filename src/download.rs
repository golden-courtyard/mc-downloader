use anyhow::{Context, Result};
use futures::stream::{self, StreamExt};
use sha1::{Digest, Sha1};
use std::io::{Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;
use tokio::io::AsyncWriteExt;

use crate::piece::PieceTable;
use crate::progress::Progress;
use crate::scheduler::{PieceJob, PieceScheduler};
use crate::version::Job;

const CONCURRENCY: usize = 16;
/// 单文件分片时的并发连接数
const PIECE_CONNS: usize = 4;
/// 低于此大小不分片
const SPLIT_THRESHOLD: u64 = 8 * 1024 * 1024;

/// 统一构造 HTTP 客户端。
///
/// **默认直连**：Mojang CDN / Fabric meta / Modrinth CDN 在国内均直连可达。
/// 只有显式传 `--proxy` 时才走代理。
pub fn client(proxy: Option<&str>) -> Result<reqwest::Client> {
    let mut b = reqwest::Client::builder().user_agent("mc-downloader/0.4");
    b = match proxy {
        Some(p) => b.proxy(reqwest::Proxy::all(p).with_context(|| format!("代理地址无效: {p}"))?),
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

fn part_path(dst: &Path) -> PathBuf {
    let ext = dst
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| format!("{e}."))
        .unwrap_or_default();
    dst.with_extension(format!("{ext}part"))
}

pub async fn run(jobs: &[Job], root: &Path, proxy: Option<&str>, retries: u32) -> Result<()> {
    let client = client(proxy)?;

    let total_bytes: u64 = jobs.iter().filter_map(|j| j.size).sum();
    let pg = Progress::new(total_bytes);
    let reporter = pg.spawn_reporter();

    let total = jobs.len();
    let stream = stream::iter(jobs.iter().cloned().map(|job| {
        let client = client.clone();
        let root = root.to_path_buf();
        let pg = pg.clone();
        async move { one_with_retry(&client, &job, &root, retries, &pg).await }
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

    reporter.abort();

    println!(
        "[*] 新下载 {} 个，已存在 {} 个",
        done.saturating_sub(cached),
        cached
    );
    Ok(())
}

/// 带重试：失败后指数退避（1s, 2s, 4s…）
async fn one_with_retry(
    client: &reqwest::Client,
    job: &Job,
    root: &Path,
    retries: u32,
    pg: &Arc<Progress>,
) -> Result<(String, bool)> {
    let mut attempt = 0u32;
    loop {
        match one(client, job, root, retries, pg).await {
            Ok(v) => return Ok(v),
            Err(e) => {
                if attempt >= retries {
                    return Err(e.context(format!("{} 重试 {retries} 次仍失败", job.path)));
                }
                tokio::time::sleep(Duration::from_secs(1u64 << attempt)).await;
                attempt += 1;
            }
        }
    }
}

/// 单文件入口：大于 8 MB 且有 size 才分片，否则单连接流式。
async fn one(
    client: &reqwest::Client,
    job: &Job,
    root: &Path,
    retries: u32,
    pg: &Arc<Progress>,
) -> Result<(String, bool)> {
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

    if let Some(parent) = dst.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }

    if let Some(size) = job.size {
        if size >= SPLIT_THRESHOLD {
            one_split(client, job, &dst, size, retries, pg).await?;
            return Ok((job.path.clone(), false));
        }
    }

    one_stream(client, job, &dst, pg).await?;
    Ok((job.path.clone(), false))
}

/// 单连接流式下载（小文件／服务器不支持 Range 时用）
async fn one_stream(
    client: &reqwest::Client,
    job: &Job,
    dst: &Path,
    pg: &Arc<Progress>,
) -> Result<()> {
    let resp = client.get(&job.url).send().await?.error_for_status()?;

    let tmp = part_path(dst);
    let mut file = tokio::fs::File::create(&tmp)
        .await
        .with_context(|| format!("创建临时文件失败: {}", tmp.display()))?;

    let mut hasher = Sha1::new();
    let mut stream = resp.bytes_stream();

    while let Some(chunk) = stream.next().await {
        let chunk = chunk.with_context(|| format!("读取响应流失败: {}", job.url))?;
        hasher.update(&chunk);
        pg.add(chunk.len() as u64);
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

    tokio::fs::rename(&tmp, dst)
        .await
        .with_context(|| format!("落盘失败: {}", dst.display()))?;
    Ok(())
}

/// 分片下载：Range GET + 按偏移写同一临时文件。
///
/// Windows 没有 `pwrite`，所以每个分片**独立打开一次文件句柄**再 seek+write
/// —— 独立句柄的写指针互不干扰，跨平台安全。
async fn one_split(
    client: &reqwest::Client,
    job: &Job,
    dst: &Path,
    size: u64,
    retries: u32,
    pg: &Arc<Progress>,
) -> Result<()> {
    let table = PieceTable::new(size);
    if !table.is_split() {
        return one_stream(client, job, dst, pg).await;
    }

    let tmp = part_path(dst);
    // 预分配（稀疏文件），保证后续按偏移写不越界
    {
        let f = std::fs::File::create(&tmp)
            .with_context(|| format!("预分配失败: {}", tmp.display()))?;
        f.set_len(size)?;
    }

    let sched = Arc::new(PieceScheduler::new());
    let jobs: Vec<PieceJob> = (0..table.total())
        .filter_map(|i| {
            let (s, e) = table.range(i)?;
            Some(PieceJob {
                file_idx: 0,
                piece_idx: i,
                start: s,
                end: e,
                url: job.url.clone(),
            })
        })
        .collect();
    let n_pieces = jobs.len();
    sched.push_many(jobs).await;
    sched.close(); // 不会再有新分片

    let conns = PIECE_CONNS.min(n_pieces).max(1);
    let mut handles = Vec::new();
    for _ in 0..conns {
        let s = sched.clone();
        let c = client.clone();
        let t = tmp.clone();
        let pg = pg.clone();
        handles.push(tokio::spawn(async move {
            while let Some(pj) = s.next().await {
                let mut attempt = 0u32;
                loop {
                    match fetch_piece(&c, &pj, &t, &pg).await {
                        Ok(()) => break,
                        Err(e) => {
                            if attempt >= retries {
                                return Err(e.context(format!("分片 {} 重试耗尽", pj.piece_idx)));
                            }
                            tokio::time::sleep(Duration::from_secs(1u64 << attempt)).await;
                            attempt += 1;
                        }
                    }
                }
            }
            Ok::<_, anyhow::Error>(())
        }));
    }
    for h in handles {
        h.await??;
    }

    // 整文件校验（SHA1 是全文件的，不是分片的）
    if let Some(expect) = &job.sha1 {
        let got = sha1_file(&tmp).await?;
        if got != *expect {
            let _ = tokio::fs::remove_file(&tmp).await;
            anyhow::bail!("sha1 不匹配 {}: got {} expect {}", job.path, got, expect);
        }
    }

    tokio::fs::rename(&tmp, dst)
        .await
        .with_context(|| format!("落盘失败: {}", dst.display()))?;
    Ok(())
}

/// 拉一个分片并写到指定偏移
async fn fetch_piece(
    client: &reqwest::Client,
    pj: &PieceJob,
    path: &Path,
    pg: &Arc<Progress>,
) -> Result<()> {
    let resp = client
        .get(&pj.url)
        .header(
            reqwest::header::RANGE,
            format!("bytes={}-{}", pj.start, pj.end - 1),
        )
        .send()
        .await?;

    if resp.status() != reqwest::StatusCode::PARTIAL_CONTENT {
        anyhow::bail!(
            "服务器不支持 Range（HTTP {}），无法分片: {}",
            resp.status(),
            pj.url
        );
    }

    let expect = (pj.end - pj.start) as usize;
    let mut buf = Vec::with_capacity(expect);
    let mut stream = resp.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk?;
        pg.add(chunk.len() as u64);
        buf.extend_from_slice(&chunk);
    }
    if buf.len() != expect {
        anyhow::bail!(
            "分片 {} 长度不符: {} != {}",
            pj.piece_idx,
            buf.len(),
            expect
        );
    }

    let p = path.to_path_buf();
    let offset = pj.start;
    tokio::task::spawn_blocking(move || -> Result<()> {
        let mut f = std::fs::OpenOptions::new().write(true).open(&p)?;
        f.seek(SeekFrom::Start(offset))?;
        f.write_all(&buf)?;
        Ok(())
    })
    .await??;

    Ok(())
}
