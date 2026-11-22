use anyhow::{Context, Result, bail};
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
    /// 单个文件失败时是否继续下其余的。
    ///
    /// **默认 `false`**：MC 场景（原版 / 整合包 / 加载器）要求"全有或全无"——
    /// 少一个文件游戏就坏了，中断比"下完了但缺东西"更好。
    ///
    /// `urls_file`（批量独立文件）设 `true`：100 个文件里第 5 个挂了，
    /// 不该让后面 95 个白等。**仍然返回 Err（退出码非 0）+ 列出失败项**，
    /// 只是其余文件都已下完。
    pub continue_on_error: bool,
    /// 全局总带宽上限（字节/秒）。`None` = 不限速。
    ///
    /// **是全局的，不是每连接一份** —— 所有 chunk 都过同一个令牌桶
    /// （位置在 `Progress::add_throttled`）。
    pub max_speed: Option<u64>,
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
            continue_on_error: false,
            max_speed: None,
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

/// 批量任务的 opts：**继续下、最后汇总失败项**（仍然返回 Err）。
///
/// 为什么要这个：`continue_on_error = false` 时，**一个文件超时就中断整批**。
/// 实测代价：1.21.1 原版 3983 个 asset，网络差时第 2000 多个挂掉 ⇒
/// 其余白下 + 整批报错（`3051s` 那次）。而"全有或全无"的本意是
/// **最终要下全**，不是**中途不许失败** —— 所以继续下、最后检查，两者都满足。
///
/// ⚠️ `forge` / `neoforge` 用它是安全的：失败会在 `run` 末尾 `bail`，
/// **根本走不到 installer `--offline`**（那一步要求库全齐）。
pub fn batch_opts(opts: &DownloadOpts) -> DownloadOpts {
    let mut o = opts.clone();
    o.continue_on_error = true;
    o
}

