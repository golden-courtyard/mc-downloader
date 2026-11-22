//! HTTP 语义层畸形响应的集成测试（issue #2，方案 B）。
//!
//! ## 为什么是这份文件
//!
//! `scripts/net_faults/fakeorigin.py` 只做一个**正确**的服务器，故障交给 netem / iptables。
//! 但「206 的 Content-Range 与请求不符」「Content-Length 说谎」「发一半就断」这几类
//! **不需要真实网络**——没有丢包、没有延迟，只有「服务器说的话和做的事不一致」。
//! 起一个 `std::net::TcpListener` 线程就能精确复现：毫秒级、跨平台、能进 CI
//! （net_faults 那套要 Linux + 特权，进不了 CI）。
//!
//! ## 被测入口
//!
//! 只用公共 API：`download::run(&[Job], root, &DownloadOpts)`。**刻意不直接调
//! `check_content_range` / `header_content_length`**：那是 `src/download.rs` 单测已覆盖的
//! 函数级逻辑；这里要证明的是「畸形响应进不了成品文件」——那是整条路径的事。
//!
//! ## 判据
//!
//! 每条测试都断言**具体错误分支**独有的字样（`Content-Range 校验失败` /
//! `长度不符` / `服务器不支持 Range` …），而不是「run 返回 Err 就算过」——
//! 只断言 Err 会把下游的 `sha1 不匹配` 兜底误当成「语义层已经拦住了」。
//!
//! ## 参数为什么不是 `DownloadOpts::default()`
//!
//! * `retries = 0`：重试与指数退避（1s/2s/4s）是**另一层**策略。开着它，每个失败用例
//!   多花 7 秒以上，语义层的错误还会被埋在「重试耗尽」的外层 context 里。
//! * `timeout_secs = 2`：只作用于建连与单次读，本地环回足够，又把 lying-length 的
//!   等待压到 2 秒。其余字段保持默认（`piece_conns = 4`）。
//!
//! 除 retries / timeout 外全部取默认值，所以 10 MiB 载荷会被切成
//! 4 片 × 2.5 MiB（`PieceTable::with_conns`）；下面 `PIECE` 就锁这个数。

