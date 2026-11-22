//! 用 toxiproxy 注入真实网络故障（延迟 / 断开），跑我们的下载引擎。
//!
//! **toxiproxy 做不到的一件事**：它的 `bandwidth` 最低 1 KB/s，而 StallGuard 要抓的是
//! "每秒只滴 1–2 字节"那种。那一条仍靠本地 Python 慢滴服务器（见
//! `docs/reports/attack-report-20261122.md` §5）—— 两者分工，不互相代替。
//!
//! 为什么要有这一层：`tests/http_semantics.rs` 的假服务器只能模拟"服务端回了什么"，
//! 模拟不了"链路中途变坏"。而 `StallGuard`、退避重试、降级这几条逻辑**只有在真链路上才被触发**。
//!
//! 运行方式：
//!
//! ```text
//! toxiproxy-server          # 或 CI 里的 services: toxiproxy（监听 127.0.0.1:8474）
//! TOXIPROXY_ADDR=127.0.0.1:8474 cargo test --test toxiproxy_faults -- --nocapture
//! ```
//!
//! **没有 toxiproxy 就跳过**（打印一行 `skip`），不让本地开发被环境卡住 ——
//! 但 CI 里有 `TOXIPROXY_REQUIRED=1`，跳过就算失败，免得"以为测了其实没测"。

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use mc_downloader::download::{self, DownloadOpts};
use mc_downloader::version::{Job, SizeHint};

const TOXIC_ENV: &str = "TOXIPROXY_ADDR";
const REQUIRED_ENV: &str = "TOXIPROXY_REQUIRED";

fn api() -> Option<String> {
    let addr = std::env::var(TOXIC_ENV).ok()?;
    if addr.trim().is_empty() {
        return None;
    }
    Some(addr)
}

fn required() -> bool {
    std::env::var(REQUIRED_ENV).is_ok_and(|v| v == "1")
}

/// 取不到 toxiproxy：本地跳过，CI 里失败。
fn skip(reason: &str) -> bool {
    if required() {
        panic!("{REASON_PREFIX}{reason}");
    }
    eprintln!("[skip] {reason}（设 {TOXIC_ENV} 与 {REQUIRED_ENV}=1 可强制）");
    true
}

const REASON_PREFIX: &str = "toxiproxy 必需但不可用：";