/// 把人可读的速率格式化出来（`5M` → `5.0 MB/s`）。
pub fn human_rate(bytes_per_sec: u64) -> String {
    let f = bytes_per_sec as f64;
    if f >= 1024.0 * 1024.0 * 1024.0 {
        format!("{:.1} GB", f / (1024.0 * 1024.0 * 1024.0))
    } else if f >= 1024.0 * 1024.0 {
        format!("{:.1} MB", f / (1024.0 * 1024.0))
    } else if f >= 1024.0 {
        format!("{:.1} KB", f / 1024.0)
    } else {
        format!("{bytes_per_sec} B")
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
    let pg = match opts.max_speed {
        Some(rate) => {
            println!("[*] 限速: {}/s", human_rate(rate));
            Progress::with_limit(total_bytes, rate)
        }
        None => Progress::new(total_bytes),
    };
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

    let mut failed: Vec<String> = Vec::new();
    while let Some(r) = stream.next().await {
        match r {
            Ok((path, was_cached)) => {
                done += 1;
                if was_cached {
                    cached += 1;
                } else {
                    println!("  [{done}/{total}] {path}");
                }
            }
            Err(e) if opts.continue_on_error => {
                // 记下来继续；最后统一报，不在这里打印（并发下会串行刷屏）
                failed.push(format!("{e:#}"));
            }
            Err(e) => {
                // 早退也要停掉进度线程：否则它在同一屏上打「完成」，紧跟着的
                // 「下载中断」就自相矛盾（实测：http_semantics 的 lying-large 用例
                // 会打印 `[ 10.0 MB / 10.0 MB ] 完成` 然后报超时中断）。
                reporter.abort();
                return Err(e).with_context(|| "下载中断");
            }
        }
    }

    reporter.abort();

    println!(
        "[*] 新下载 {} 个，已存在 {} 个",
        done.saturating_sub(cached),
        cached
    );

    // continue_on_error 下：失败项攒到这里统一报。
    // **仍然返回 Err** —— 退出码非 0，脚本能感知；但其余文件都已下完。
    if !failed.is_empty() {
        bail!(
            "{} 个文件失败（其余 {} 个已完成）：\n  {}",
            failed.len(),
            done,
            failed.join("\n  ")
        );
    }
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
    // 落点必须过 `pathsafe`：`job.path` 可能来自 URL 末段、整合包清单或 CurseForge 的
    // `file_name` —— 全是不可信输入，而 `PathBuf::join` 遇到绝对路径会**整体替换**，
    // 光看"像不像相对路径"挡不住。
    let dst = crate::pathsafe::under_root(root, Path::new(&job.path))
        .with_context(|| format!("目标路径不安全，拒绝落盘: {}", job.path))?;

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
        // 限速点：**真实下载的字节**才扣令牌（续传补记进度不走这里）
        pg.add_throttled(chunk.len() as u64).await;
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
    // 降级（服务器回 200 → 退回单流）时要把进度拨回这个值：那次分片尝试的字节不算数。
    let done_before = pg.done();
    let mut table = PieceTable::with_conns(size, opts.piece_conns);
    if !table.is_split() {
        return one_stream(client, job, dst, pg).await;
    }

    let tmp = part_path(dst);

    // 续传：`.part` 与 `.part.state` 同时有效才认这份进度。
    // 只有 `.part` 没有状态文件时无法知道哪些片段完整（稀疏文件的空洞读出来是 0），
    // 只能从头下 —— 宁可重下，不要拼出坏文件。
    if tmp.exists() {
        // 必须用**实际** piece_conns 重建判据：片大小是 size/conns 的函数，
        // 用默认 4 去对照会让 `--piece-conns 8` 这类配置的续传永远失效（静默重下）。
        match PieceState::load_with_conns(&tmp, size, opts.piece_conns) {
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
    // 关键观测点：片数由 `PieceTable::with_conns` 按 size/conns 算，夹逼后仍可能少于 conns
    // （小文件受 MIN_PIECE 限制）。这里如实打印"实际并发"，别让用户以为配了 32 就真开了 32。
    println!(
        "[*] 分片：{n_pieces} 片 · 实际并发 {conns}（配置 {}；片大小 {piece_size} 字节）",
        opts.piece_conns
    );
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
                // 本分片已试过的源：**每片一份**，重试时据此换源（不能提到 one_split 里共享）
                let mut tried: Vec<usize> = Vec::new();
                let mut attempt = 0u32;
                let mut fatal: Option<anyhow::Error> = None;
                loop {
                    match fetch_piece(&c, &pj, &t, size, &pg, &pool, &mut tried).await {
                        Ok(()) => {
                            tbl.lock().await.mark_done(pj.piece_idx);
                            break;
                        }
                        Err(e) => {
                            // 服务器回 200（整份）：重试只会再撞一次，直接把标记抛上去让
                            // `one_split` 降级成单流。
                            if e.downcast_ref::<RangeUnsupported>().is_some() {
                                fatal = Some(e);
                                break;
                            }
                            // 无源可用 = 不可恢复：退避只会原地再撞一次，别白等
                            if e.downcast_ref::<SourcesExhausted>().is_some() {
                                fatal = Some(e.context(format!("分片 {} 无可用源", pj.piece_idx)));
                                break;
                            }
                            if attempt >= retries {
                                fatal = Some(e.context(format!("分片 {} 重试耗尽", pj.piece_idx)));
                                break;
                            }
                            tokio::time::sleep(Duration::from_secs(1u64 << attempt)).await;
                            attempt += 1;
                        }
                    }
                }
                // 闸门槽只在这一处释放 —— 两个失败出口 + 成功出口共用它
                if let Some((_, n)) = &gate {
                    n.fetch_sub(1, Ordering::Relaxed);
                }
                if let Some(e) = fatal {
                    return Err(e);
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
                } else if delta * 4 < last_delta * 3 && budget > 1 && t - last_cut >= 2 {
                    // 只有「掉幅 > 25%」才算拥塞信号：`delta*4 < last_delta*3` ⇔ `delta < 0.75×last`。
                    // 不设阈值时 delta 掉 0.2%（本身就是噪声）就会腰斩并发 —— 批次 5 的 add/cut
                    // 锯齿（cut=4 / add=22）很可能就是它自己抖出来的。整数乘避免浮点。
                    // 另外仍保留「减半后两个 tick 内不再减」的防抖：阈值管"单次掉幅"，防抖管"频率"。
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

    // 逐个收 worker。用 `pop()` 而不是 `for`：出错时**余下的 handle 必须还在 Vec 里**，
    // 否则 `break` 会把它们 drop 掉（JoinHandle 的 drop 是 detach，不是 abort），
    // 那些 worker 会继续往 `.part` 里写 —— 而降级分支正要截断这个文件重下。
    let mut worker_err: Option<anyhow::Error> = None;
    while let Some(h) = handles.pop() {
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
        // 降级：服务器对 Range 回 200（整份）⇒ 退回单流，而不是把整批判失败。
        //
        // 顺序不能换：先把还在跑的 worker 掐掉，再让 `one_stream` 去 `File::create`
        // 截断 `.part`（它从头写）；否则晚到的分片写会把新流搅坏。
        // 已计的字节要拨回去（本次下载从 0 重来），否则进度会超过总量。
        if e.downcast_ref::<RangeUnsupported>().is_some() {
            for h in &handles {
                h.abort();
            }
            pg.reset_to(done_before);
            PieceState::remove(&tmp);
            println!("[*] 服务器对 Range 回 200（整份），已退回单流下载");
            return one_stream(client, job, dst, pg).await;
        }
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

/// 开区间 `[start, end_excl)` → Range / `Content-Range` 用的闭区间 `(start, end_incl)`。
///
/// **算一次、用两次**：请求头与 `Content-Range` 校验共用这一个换算。
/// 若两处各写一次 `end - 1`，任一侧单独改动都会让**所有**分片被判"区间不符"，
/// 而纯字符串单测发现不了 —— 这是唯一能锁住它的写法。
fn byte_range(start: u64, end_excl: u64) -> (u64, u64) {
    (start, end_excl - 1)
}

/// 校验 `Content-Range` 覆盖的区间与请求的分片一致。
///
/// 为什么需要：只校验 `206` + 长度**抓不到"起点错"**——服务器回了 206、
/// 长度也对，但区间起点不是请求的位置（CDN 缓存错位 / 多源镜像配错 / 代理改写），
/// 数据就会被写到错误偏移上：不报错、文件大小正确、只有 sha1 错，极难查。
///
/// `end_incl` 是**闭区间**终点；调用方传 `pj.end - 1`（`PieceJob.end` 是开区间）。
fn check_content_range(
    header: &str,
    start: u64,
    end_incl: u64,
    expect_total: Option<u64>,
) -> anyhow::Result<()> {
    let raw = header.trim();
    // 单位大小写不敏感（ABNF 字面量如此，`BYTES` 也合法）。
    // `bytes=100-199/1000` 没有空白 ⇒ unit 变成整串 ⇒ 不等于 bytes ⇒ 照样报错。
    let (unit, after_unit) = raw
        .split_once(|c: char| c.is_ascii_whitespace())
        .unwrap_or((raw, ""));
    if !unit.eq_ignore_ascii_case("bytes") {
        bail!("Content-Range 缺少 bytes 单位: {raw}");
    }
    let rest = after_unit.trim_start();
    let (range, total) = rest
        .split_once('/')
        .with_context(|| format!("Content-Range 缺少 `/总长`: {raw}"))?;
    let (s_str, e_str) = range
        .split_once('-')
        .with_context(|| format!("Content-Range 区间缺少 `-`: {raw}"))?;
    let got_start: u64 = s_str
        .trim()
        .parse()
        .with_context(|| format!("Content-Range 起点解析失败: {raw}"))?;
    let got_end: u64 = e_str
        .trim()
        .parse()
        .with_context(|| format!("Content-Range 终点解析失败: {raw}"))?;
    // 总长：格式总是校验；**给了 `expect_total` 才比对**。
    // 之前这里只校验格式（理由是"拿不到权威 file_size、也不该为它改签名"）——
    // 现在调用方（`one_split`）手里真有权威 `size`，所以把比对补上：
    // 远端对象在下载过程中被换掉时，`total` 会变，这一条能直接报「大小变了」，
    // 而不是等到最后全文件 SHA1 失败时让用户去猜"是我算错了还是源站换了文件"。
    let total_raw = total.trim();
    let parsed_total = if total_raw == "*" {
        None // `*` 未知 ⇒ 跳过比对
    } else {
        Some(
            total_raw
                .parse::<u64>()
                .with_context(|| format!("Content-Range 总长解析失败: {raw}"))?,
        )
    };

    if got_start != start || got_end != end_incl {
        bail!(
            "Content-Range 与请求的分片不符: 服务器回 {got_start}-{got_end}，\
             请求 {start}-{end_incl}，原始 header: {raw}"
        );
    }
    if let (Some(got), Some(expect)) = (parsed_total, expect_total)
        && got != expect
    {
        bail!("远端文件大小变了: Content-Range 总长 {got}，期望 {expect}（原始 header: {raw}）");
    }
    Ok(())
}

/// 所有源都试过了 —— 不可恢复，退避重试只会原地再撞一次。
///
/// 用它当标记类型而不是匹配错误文案：文案会改，类型不会。
#[derive(Debug)]
struct SourcesExhausted;

impl std::fmt::Display for SourcesExhausted {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("所有源都试过了")
    }
}

impl std::error::Error for SourcesExhausted {}

/// 服务器对 `Range` 请求回了 **200 整份**（而不是 206）。
///
/// 这是**可降级**的错误：`one_split` 抓到它就退回 `one_stream` —— 200 的 body 本来就是整份，
/// 单流照样能下完。别的非 206 状态码仍然照常 `record_failure` + 报错。
///
/// 用类型而不是文案当标记：文案会改，类型不会（同 `SourcesExhausted`）。
///
/// ⚠️ 已知边界：退回前会 abort 掉其余 worker，但**已经在 `spawn_blocking` 里落盘的那一次写
/// 停不下来**。真实触发场景（服务器一律回 200）里一片都写不出去（每片都在 `send()` 拿到状态码后
/// 就退出），所以这条竞态只在"同一个源时而 206 时而 200"的畸形服务器上才可能露出；
/// 代价写成注释留在这里，而不是假装它不存在。
#[derive(Debug)]
struct RangeUnsupported {
    url: String,
}

impl std::fmt::Display for RangeUnsupported {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "服务器对 Range 回 200（整份），无法分片: {}", self.url)
    }
}

impl std::error::Error for RangeUnsupported {}

/// 拉一个分片并写到指定偏移。
///
/// `size` 是整文件的权威总长：用来比对 `Content-Range` 的总长，抓"远端对象中途被换掉"。
async fn fetch_piece(
    client: &reqwest::Client,
    pj: &PieceJob,
    path: &Path,
    size: u64,
    pg: &Arc<Progress>,
    pool: &Arc<SourcePool>,
    tried: &mut Vec<usize>,
) -> Result<()> {
    // 每次尝试都现选源：`tried` 为空时等价于 `pick()`（轮转 + 择优）。
    // 这样前几片记录的 ewma 才能影响后几片的选择 —— 建 job 时选死就学不到东西。
    // 只抛标记类型、不在这儿加 context：外层重试循环会补上「分片 N 无可用源」，
    // 两处都加会让错误链里出现两次「分片 N」。
    let idx = pool
        .pick_excluding(tried)
        .ok_or_else(|| anyhow::Error::new(SourcesExhausted))?;
    // 尝试**之前**就记下：失败也必须算"这个源试过了"，否则会原地重试到耗尽
    tried.push(idx);
    let url = pool
        .url_of(idx)
        .ok_or_else(|| anyhow::anyhow!("源 {idx} 不存在"))?;
    let t0 = std::time::Instant::now();

    // 请求与校验共用同一个开→闭换算，避免两处各自 `pj.end - 1` 而漂移
    let (req_start, req_end_incl) = byte_range(pj.start, pj.end);

    let resp = client
        .get(&url)
        .header(
            reqwest::header::RANGE,
            format!("bytes={req_start}-{req_end_incl}"),
        )
        .send()
        .await?;

    if resp.status() != reqwest::StatusCode::PARTIAL_CONTENT {
        let status = resp.status();
        if status == reqwest::StatusCode::OK {
            // 200 = 服务器把整份发过来了。**这不是错误，是可降级**：`one_split` 抓到
            // 这个标记就退回 `one_stream`（200 的 body 本来就是整份）。
            // 刻意**不** record_failure：源没问题，只是不支持分片。
            return Err(anyhow::Error::new(RangeUnsupported { url }));
        }
        if status == reqwest::StatusCode::RANGE_NOT_SATISFIABLE {
            // 416 = "这个区间我给不出来"，**恰恰说明它支持 Range**。
            // 常见成因是远端对象变小/被换掉了（我们的 `size` 比它大），
            // 所以别报成"不支持 Range"——那句话会把排查引到完全相反的方向。
            pool.record_failure(idx);
            anyhow::bail!(
                "服务器回 416（区间不可满足）：远端文件可能已变小或被替换 \
                 （请求 bytes={req_start}-{req_end_incl}）: {url}"
            );
        }
        pool.record_failure(idx);
        anyhow::bail!("服务器不支持 Range（HTTP {status}），无法分片: {url}");
    }

    // 206 + 长度对上还不够：区间起点错时长度照样对，数据会被写到错误偏移。
    //
    // 下面三条失败出口都先 `record_failure` 再响亮报错，与上面"不支持 Range"那条一致。
    // 现在重试会换源（`tried` 累积已试过的下标），所以 record_* 记的是**实际用到的 idx**。
    // 注意 `pick()` 只在建 job 时调一次，故"本轮降权影响后续 job"仍不成立 ——
    // 那是惰性派发的事，不是这里的。
    let cr = resp
        .headers()
        .get(reqwest::header::CONTENT_RANGE)
        .ok_or_else(|| anyhow::anyhow!("服务器未返回 Content-Range（206 必须带它）: {url}"))
        .inspect_err(|_| pool.record_failure(idx))?
        .to_str()
        .with_context(|| format!("Content-Range 不是合法 ASCII: {url}"))
        .inspect_err(|_| pool.record_failure(idx))?;
    if let Err(e) = check_content_range(cr, req_start, req_end_incl, Some(size)) {
        pool.record_failure(idx);
        return Err(e)
            .with_context(|| format!("分片 {} 的 Content-Range 校验失败: {url}", pj.piece_idx));
    }

    let expect = (pj.end - pj.start) as usize;
    let mut buf = Vec::with_capacity(expect);
    let mut stream = resp.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk?;
        // 限速点：分片 worker 也过同一个令牌桶 ⇒ 总带宽是全局上限，不是每连接各一份
        pg.add_throttled(chunk.len() as u64).await;
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

    pool.record_success(idx, expect as u64, t0.elapsed().as_secs_f64());
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

    // `split_for` 的用例已迁到 `tests/params_split_for.rs`（rstest 参数化）：
    //   边界 0 / 1 / 4 / 16 / 17 / 32 / usize::MAX 等 9 个 case，外加两个属性断言
    //   （每任务至少 1 条、且不超过原并发）。原来这里两个手写函数做的事完全同构，
    //   加一个边界就要再写一个函数 —— 迁移的目的不是"少写几行"，是让加用例变成加一行。

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

    #[test]
    fn byte_range_converts_exclusive_end_to_inclusive() {
        assert_eq!(byte_range(100, 200), (100, 199));
        assert_eq!(byte_range(0, 1), (0, 0));
    }

    #[test]
    fn content_range_accepts_matching_range() {
        assert!(check_content_range("bytes 100-199/1000", 100, 199, None).is_ok());
    }

    #[test]
    fn content_range_rejects_wrong_start() {
        let e = check_content_range("bytes 100-199/1000", 101, 199, None).unwrap_err();
        let msg = format!("{e:#}");
        assert!(msg.contains("100-199"), "错误里要有服务器回的区间: {msg}");
        assert!(msg.contains("101-199"), "错误里要有请求的区间: {msg}");
    }

    #[test]
    fn content_range_rejects_wrong_end() {
        let e = check_content_range("bytes 100-199/1000", 100, 198, None).unwrap_err();
        let msg = format!("{e:#}");
        assert!(msg.contains("100-199"), "错误里要有服务器回的区间: {msg}");
        assert!(msg.contains("100-198"), "错误里要有请求的区间: {msg}");
    }

    #[test]
    fn content_range_accepts_unknown_total() {
        assert!(check_content_range("bytes 100-199/*", 100, 199, None).is_ok());
        // 即便调用方知道权威总长，`*` 也跳过比对：服务器说"我不知道"，不是矛盾
        assert!(check_content_range("bytes 100-199/*", 100, 199, Some(1000)).is_ok());
    }

    #[test]
    fn content_range_rejects_bad_total() {
        assert!(check_content_range("bytes 100-199/abc", 100, 199, None).is_err());
        assert!(check_content_range("bytes 100-199/", 100, 199, None).is_err());
    }

    #[test]
    fn content_range_accepts_uppercase_unit() {
        // ABNF 字面量大小写不敏感，`BYTES` / `Bytes` 都是合法格式
        assert!(check_content_range("BYTES 100-199/1000", 100, 199, None).is_ok());
        assert!(check_content_range("Bytes 100-199/*", 100, 199, None).is_ok());
    }

    #[test]
    fn content_range_rejects_equals_separator() {
        // `bytes=` 不是合法格式（RFC 7233 是 `bytes <s>-<e>/<total>`）
        assert!(check_content_range("bytes=100-199/1000", 100, 199, None).is_err());
    }

    #[test]
    fn content_range_rejects_garbage_without_panic() {
        assert!(check_content_range("garbage", 0, 0, None).is_err());
    }

    #[test]
    fn content_range_checks_total_against_expected_size() {
        // 与调用方给的权威 size 一致 ⇒ 通过
        assert!(check_content_range("bytes 100-199/1000", 100, 199, Some(1000)).is_ok());
        // 不一致 ⇒ 精确报「大小变了」，而不是等到全文件 sha1 失败时让人猜是谁的错
        let e = check_content_range("bytes 100-199/1001", 100, 199, Some(1000)).unwrap_err();
        let msg = format!("{e:#}");
        assert!(
            msg.contains("远端文件大小变了"),
            "要能看出是源站换了文件: {msg}"
        );
        assert!(
            msg.contains("1001") && msg.contains("1000"),
            "服务器总长与期望总长都要出现在错误里: {msg}"
        );
        // 区间已经错了时优先报区间（更具体），不要被总长那条抢先
        let e = check_content_range("bytes 100-199/1001", 101, 199, Some(1000)).unwrap_err();
        assert!(format!("{e:#}").contains("与请求的分片不符"));
    }

    #[test]
    fn sources_exhausted_survives_context() {
        let e = anyhow::Error::new(SourcesExhausted).context("分片 7");
        assert!(
            e.downcast_ref::<SourcesExhausted>().is_some(),
            "套了 context 之后仍要能 downcast —— 否则不可恢复错误会被误当可重试"
        );
        let other: anyhow::Error = anyhow::anyhow!("别的错");
        assert!(other.downcast_ref::<SourcesExhausted>().is_none());
    }
}