use std::fs;
use std::io::{self, Read, Write};
use std::net::{Ipv4Addr, SocketAddr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use mc_downloader::download::{DownloadOpts, run};
use mc_downloader::version::Job;
use sha1::{Digest, Sha1};

// ─────────────────────────── 常量 ───────────────────────────

/// 10 MiB：>= `SPLIT_THRESHOLD`（8 MiB），默认 4 连接下切成 4 片。
const PAYLOAD: usize = 10 * 1024 * 1024;
/// 单片长度：10 MiB / 4 连接 = 2.5 MiB。2.5 MiB 是 4 KiB 的整数倍（640 页），
/// 不会被 `floor_to_page` 改动。lying-length 偏小的断言把它写死，防「片大小悄悄变了」。
const PIECE: usize = 2_621_440;
/// `LyingSmall` 实际发出的字节数：只发 1 KiB，却声明/请求的是整个分片。
const SMALL_BODY: usize = 1024;
/// `LyingLarge` 声明的 Content-Length 比实际多出的字节数。
const LIE_EXTRA: usize = 4 * 1024;
/// `ShortRange` 把 Content-Range 起点错开多少字节（错 7 字节就足以写错偏移）。
const SHORT_RANGE_SHIFT: usize = 7;
/// lying-length 偏大时「发完不关连接」要撑住的时长；必须大于测试用的读超时（2s）。
const HOLD_OPEN: Duration = Duration::from_secs(6);

// ─────────────────────────── 临时目录 ───────────────────────────

/// 每个测试自建的临时目录，`Drop` 时递归删除（断言 panic 时同样会清）。
struct TempDir {
    path: PathBuf,
}

impl TempDir {
    fn new(tag: &str) -> Self {
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("系统时间早于 UNIX_EPOCH")
            .as_nanos();
        let path =
            std::env::temp_dir().join(format!("mcd-http-sem-{}-{tag}-{stamp}", std::process::id()));
        fs::create_dir_all(&path).expect("创建测试临时目录失败");
        Self { path }
    }

    fn path(&self) -> &Path {
        &self.path
    }

    fn join(&self, name: &str) -> PathBuf {
        self.path.join(name)
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        // 清理失败不该掩盖判据本身（删不掉时出声即可）。重试几次是因为
        // 运行时里的连接/文件句柄可能比测试主体晚一拍才释放（Windows 上尤其）。
        for _ in 0..10 {
            match fs::remove_dir_all(&self.path) {
                Ok(()) => return,
                Err(e) if e.kind() == io::ErrorKind::NotFound => return,
                Err(_) => thread::sleep(Duration::from_millis(20)),
            }
        }
        eprintln!(
            "[!] 清理临时目录失败（不影响判据）: {}",
            self.path.display()
        );
    }
}

// ─────────────────────── 本地畸形 HTTP 服务 ───────────────────────

/// 六类「服务器说的话和做的事不一致」。
#[derive(Clone, Copy, Debug)]
enum Fault {
    /// 206 + 长度正确，但 `Content-Range` 起点比请求的位置多 7 字节。
    ShortRange,
    /// `Content-Length` 比实际发送多 4 KiB，且发完**不关连接**。
    LyingLarge,
    /// `Content-Length` 只有 1 KiB（请求的是 2.5 MiB 的整个分片）。
    LyingSmall,
    /// 声明整片长度，只发一半就关连接。
    TruncateMid,
    /// 声明 `Accept-Ranges: bytes`，却对 Range 请求回 200 整份（#6：客户端应降级单流）。
    AcceptRangesBut200,
    /// 206 且区间正确，但 `Content-Range` 总长比真文件多 1（#7A：远端对象被换掉）。
    WrongTotal,
}

/// 最小 HTTP 服务：只解析请求头里的 `Range:`，按 [`Fault`] 回畸形响应。
///
/// 端口用 `127.0.0.1:0` 让系统分配，避免并行测试撞端口。
struct FaultServer {
    addr: SocketAddr,
    stop: Arc<AtomicBool>,
    accept: Option<JoinHandle<()>>,
}

impl FaultServer {
    fn start(fault: Fault, payload: Arc<Vec<u8>>) -> Self {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).expect("绑定 127.0.0.1:0 失败");
        let addr = listener.local_addr().expect("读取监听地址失败");
        listener
            .set_nonblocking(true)
            .expect("设置 accept 非阻塞失败");
        let stop = Arc::new(AtomicBool::new(false));
        let stop_flag = stop.clone();
        let accept = thread::spawn(move || {
            while !stop_flag.load(Ordering::SeqCst) {
                match listener.accept() {
                    Ok((stream, _peer)) => {
                        let payload = payload.clone();
                        // 分片下载会同时开 piece_conns 条连接 ⇒ 每连接一个线程
                        thread::spawn(move || {
                            if let Err(e) = serve(stream, fault, &payload) {
                                eprintln!("[srv:{fault:?}] 连接处理失败: {e}");
                            }
                        });
                    }
                    Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(5));
                    }
                    Err(_) => break,
                }
            }
        });
        Self {
            addr,
            stop,
            accept: Some(accept),
        }
    }

    fn url(&self, name: &str) -> String {
        format!("http://127.0.0.1:{}/{name}", self.addr.port())
    }
}

impl Drop for FaultServer {
    fn drop(&mut self) {
        // 唤醒 accept 循环（它每 5ms 自己也会看一次 flag），然后收摊
        self.stop.store(true, Ordering::SeqCst);
        if let Some(h) = self.accept.take() {
            let _ = h.join();
        }
    }
}