fn http_post(addr: &str, path: &str, body: &str) -> std::io::Result<String> {
    let mut s = TcpStream::connect(addr)?;
    s.set_read_timeout(Some(Duration::from_secs(10)))?;
    let req = format!(
        "POST {path} HTTP/1.1\r\nHost: {addr}\r\nContent-Type: application/json\r\n\
         Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    s.write_all(req.as_bytes())?;
    let mut out = String::new();
    s.read_to_string(&mut out)?;
    Ok(out)
}

fn http_delete(addr: &str, path: &str) -> std::io::Result<()> {
    let mut s = TcpStream::connect(addr)?;
    s.set_read_timeout(Some(Duration::from_secs(10)))?;
    let req = format!("DELETE {path} HTTP/1.1\r\nHost: {addr}\r\nConnection: close\r\n\r\n");
    s.write_all(req.as_bytes())?;
    let mut out = String::new();
    let _ = s.read_to_string(&mut out);
    Ok(())
}

/// 极简 HTTP 源站：只服务一个固定 body，支持 Range（分片路径要用）。
fn start_origin(body: Vec<u8>) -> (u16, std::thread::JoinHandle<()>) {
    let l = TcpListener::bind("127.0.0.1:0").expect("bind origin");
    let port = l.local_addr().unwrap().port();
    let body = Arc::new(body);
    let handle = std::thread::spawn(move || {
        for stream in l.incoming() {
            let Ok(mut s) = stream else { continue };
            let body = body.clone();
            std::thread::spawn(move || {
                let mut buf = [0u8; 4096];
                let Ok(n) = s.read(&mut buf) else { return };
                let head = String::from_utf8_lossy(&buf[..n]).to_string();
                let start = head
                    .lines()
                    .find(|l| l.to_ascii_lowercase().starts_with("range:"))
                    .and_then(|l| l.split('=').nth(1))
                    .and_then(|r| r.split('-').next())
                    .and_then(|n| n.trim().parse::<usize>().ok())
                    .unwrap_or(0);
                let (status, chunk) = if start == 0 && !head.contains("Range") {
                    ("200 OK", &body[..])
                } else {
                    ("206 Partial Content", &body[start.min(body.len())..])
                };
                let mut resp = format!("HTTP/1.1 {status}\r\nContent-Length: {}\r\n", chunk.len());
                if status.starts_with("206") {
                    resp.push_str(&format!(
                        "Content-Range: bytes {}-{}/{}\r\n",
                        start,
                        body.len() - 1,
                        body.len()
                    ));
                }
                resp.push_str("Connection: close\r\n\r\n");
                let _ = s.write_all(resp.as_bytes());
                let _ = s.write_all(chunk);
                let _ = s.flush();
            });
        }
    });
    (port, handle)
}

/// 建一个代理：listen 端口 → 上游 origin 端口。返回 (proxy 名, listen 端口)。
fn make_proxy(api_addr: &str, listen_port: u16, upstream_port: u16, name: &str) -> bool {
    let body = format!(
        r#"{{"name":"{name}","listen":"127.0.0.1:{listen_port}","upstream":"127.0.0.1:{upstream_port}","enabled":true}}"#
    );
    match http_post(api_addr, "/proxies", &body) {
        Ok(resp) if resp.starts_with("HTTP/1.1 2") => true,
        Ok(resp) => {
            eprintln!("[warn] 建代理失败：{}", resp.lines().next().unwrap_or(""));
            false
        }
        Err(e) => {
            eprintln!("[warn] 连不上 toxiproxy API: {e}");
            false
        }
    }
}

fn add_toxic(api_addr: &str, proxy: &str, toxic: &str, body: &str) -> bool {
    let path = format!("/proxies/{proxy}/toxics");
    match http_post(api_addr, &path, body) {
        Ok(resp) if resp.starts_with("HTTP/1.1 2") => {
            eprintln!("[info] 注入 {toxic}");
            true
        }
        Ok(resp) => {
            eprintln!(
                "[warn] 注入 {toxic} 失败：{}",
                resp.lines().next().unwrap_or("")
            );
            false
        }
        Err(e) => {
            eprintln!("[warn] 注入 {toxic} 时连接失败: {e}");
            false
        }
    }
}

fn free_port() -> u16 {
    TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

fn job(url: String, size: u64, dst: &std::path::Path) -> Job {
    Job {
        url,
        path: dst.file_name().unwrap().to_string_lossy().to_string(),
        sha1: None,
        size: SizeHint::Exact(size),
        mirrors: vec![],
    }
}

fn tmpdir(tag: &str) -> PathBuf {
    let p = std::env::temp_dir().join(format!("mcd-toxi-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&p);
    std::fs::create_dir_all(&p).unwrap();
    p
}

#[test]
fn latency_does_not_break_a_download() {
    let Some(api_addr) = api() else {
        let _ = skip("没有 TOXIPROXY_ADDR");
        return;
    };
    if !required() && TcpStream::connect(&api_addr).is_err() {
        let _ = skip("toxiproxy API 连不上");
        return;
    }
    let payload = vec![7u8; 1024 * 1024]; // 1 MiB
    let (up, _h) = start_origin(payload.clone());
    let listen = free_port();
    let name = "lat-ok";
    let _ = http_delete(&api_addr, &format!("/proxies/{name}"));
    if !make_proxy(&api_addr, listen, up, name) {
        let _ = skip("建代理失败");
        return;
    }
    // 每跳 300ms 延迟：正常下载应当**照常成功**（这钉的是"别把超时设太短"）
    assert!(
        add_toxic(
            &api_addr,
            name,
            "latency",
            r#"{"name":"lat","type":"latency","attributes":{"latency":300,"jitter":0}}"#
        ),
        "注入 latency 失败"
    );

    let dir = tmpdir("lat");
    let dst = dir.join("f.bin");
    let mut opts = DownloadOpts::default();
    opts.timeout_secs = 10;
    opts.piece_conns = 1; // 单片，避免并发把延迟叠加成额外的不确定性
    let jobs = vec![job(
        format!("http://127.0.0.1:{listen}/f.bin"),
        payload.len() as u64,
        &dst,
    )];

    let rt = tokio::runtime::Runtime::new().unwrap();
    let began = Instant::now();
    let r = rt.block_on(download::run(&jobs, &dir, &opts));
    let took = began.elapsed();
    let _ = http_delete(&api_addr, &format!("/proxies/{name}"));

    assert!(r.is_ok(), "300ms 延迟不该让下载失败: {r:?}");
    assert_eq!(std::fs::read(&dst).unwrap().len(), payload.len());
    assert!(
        took < Duration::from_secs(30),
        "1 MiB + 300ms 延迟耗时 {took:?} 异常"
    );
}

#[test]
fn dead_link_does_not_hang_and_leaves_no_full_file() {
    let Some(api_addr) = api() else {
        let _ = skip("没有 TOXIPROXY_ADDR");
        return;
    };
    if !required() && TcpStream::connect(&api_addr).is_err() {
        let _ = skip("toxiproxy API 连不上");
        return;
    }
    let payload = vec![9u8; 512 * 1024];
    let (up, _h) = start_origin(payload.clone());
    let listen = free_port();
    let name = "stall";
    let _ = http_delete(&api_addr, &format!("/proxies/{name}"));
    if !make_proxy(&api_addr, listen, up, name) {
        let _ = skip("建代理失败");
        return;
    }
    // `timeout` 毒素：连接**不断开也不给数据**。这才是 slowloris 的真正形状：
    // 单次读一直没有返回 → `read_timeout` 会先抗议；而"完全不动"也是 StallGuard 的判据。
    //
    // 注意：不能用 `bandwidth` 代替 —— 2 KB/s 属于"慢但在动"，
    // 4096 字节的窗口迟早会满，所以 **不应该** 判停滞（我第一版就是这么写错的，跑了 285 s 才发现）。
    assert!(
        add_toxic(
            &api_addr,
            name,
            "timeout",
            r#"{"name":"stop","type":"timeout","attributes":{"timeout":0}}"#
        ),
        "注入 timeout 失败"
    );

    let dir = tmpdir("stall");
    let dst = dir.join("s.bin");
    let mut opts = DownloadOpts::default();
    opts.timeout_secs = 2; // 窗口 = max(10s, 2×2) = 10s
    opts.retries = 1;
    opts.piece_conns = 1;
    let jobs = vec![job(
        format!("http://127.0.0.1:{listen}/s.bin"),
        payload.len() as u64,
        &dst,
    )];

    let rt = tokio::runtime::Runtime::new().unwrap();
    let began = Instant::now();
    let r = rt.block_on(download::run(&jobs, &dir, &opts));
    let took = began.elapsed();
    let _ = http_delete(&api_addr, &format!("/proxies/{name}"));

    // 要么被 StallGuard 抓到（消息带"停滞"），要么 read_timeout 先报警；两者都是"不挂死"。
    match r {
        Err(e) => {
            let msg = format!("{e:#}");
            assert!(
                msg.contains("停滞") || msg.contains("下载中断"),
                "失败原因应指向停滞/中断，实际: {msg}"
            );
        }
        Ok(()) => panic!("512 KiB @2KB/s 不该在 {took:?} 内完成"),
    }
    // 关键：**不许留下"看起来成功"的成品**
    if dst.exists() {
        assert_ne!(
            std::fs::read(&dst).unwrap().len(),
            payload.len(),
            "失败却留下了完整长度的文件"
        );
    }
}
