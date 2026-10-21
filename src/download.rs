use anyhow::{Context, Result};
use futures::stream::{self, StreamExt};
use sha1::{Digest, Sha1};
use std::io::{Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;
use tokio::io::AsyncWriteExt;

use crate::piece::{PieceState, PieceTable};
use crate::progress::Progress;
use crate::scheduler::{PieceJob, PieceScheduler};
use crate::source::SourcePool;
use crate::version::Job;

/// 默认：同时下载的文件数
pub const DEFAULT_CONCURRENCY: usize = 16;
/// 默认：单文件分片时的并发连接数
pub const DEFAULT_PIECE_CONNS: usize = 4;
/// 低于此大小不分片
const SPLIT_THRESHOLD: u64 = 8 * 1024 * 1024;
/// 默认超时（秒）：只作用于建连与单次读，**不是**整请求总时限
pub const DEFAULT_TIMEOUT_SECS: u64 = 30;

/// 一次下载的全部参数。
///
/// 收成一个结构体是因为这 5 个值原本要在
/// `run → one_with_retry → one → one_split` 四层里手工搬运，
/// 每加一个参数要改 4~5 个签名（clippy 报过两次 `too many arguments`）。
#[derive(Debug, Clone)]
pub struct DownloadOpts {
    /// `None` = 直连（不走系统代理）
    pub proxy: Option<String>,
    /// 单个文件失败后的重试次数
    pub retries: u32,
    /// 同时下载的文件数
    pub concurrency: usize,
    /// 单文件分片时的并发连接数
    pub piece_conns: usize,
    /// AIMD 自适应连接数
    pub adaptive: bool,
    /// 建连超时 + 读超时（秒）
    pub timeout_secs: u64,
}

impl Default for DownloadOpts {
    fn default() -> Self {
        Self {
            proxy: None,
            retries: 3,
            concurrency: DEFAULT_CONCURRENCY,
            piece_conns: DEFAULT_PIECE_CONNS,
            adaptive: false,
            timeout_secs: DEFAULT_TIMEOUT_SECS,
        }
    }
}

impl DownloadOpts {
    /// 任务级并行时按任务数均分总并发，避免 N × concurrency 条连接把对端打爆。
    pub fn split_for(&self, n: usize) -> Self {
        let mut o = self.clone();
        o.concurrency = (self.concurrency / n.max(1)).max(1);
        o
    }
}

/// 统一构造 HTTP 客户端。
///
/// **默认直连**：Mojang CDN / Fabric meta / Modrinth CDN 在国内均直连可达。
/// 只有显式配 `proxy` 时才走代理。
///
/// 超时用 `connect_timeout` + `read_timeout`，**刻意不用 `ClientBuilder::timeout`**——
/// 后者的语义是「从开始连接到响应体读完的总时限」，拿它当下载超时会让
/// 任何超过该时长的文件（3 GB ISO 实测分钟级）在传输途中被掐断。
pub fn client(opts: &DownloadOpts) -> Result<reqwest::Client> {
    let mut b = reqwest::Client::builder()
        .user_agent(concat!("mc-downloader/", env!("CARGO_PKG_VERSION")))
        .connect_timeout(Duration::from_secs(opts.timeout_secs))
        .read_timeout(Duration::from_secs(opts.timeout_secs));
    b = match &opts.proxy {
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

/// 从响应头读 `Content-Length`。
///
/// ⚠️ **不要用 `Response::content_length()`**：它取的是 **body 的 size_hint**，
/// 而 HEAD 响应没有 body ⇒ 稳定返回 `Some(0)`。拿它当文件大小会让「不带 --size 的
/// 探测」永远得到 0，于是静默退化成单连接不分片。
pub fn header_content_length(resp: &reqwest::Response) -> Option<u64> {
    resp.headers()
        .get(reqwest::header::CONTENT_LENGTH)?
        .to_str()
        .ok()?
        .parse::<u64>()
        .ok()
        .filter(|n| *n > 0)
}

fn part_path(dst: &Path) -> PathBuf {
    let ext = dst
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| format!("{e}."))
        .unwrap_or_default();
    dst.with_extension(format!("{ext}part"))
}

pub async fn run(jobs: &[Job], root: &Path, opts: &DownloadOpts) -> Result<()> {
    let client = client(opts)?;

    let total_bytes: u64 = jobs.iter().filter_map(|j| j.size).sum();
    let pg = Progress::new(total_bytes);
    let reporter = pg.spawn_reporter();

    let total = jobs.len();
    let stream = stream::iter(jobs.iter().cloned().map(|job| {
        let client = client.clone();
        let root = root.to_path_buf();
        let pg = pg.clone();
        let opts = opts.clone();
        async move { one_with_retry(&client, &job, &root, &opts, &pg).await }
    }))
    .buffer_unordered(opts.concurrency.max(1));

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
    opts: &DownloadOpts,
    pg: &Arc<Progress>,
) -> Result<(String, bool)> {
    let mut attempt = 0u32;
    loop {
        match one(client, job, root, opts, pg).await {
            Ok(v) => return Ok(v),
            Err(e) => {
                if attempt >= opts.retries {
                    return Err(e.context(format!("{} 重试 {} 次仍失败", job.path, opts.retries)));
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
    opts: &DownloadOpts,
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

    if let Some(size) = job.size
        && size >= SPLIT_THRESHOLD
    {
        one_split(client, job, &dst, size, opts, pg).await?;
        return Ok((job.path.clone(), false));
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
    opts: &DownloadOpts,
    pg: &Arc<Progress>,
) -> Result<()> {
    let mut table = PieceTable::new(size);
    if !table.is_split() {
        return one_stream(client, job, dst, pg).await;
    }

    let tmp = part_path(dst);

    // 续传：`.part` 与 `.part.state` 同时有效才认这份进度。
    // 只有 `.part` 没有状态文件时无法知道哪些片段完整（稀疏文件的空洞读出来是 0），
    // 只能从头下 —— 宁可重下，不要拼出坏文件。
    if tmp.exists() {
        match PieceState::load(&tmp, size) {
            Some(t) => {
                if t.done_count() > 0 {
                    println!(
                        "[*] 续传：已完成 {}/{} 片，只补缺口",
                        t.done_count(),
                        t.total()
                    );
                    pg.add(t.done_bytes());
                }
                table = t;
            }
            None => println!("[*] 有 .part 但无有效进度记录，从头下载"),
        }
    }

    // 预分配（稀疏文件），保证后续按偏移写不越界。
    // `truncate(false)` 是**刻意**的：续传时文件里已有已下好的内容，
    // 截断会把它清掉，接下来只写缺口就会拼出一个前段全零的坏文件。
    {
        let f = std::fs::OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(false)
            .open(&tmp)
            .with_context(|| format!("打开临时文件失败: {}", tmp.display()))?;
        if f.metadata()?.len() != size {
            f.set_len(size)?;
        }
    }

    // 多源：job.mirrors 非空时用它，否则只用 job.url
    let mirrors: Vec<String> = if job.mirrors.is_empty() {
        vec![job.url.clone()]
    } else {
        let mut m = job.mirrors.clone();
        if !m.contains(&job.url) {
            m.insert(0, job.url.clone());
        }
        m
    };
    let pool = Arc::new(SourcePool::new(&mirrors));
    if mirrors.len() > 1 {
        println!("[*] 多源: {} 个", mirrors.len());
    }

    let sched = Arc::new(PieceScheduler::new());
    // 只排未完成的片段：位图在这里第一次被真正读到
    let jobs: Vec<PieceJob> = table
        .pending()
        .into_iter()
        .filter_map(|i| {
            let (s, e) = table.range(i)?;
            Some(PieceJob {
                file_idx: 0,
                piece_idx: i,
                start: s,
                end: e,
                source_idx: pool.pick(),
            })
        })
        .collect();
    let n_pieces = jobs.len();
    sched.push_many(jobs).await;
    sched.close(); // 不会再有新分片

    // 单片大小只在这里取一次（tuner 闭包里不能 await 锁）；仅 adaptive 会用到它
    let piece_size = table.piece_size();
    let table = Arc::new(tokio::sync::Mutex::new(table));

    let conns = opts.piece_conns.min(n_pieces).max(1);
    // 自适应模式：worker 数按上限开，实际并发由 Semaphore 控制
    let workers = if opts.adaptive {
        (conns * 2).min(n_pieces).max(1)
    } else {
        conns
    };
    // adaptive：并发上限放在 budget 里（tuner 可写、worker 只读），不用固定容量的 Semaphore
    // —— tokio 的 `forget_permits` 只能减「空闲」许可（`batch_semaphore.rs:411` 饱和相减），
    //    而稳态下 workers(2×conns) > permits(conns) 使 avail 恒为 0，减半分支因此永远减不动。
    // 非 adaptive：路径保持原样，仍用 Semaphore。
    let gate = opts.adaptive.then(|| {
        (
            Arc::new(AtomicUsize::new(conns)),
            Arc::new(AtomicUsize::new(0)),
        )
    });
    let sem = (!opts.adaptive).then(|| Arc::new(tokio::sync::Semaphore::new(conns)));

    let retries = opts.retries;
    let mut handles = Vec::new();
    for _ in 0..workers {
        let s = sched.clone();
        let c = client.clone();
        let t = tmp.clone();
        let pg = pg.clone();
        let pool = pool.clone();
        let sem = sem.clone();
        let gate = gate.clone();
        let tbl = table.clone();
        handles.push(tokio::spawn(async move {
            while let Some(pj) = s.next().await {
                // adaptive：先在闸门里占一个槽（budget 由 tuner 动态调整）；非 adaptive 走 Semaphore
                if let Some((b, n)) = &gate {
                    loop {
                        let cap = b.load(Ordering::Relaxed);
                        if n.fetch_add(1, Ordering::Relaxed) < cap {
                            break;
                        }
                        n.fetch_sub(1, Ordering::Relaxed);
                        tokio::time::sleep(Duration::from_millis(50)).await;
                    }
                }
                let _permit = match &sem {
                    Some(sm) => sm.acquire().await.ok(),
                    None => None,
                };
                let mut attempt = 0u32;
                loop {
                    match fetch_piece(&c, &pj, &t, &pg, &pool).await {
                        Ok(()) => {
                            tbl.lock().await.mark_done(pj.piece_idx);
                            break;
                        }
                        Err(e) => {
                            if attempt >= retries {
                                // worker 即将退出：把闸门槽还回去，别让计数器悬着
                                if let Some((_, n)) = &gate {
                                    n.fetch_sub(1, Ordering::Relaxed);
                                }
                                return Err(e.context(format!("分片 {} 重试耗尽", pj.piece_idx)));
                            }
                            tokio::time::sleep(Duration::from_secs(1u64 << attempt)).await;
                            attempt += 1;
                        }
                    }
                }
                if let Some((_, n)) = &gate {
                    n.fetch_sub(1, Ordering::Relaxed);
                }
            }
            Ok::<_, anyhow::Error>(())
        }));
    }
    // 同上：不是「有/无闸门两种分支」，而是「有 gate 才派调参任务」
    let tuner = gate.as_ref().map(|(b, n)| {
        let b = b.clone();
        let n = n.clone();
        let pg2 = pg.clone();
        tokio::spawn(async move {
            // 3 个 tick 内下不来一片 ⇒ AIMD 的调节粒度已大于响应窗口。
            // 改 TICK 必须同步改这里（`Duration::as_secs_f64` 不是 const fn，故写字面量）。
            const TICK: Duration = Duration::from_secs(2);
            const COARSE_SECS: f64 = 3.0 * 2.0;
            let mut budget = conns;
            let mut zero_ticks = 0u32;
            let mut checked = false;
            let mut last_cut = 0u32;
            let mut t = 0u32;
            let mut last = pg2.done();
            let mut last_delta = 0u64;
            let mut tick = tokio::time::interval(TICK);
            tick.tick().await;
            loop {
                tick.tick().await;
                t += 1;
                let now = pg2.done();
                let delta = now.saturating_sub(last);
                last = now;

                // ① 一次性判定：必须排在任何 add/cut 之前 —— 否则退回时 budget 已不在初始值，
                //    "退回固定并发"退回的就是个被调过的数。
                if !checked {
                    let piece_secs = if delta == 0 {
                        zero_ticks += 1;
                        if zero_ticks < 3 {
                            continue;
                        }
                        f64::INFINITY // 连续 3 个 tick 一个字节都没动
                    } else {
                        piece_size as f64 * conns as f64 * 2.0 / delta as f64
                    };
                    checked = true;
                    if piece_secs > COARSE_SECS {
                        let reason = if delta == 0 { "no_data" } else { "coarse_grain" };
                        if delta == 0 {
                            eprintln!(
                                "[adaptive] 连续 3 个 2s tick 无数据（piece={piece_size}B × {conns} 连）：AIMD 无从调节，已退回固定并发 {conns}；请先确认链路可用"
                            );
                        } else {
                            eprintln!(
                                "[adaptive] 单片预计 {piece_secs:.0}s（piece={piece_size}B × {conns} 连），超过 3 个 2s tick：AIMD 的调节粒度大于响应窗口，已退回固定并发 {conns}；如需自适应请提高带宽或降低 --piece-conns"
                            );
                        }
                        if std::env::var_os("MC_TUNE_LOG").is_some() {
                            eprintln!("[tune] act=disabled reason={reason}");
                        }
                        // 任务就此结束：不再 add/cut，并发停在 conns。
                        // 收尾处的 `tuner.abort()` 对已结束任务是 no-op（harness.rs:118
                        // `remote_abort` 的状态迁移失败即返回），这里并没有"取消"逻辑。
                        return;
                    }
                }
                if delta == 0 {
                    continue;
                }

                let mut act: &str = "hold";
                let mut take = 0usize;
                if delta > last_delta && budget < workers {
                    budget += 1;
                    act = "add";
                } else if delta < last_delta && budget > 1 && t - last_cut >= 2 {
                    // 防抖：减半后两个 tick 内不再减，避免抖动下反复砍半把并发饿死在低位
                    take = (budget / 2).max(1);
                    budget -= take;
                    last_cut = t;
                    act = "cut";
                }
                b.store(budget, Ordering::Relaxed);
                // 诊断输出（design §1.3 D6）：只读，不参与任何判断，无环境变量时零行为差异
                if std::env::var_os("MC_TUNE_LOG").is_some() {
                    eprintln!(
                        "[tune] act={act} budget={budget} take={take} inflight={} delta={delta} last={last_delta}",
                        n.load(Ordering::Relaxed)
                    );
                }
                last_delta = delta;
            }
        })
    });

    // 位图落盘：每 2 秒快照一次。
    // 崩溃最多丢 2 秒的进度（重下几片），不值得为每一片做一次同步写。
    let flusher = {
        let p = tmp.clone();
        let tbl = table.clone();
        tokio::spawn(async move {
            let mut tick = tokio::time::interval(Duration::from_secs(2));
            tick.tick().await;
            loop {
                tick.tick().await;
                let snap = tbl.lock().await.clone();
                let p = p.clone();
                match tokio::task::spawn_blocking(move || PieceState::save(&p, &snap)).await {
                    Ok(Ok(())) => {}
                    Ok(Err(e)) => eprintln!("[!] 进度状态写入失败: {e}"),
                    Err(e) => eprintln!("[!] 进度状态任务失败: {e}"),
                }
            }
        })
    };

    // 逐个收 worker：出错也要先停掉 flusher，否则后台任务会一直写状态
    let mut worker_err: Option<anyhow::Error> = None;
    for h in handles {
        match h.await {
            Ok(Ok(())) => {}
            Ok(Err(e)) => {
                worker_err = Some(e);
                break;
            }
            Err(join_err) => {
                worker_err = Some(anyhow::anyhow!("分片 worker 异常退出: {join_err}"));
                break;
            }
        }
    }
    if let Some(t) = tuner {
        t.abort();
    }
    flusher.abort();
    if let Some(e) = worker_err {
        return Err(e);
    }
    // 收尾再落一次：此刻位图应是全 done，供下次直接跳过
    {
        let snap = table.lock().await.clone();
        let p = tmp.clone();
        let _ = tokio::task::spawn_blocking(move || PieceState::save(&p, &snap)).await;
    }

    // 整文件校验（SHA1 是全文件的，不是分片的）
    if let Some(expect) = &job.sha1 {
        let got = sha1_file(&tmp).await?;
        if got != *expect {
            let _ = tokio::fs::remove_file(&tmp).await;
            PieceState::remove(&tmp);
            anyhow::bail!("sha1 不匹配 {}: got {} expect {}", job.path, got, expect);
        }
    }

    tokio::fs::rename(&tmp, dst)
        .await
        .with_context(|| format!("落盘失败: {}", dst.display()))?;
    // 成品已落地，状态文件不再有意义；留着会和下次的 .part 配错
    PieceState::remove(&tmp);
    Ok(())
}

/// 拉一个分片并写到指定偏移
async fn fetch_piece(
    client: &reqwest::Client,
    pj: &PieceJob,
    path: &Path,
    pg: &Arc<Progress>,
    pool: &Arc<SourcePool>,
) -> Result<()> {
    let url = pool
        .url_of(pj.source_idx)
        .ok_or_else(|| anyhow::anyhow!("源 {} 不存在", pj.source_idx))?;
    let t0 = std::time::Instant::now();

    let resp = client
        .get(&url)
        .header(
            reqwest::header::RANGE,
            format!("bytes={}-{}", pj.start, pj.end - 1),
        )
        .send()
        .await?;

    if resp.status() != reqwest::StatusCode::PARTIAL_CONTENT {
        pool.record_failure(pj.source_idx);
        anyhow::bail!(
            "服务器不支持 Range（HTTP {}），无法分片: {}",
            resp.status(),
            url
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

    pool.record_success(pj.source_idx, expect as u64, t0.elapsed().as_secs_f64());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_opts_are_sane() {
        let o = DownloadOpts::default();
        assert_eq!(o.concurrency, DEFAULT_CONCURRENCY);
        assert_eq!(o.piece_conns, DEFAULT_PIECE_CONNS);
        assert_eq!(o.retries, 3);
        assert_eq!(o.timeout_secs, DEFAULT_TIMEOUT_SECS);
        assert!(!o.adaptive);
        assert!(o.proxy.is_none());
    }

    #[test]
    fn split_for_never_yields_zero() {
        let o = DownloadOpts::default(); // 16
        assert_eq!(o.split_for(4).concurrency, 4);
        // 任务数多于总并发：每任务至少 1，不能出现 0（否则 buffer_unordered(0) 永不推进）
        assert_eq!(o.split_for(32).concurrency, 1);
        // n=0 不该除零
        assert_eq!(o.split_for(0).concurrency, 16);
    }

    #[test]
    fn split_for_preserves_other_fields() {
        let o = DownloadOpts {
            proxy: Some("http://127.0.0.1:7897".into()),
            retries: 7,
            adaptive: true,
            timeout_secs: 9,
            ..Default::default()
        };
        let s = o.split_for(2);
        assert_eq!(s.proxy, o.proxy);
        assert_eq!(s.retries, 7);
        assert!(s.adaptive);
        assert_eq!(s.timeout_secs, 9);
        assert_eq!(s.concurrency, 8);
    }

    #[test]
    fn client_builds_with_default_opts() {
        assert!(client(&DownloadOpts::default()).is_ok());
    }

    #[test]
    fn client_rejects_malformed_proxy() {
        let o = DownloadOpts {
            proxy: Some("这不是一个 URL".into()),
            ..Default::default()
        };
        assert!(client(&o).is_err(), "非法代理地址应当报错，而不是静默直连");
    }
}