/// 读请求头：读到 `\r\n\r\n` 或 16 KiB 为止。这里不需要请求体。
fn read_request_head(stream: &mut TcpStream) -> io::Result<String> {
    let mut buf = Vec::new();
    let mut chunk = [0u8; 512];
    loop {
        let n = stream.read(&mut chunk)?;
        if n == 0 {
            break;
        }
        buf.extend_from_slice(&chunk[..n]);
        if buf.len() > 16 * 1024 || buf.windows(4).any(|w| w == b"\r\n\r\n") {
            break;
        }
    }
    Ok(String::from_utf8_lossy(&buf).into_owned())
}

/// 从请求头里取 `Range: bytes=<start>-<end>`（闭区间，转成开区间返回）。
fn requested_range(head: &str, len: usize) -> Option<(usize, usize)> {
    for line in head.lines() {
        let Some((name, value)) = line.split_once(':') else {
            continue;
        };
        if !name.trim().eq_ignore_ascii_case("range") {
            continue;
        }
        let spec = value.trim().strip_prefix("bytes=")?;
        let (start, end_incl) = spec.split_once('-')?;
        let start: usize = start.trim().parse().ok()?;
        let end_incl: usize = end_incl.trim().parse().ok()?;
        return Some((start, (end_incl + 1).min(len)));
    }
    None
}

fn write_response_head(
    stream: &mut TcpStream,
    status: u16,
    reason: &str,
    headers: &[(&str, String)],
) -> io::Result<()> {
    let mut out = format!("HTTP/1.1 {status} {reason}\r\n");
    for (name, value) in headers {
        out.push_str(&format!("{name}: {value}\r\n"));
    }
    out.push_str("\r\n");
    stream.write_all(out.as_bytes())
}

fn serve(mut stream: TcpStream, fault: Fault, payload: &[u8]) -> io::Result<()> {
    // Windows 上 accept 出来的 socket 会继承监听端的非阻塞模式 ⇒ 请求头还没到就
    // WouldBlock，serve 提前返回把连接一关，客户端只会看到 10053（连接被中止）。
    // 显式改回阻塞，让「等请求头」变成真的等。
    stream.set_nonblocking(false)?;
    stream.set_read_timeout(Some(Duration::from_secs(5)))?;
    let head = read_request_head(&mut stream)?;
    let range = requested_range(&head, payload.len());
    let total = payload.len();

    match fault {
        Fault::AcceptRangesBut200 => {
            // 模拟「忽略 Range」的服务器：对 Range 请求也回 200 + **整份** body。
            //
            // #6 的判据是「降级后整份能下完」⇒ 必须发完整 payload。早先这里只发 4 KiB
            //（理由是避免客户端没解析完 head 就吃到 RST），但那会让降级测试变成假绿：
            // 降级后的单流最多只拿到 4 KiB，sha1 永远对不上。
            //
            // 第一批 Range 请求的客户端拿到 head 就退出并 abort 连接，那几条连接上的写
            // 会失败（`let _ =` 忽略），不影响降级后的那条新连接。
            let headers = [
                ("Accept-Ranges", "bytes".to_string()),
                ("Content-Length", total.to_string()),
                ("Connection", "close".to_string()),
            ];
            write_response_head(&mut stream, 200, "OK", &headers)?;
            let _ = stream.write_all(payload);
        }
        Fault::WrongTotal => {
            // 区间完全正确、只有总长多 1：用来锁「总长与 job.size 比对」这条分支，
            // 而不是让它落到最后的全文件 sha1 失败。
            let Some((start, end_excl)) = range else {
                return Ok(());
            };
            let body = &payload[start..end_excl];
            let wrong_total = total + 1;
            let headers = [
                (
                    "Content-Range",
                    format!("bytes {start}-{}/{wrong_total}", end_excl - 1),
                ),
                ("Content-Length", body.len().to_string()),
                ("Connection", "close".to_string()),
            ];
            write_response_head(&mut stream, 206, "Partial Content", &headers)?;
            let _ = stream.write_all(body);
        }
        Fault::ShortRange => {
            let Some((start, end_excl)) = range else {
                return Ok(());
            };
            let body = &payload[start..end_excl];
            let wrong_start = start + SHORT_RANGE_SHIFT;
            let wrong_end = wrong_start + body.len() - 1;
            let headers = [
                (
                    "Content-Range",
                    format!("bytes {wrong_start}-{wrong_end}/{total}"),
                ),
                ("Content-Length", body.len().to_string()),
                ("Connection", "close".to_string()),
            ];
            write_response_head(&mut stream, 206, "Partial Content", &headers)?;
            let _ = stream.write_all(body);
        }
        Fault::LyingLarge => {
            let Some((start, end_excl)) = range else {
                return Ok(());
            };
            let body = &payload[start..end_excl];
            let declared = body.len() + LIE_EXTRA;
            let headers = [
                (
                    "Content-Range",
                    format!("bytes {start}-{}/{total}", end_excl - 1),
                ),
                ("Content-Length", declared.to_string()),
                ("Connection", "close".to_string()),
            ];
            write_response_head(&mut stream, 206, "Partial Content", &headers)?;
            let _ = stream.write_all(body);
            let _ = stream.flush();
            // 关键：**不关连接、也不再发字节** —— 客户端只能靠读超时发现不对
            thread::sleep(HOLD_OPEN);
        }
        Fault::LyingSmall => match range {
            Some((start, end_excl)) => {
                let body = &payload[start..end_excl.min(start + SMALL_BODY)];
                let headers = [
                    (
                        "Content-Range",
                        format!("bytes {start}-{}/{total}", end_excl - 1),
                    ),
                    ("Content-Length", body.len().to_string()),
                    ("Connection", "close".to_string()),
                ];
                write_response_head(&mut stream, 206, "Partial Content", &headers)?;
                let _ = stream.write_all(body);
            }
            None => {
                let body = &payload[..payload.len().min(SMALL_BODY)];
                let headers = [
                    ("Content-Length", body.len().to_string()),
                    ("Connection", "close".to_string()),
                ];
                write_response_head(&mut stream, 200, "OK", &headers)?;
                let _ = stream.write_all(body);
            }
        },
        Fault::TruncateMid => {
            let Some((start, end_excl)) = range else {
                return Ok(());
            };
            let span = end_excl - start;
            let headers = [
                (
                    "Content-Range",
                    format!("bytes {start}-{}/{total}", end_excl - 1),
                ),
                ("Content-Length", span.to_string()),
                ("Connection", "close".to_string()),
            ];
            write_response_head(&mut stream, 206, "Partial Content", &headers)?;
            let _ = stream.write_all(&payload[start..start + span / 2]);
            // 直接返回：连接关闭，但 Content-Length 还差 span/2 没发
        }
    }
    Ok(())
}

// ─────────────────────────── 脚手架 ───────────────────────────

/// 生成非平凡载荷：每个位置都是 (i*31 + seed*17) % 251 + 1 ∈ [1, 251]，
/// 不会出现「整片全 0」——那是稀疏 `.part` 空洞的样子，会和真内容混在一起。
fn pattern(len: usize, seed: u8) -> Vec<u8> {
    (0..len)
        .map(|i| (((i as u64 * 31 + seed as u64 * 17) % 251) + 1) as u8)
        .collect()
}

fn sha1_hex(data: &[u8]) -> String {
    hex::encode(Sha1::digest(data))
}

fn sha1_of_file(path: &Path) -> io::Result<String> {
    let mut f = fs::File::open(path)?;
    let mut hasher = Sha1::new();
    let mut buf = vec![0u8; 128 * 1024];
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(hex::encode(hasher.finalize()))
}

fn job_with_sha1(url: &str, payload: &[u8]) -> Job {
    Job {
        url: url.to_string(),
        path: "out.bin".to_string(),
        sha1: Some(sha1_hex(payload)),
        size: Some(payload.len() as u64),
        mirrors: Vec::new(),
    }
}

fn test_opts() -> DownloadOpts {
    DownloadOpts {
        retries: 0,
        timeout_secs: 2,
        ..DownloadOpts::default()
    }
}

/// 跑一次下载并**要求失败**；成功本身就是缺陷（残缺响应被当成品）。
async fn expect_error(root: &Path, job: &Job, label: &str) -> anyhow::Error {
    match run(std::slice::from_ref(job), root, &test_opts()).await {
        Ok(()) => panic!("{label}: 期望 Err，但 run 返回 Ok —— 残缺响应被当成了成品"),
        Err(e) => e,
    }
}

/// 「失败不落成品」：成品路径不存在；若留下 `.part`，其内容**不是**完整载荷
/// （大小看起来对也不算数——这正是「看起来完整」的残缺文件）。
fn assert_no_complete_artifact(root: &Path, name: &str, payload_sha1: &str, label: &str) {
    let dst = root.join(name);
    assert!(
        !dst.exists(),
        "{label}: 失败后不应落下成品 {} —— 残缺数据被 rename 成了最终文件",
        dst.display()
    );
    let part = root.join(format!("{name}.part"));
    if part.exists() {
        let got = sha1_of_file(&part).expect("读取 .part 失败");
        assert_ne!(
            got, payload_sha1,
            "{label}: .part 的内容与完整载荷一致 —— 这正是「看起来完整」的残缺文件"
        );
    }
}

/// 错误链里是否有 reqwest 的超时错误（结构判定，不靠错误文案）。
fn has_reqwest_timeout(err: &anyhow::Error) -> bool {
    err.chain().any(|cause| {
        cause
            .downcast_ref::<reqwest::Error>()
            .is_some_and(|e| e.is_timeout())
    })
}

/// 错误链里是否有 reqwest 的传输/解码层错误。
fn has_reqwest_error(err: &anyhow::Error) -> bool {
    err.chain()
        .any(|cause| cause.downcast_ref::<reqwest::Error>().is_some())
}

// ─────────────────────────── 六类故障 ───────────────────────────

/// 故障 1：206 + 长度正确，但 `Content-Range` 起点不是请求的位置。
///
/// 这条最能说明 `check_content_range` 为什么必须存在：只校验 206 + 长度**抓不到
/// 「起点错」**——数据会被写到错误偏移，不报错、文件大小正确，只有 sha1 错。
///
/// 断言：错误链里有 `Content-Range 校验失败`（分片层的出口）与
/// `Content-Range 与请求的分片不符`（具体原因，带服务器回的区间与请求的区间）；
/// 成品不落地，`.part` 内容不完整。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn short_range_is_rejected_before_any_write() {
    const LABEL: &str = "short-range";
    let dir = TempDir::new("short-range");
    let payload = Arc::new(pattern(PAYLOAD, 1));
    let expect_sha1 = sha1_hex(&payload);
    let server = FaultServer::start(Fault::ShortRange, payload.clone());
    let job = job_with_sha1(&server.url("blob.bin"), &payload);

    let err = expect_error(dir.path(), &job, LABEL).await;
    let msg = format!("{err:#}");
    println!("[{LABEL}] {msg}");

    assert!(
        msg.contains("Content-Range 校验失败"),
        "{LABEL}: 必须走到「分片 Content-Range 校验失败」这条出口，实际: {msg}"
    );
    assert!(
        msg.contains("Content-Range 与请求的分片不符"),
        "{LABEL}: 错误里要能看出是「区间不符」（而不是缺单位/解析失败），实际: {msg}"
    );
    assert!(
        msg.contains("服务器回") && msg.contains("请求"),
        "{LABEL}: 错误里要同时带服务器回的区间与请求的区间，否则定位不到是哪个镜像错位，实际: {msg}"
    );
    assert!(
        msg.contains(&format!("/{PAYLOAD}")),
        "{LABEL}: Content-Range 的总长（{PAYLOAD}）应出现在原始 header 里，实际: {msg}"
    );
    assert_no_complete_artifact(dir.path(), "out.bin", &expect_sha1, LABEL);
}

/// 故障 2：`Content-Length` 比实际发送多 4 KiB，且发完**不关连接**。
///
/// 判据是「读超时后 Err」，用两条独立证据：
/// ① 错误链里有 reqwest 的**超时**错误（`is_timeout()`），既不是 EOF 也不是长度不符；
/// ② 耗时至少 1 秒——这么快就失败说明走的不是超时分支（配置的读超时是 2 秒）。
///
/// 反证也一起断言：成品不落地、`.part` 不完整。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn lying_large_length_times_out() {
    const LABEL: &str = "lying-large";
    let dir = TempDir::new("lying-large");
    let payload = Arc::new(pattern(PAYLOAD, 2));
    let expect_sha1 = sha1_hex(&payload);
    let server = FaultServer::start(Fault::LyingLarge, payload.clone());
    let job = job_with_sha1(&server.url("blob.bin"), &payload);

    let started = Instant::now();
    let err = expect_error(dir.path(), &job, LABEL).await;
    let elapsed = started.elapsed();
    let msg = format!("{err:#}");
    println!("[{LABEL}] 耗时 {elapsed:?}；{msg}");

    assert!(
        has_reqwest_timeout(&err),
        "{LABEL}: 错误链里必须有 reqwest 的超时错误（读超时分支），实际: {msg}"
    );
    assert!(
        msg.contains("重试耗尽"),
        "{LABEL}: 应当从分片 worker 的失败出口冒出来，实际: {msg}"
    );
    assert!(
        elapsed >= Duration::from_secs(1),
        "{LABEL}: 只在 {elapsed:?} 就失败，说明不是「读超时」分支（配置的是 2s）"
    );
    assert!(
        elapsed < Duration::from_secs(15),
        "{LABEL}: 耗时 {elapsed:?} 过长，读超时没有生效或连接没被掐断"
    );
    assert_no_complete_artifact(dir.path(), "out.bin", &expect_sha1, LABEL);
}

/// 故障 3：`Content-Length` 比实际发送**小**（发够就关），分两条路径各断言一条具体分支：
///
/// * 分片路径（>= 8 MiB）：`fetch_piece` 的「分片长度不符」必须拦住，
///   且错误里带实测数字（`1024 != 2621440`）；
/// * 单流路径（< 8 MiB）：`one_stream` 没有长度检查，**必须**由全文件 sha1 拦住。
///
/// 两条合起来才是「有 sha1 时不会静默接受」的完整证据。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn lying_small_length_is_not_accepted() {
    // ── 分片路径：长度检查 ──
    const LABEL: &str = "lying-small-split";
    let dir = TempDir::new("lying-small-split");
    let payload = Arc::new(pattern(PAYLOAD, 3));
    let expect_sha1 = sha1_hex(&payload);
    let server = FaultServer::start(Fault::LyingSmall, payload.clone());
    let job = job_with_sha1(&server.url("blob.bin"), &payload);

    let err = expect_error(dir.path(), &job, LABEL).await;
    let msg = format!("{err:#}");
    println!("[{LABEL}] {msg}");

    assert!(
        msg.contains("长度不符"),
        "{LABEL}: 必须走到「分片长度不符」这条出口，实际: {msg}"
    );
    assert!(
        msg.contains(&format!("{SMALL_BODY} != {PIECE}")),
        "{LABEL}: 错误里要带实际收到的字节数与应得字节数（{SMALL_BODY} != {PIECE}），实际: {msg}"
    );
    assert_no_complete_artifact(dir.path(), "out.bin", &expect_sha1, LABEL);

    // ── 单流路径：sha1 兜底 ──
    const STREAM_LABEL: &str = "lying-small-stream";
    let stream_dir = TempDir::new("lying-small-stream");
    let small_payload = Arc::new(pattern(1 << 20, 4));
    let stream_server = FaultServer::start(Fault::LyingSmall, small_payload.clone());
    let stream_job = job_with_sha1(&stream_server.url("small.bin"), &small_payload);

    let err2 = expect_error(stream_dir.path(), &stream_job, STREAM_LABEL).await;
    let msg2 = format!("{err2:#}");
    println!("[{STREAM_LABEL}] {msg2}");

    assert!(
        msg2.contains("sha1 不匹配"),
        "{STREAM_LABEL}: 单流路径没有长度检查，必须由 sha1 拦住（否则就是静默截断），实际: {msg2}"
    );
    assert!(
        !stream_dir.join("out.bin").exists(),
        "{STREAM_LABEL}: sha1 不匹配后不应落成品"
    );
}

/// 故障 4：声明整片长度，只发一半就关连接。
///
/// 判据：错误从**读 body 的传输层**冒出来（错误链里有 reqwest 错误），
/// 而不是被下游的长度检查顺手拦住——两者都会 Err，但只有前者证明
/// 「提前 EOF 会被当成失败」。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn truncate_mid_is_detected_while_reading_body() {
    const LABEL: &str = "truncate-mid";
    let dir = TempDir::new("truncate-mid");
    let payload = Arc::new(pattern(PAYLOAD, 5));
    let expect_sha1 = sha1_hex(&payload);
    let server = FaultServer::start(Fault::TruncateMid, payload.clone());
    let job = job_with_sha1(&server.url("blob.bin"), &payload);

    let err = expect_error(dir.path(), &job, LABEL).await;
    let msg = format!("{err:#}");
    println!("[{LABEL}] {msg}");

    assert!(
        has_reqwest_error(&err),
        "{LABEL}: 提前 EOF 必须在读 body 时就被发现（错误链里应有 reqwest 错误），实际: {msg}"
    );
    assert!(
        msg.contains("重试耗尽"),
        "{LABEL}: 应当从分片 worker 的失败出口冒出来，实际: {msg}"
    );
    assert!(
        !msg.contains("长度不符"),
        "{LABEL}: 被长度检查兜住会掩盖「传输层提前 EOF」这条分支，实际: {msg}"
    );
    assert_no_complete_artifact(dir.path(), "out.bin", &expect_sha1, LABEL);
}

/// 故障 5：声明 `Accept-Ranges: bytes`，却对 Range 请求回 200 整份（issue #6）。
///
/// 判据是「**降级**」而不是「报错」：200 的 body 本来就是整份，客户端应退回单流把文件
/// 完整下完（`run` 返回 Ok）。同时验证降级收尾干净：`.part` 不残留
///（降级前清 `.part.state`，再由 `one_stream` 的 `File::create` 截断重写）。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn range_answered_with_200_falls_back_to_stream() {
    const LABEL: &str = "range-200-fallback";
    let dir = TempDir::new("range-200-fallback");
    let payload = Arc::new(pattern(PAYLOAD, 6));
    let expect_sha1 = sha1_hex(&payload);
    let server = FaultServer::start(Fault::AcceptRangesBut200, payload.clone());
    let job = job_with_sha1(&server.url("blob.bin"), &payload);

    run(std::slice::from_ref(&job), dir.path(), &test_opts())
        .await
        .expect("服务器对 Range 回 200 时应降级为单流并下完，而不是报错");

    let got = fs::read(dir.join("out.bin")).expect("降级成功后应有成品");
    assert_eq!(
        got.len(),
        PAYLOAD,
        "{LABEL}: 成品必须是整份 {PAYLOAD} 字节（不是 head 截断后的残片）"
    );
    assert_eq!(
        sha1_hex(&got),
        expect_sha1,
        "{LABEL}: 成品内容与整份载荷的 sha1 必须一致"
    );
    assert!(
        !dir.join("out.bin.part").exists(),
        "{LABEL}: 降级收尾后不应残留 .part"
    );
}

/// 故障 6：206 的 `Content-Range` 区间正确，但**总长比 `job.size` 多 1**（issue #7A）。
///
/// 判据：不能等到最后全文件 sha1 失败才让人猜「是算错了还是源站换了文件」——
/// 分片校验处就要直接报「远端文件大小变了」，并同时给出服务器声明的总长与期望总长。
/// 成品不落地。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn content_range_total_mismatch_is_rejected() {
    const LABEL: &str = "wrong-total";
    let dir = TempDir::new("wrong-total");
    let payload = Arc::new(pattern(PAYLOAD, 8));
    let expect_sha1 = sha1_hex(&payload);
    let server = FaultServer::start(Fault::WrongTotal, payload.clone());
    let job = job_with_sha1(&server.url("blob.bin"), &payload);

    let err = expect_error(dir.path(), &job, LABEL).await;
    let msg = format!("{err:#}");
    println!("[{LABEL}] {msg}");

    assert!(
        msg.contains("远端文件大小变了"),
        "{LABEL}: 必须直接报「远端文件大小变了」，而不是等到最后 sha1 失败，实际: {msg}"
    );
    assert!(
        msg.contains(&format!("总长 {}，期望 {}", PAYLOAD + 1, PAYLOAD)),
        "{LABEL}: 错误里要同时出现服务器给的总长与期望总长，实际: {msg}"
    );
    assert!(
        msg.contains("Content-Range 校验失败"),
        "{LABEL}: 应当从分片校验出口冒出来，实际: {msg}"
    );
    assert_no_complete_artifact(dir.path(), "out.bin", &expect_sha1, LABEL);
}

// ─────────────────── 契约锚点：size 只是提示，不是校验 ───────────────────

/// **契约锚点**：`Job.size` 只是分片提示，**不是**完整性判据。
///
/// 契约写在两处：
/// * `fetch --size` 的 CLI 文档：「只影响『分片还是单流』，不影响能否下载」；
/// * `src/commands/fetch.rs:12-13`：「长度只用来决定分片还是单流，**不是下载的前提**」。
///
/// 所以单流路径（`src/download.rs:307-345`）不比对 `job.size` 是**刻意行为**，不是遗漏。
/// 服务器回 `200 + Content-Length: 1024` 并只发 1024 字节时，**从它自己的声明看这就是
/// 完整响应**——被违背的是「调用方声明的 1 MiB」，而那句声明本来就是提示。
///
/// 完整性由 `sha1` 负责（`Job.sha1`；原版 / 加载器 / 整合包的作业都带）。`fetch` 恒
/// `sha1: None`，此时唯一可信的长度凭据是服务器声明的 `Content-Length`：分片路径已比对
/// 片长（见 `lying_small_length_is_not_accepted`），单流路径由 reqwest 按
/// `Content-Length` 读满（提前 EOF 见 `truncate_mid_is_detected_while_reading_body`）。
///
/// **这条测试的价值是把口径钉住**：若将来有人把 `size` 当强校验（看到 1024 != 1 MiB
/// 就报错），它会红。**改口径之前先改 `docs/usage.md` 与 `commands/fetch.rs` 的注释**，
/// 别只改代码。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn declared_size_is_a_hint_not_a_check() {
    const DECLARED: u64 = 1 << 20;
    let dir = TempDir::new("size-is-a-hint");
    let payload = Arc::new(pattern(DECLARED as usize, 7));
    let server = FaultServer::start(Fault::LyingSmall, payload.clone());
    let job = Job {
        url: server.url("small.bin"),
        path: "out.bin".to_string(),
        sha1: None,
        size: Some(DECLARED),
        mirrors: Vec::new(),
    };

    run(std::slice::from_ref(&job), dir.path(), &test_opts())
        .await
        .expect("size 只是提示：声明 1 MiB、响应只有 1024 字节时应当成功，而不是报错");

    let got = fs::read(dir.join("out.bin")).expect("run 返回 Ok 就应有成品");
    assert_eq!(
        got.len(),
        SMALL_BODY,
        "成品必须恰好是服务器声明并发出的 {SMALL_BODY} 字节（size 不裁剪也不补足）"
    );
    assert_ne!(
        got.len(),
        DECLARED as usize,
        "实际落盘 {SMALL_BODY} 字节、声明 {DECLARED} 字节 —— 这正是「size 不是强校验」的锚点"
    );
    assert_eq!(
        sha1_hex(&got),
        sha1_hex(&payload[..SMALL_BODY]),
        "成品内容必须与服务器发出的前 {SMALL_BODY} 字节逐字节一致（sha1）"
    );
}
