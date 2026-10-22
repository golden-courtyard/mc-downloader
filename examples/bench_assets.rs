//! 临时性能探针：定位"每个 asset 13.7 秒"慢在哪一段。
//!
//! 用法：`cargo run --release --example bench_assets -- <代理> [并发数] [文件数]`
//! 例：`cargo run --release --example bench_assets -- http://127.0.0.1:7897 16 48`
//!
//! 测四段：建连(→首字节前的响应头) / 首块 / 余下传输 / 写盘。
//! 跑完可删。

use std::sync::Arc;
use std::time::Instant;

use futures::StreamExt;
use tokio::io::AsyncWriteExt;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().collect();
    let proxy = args.get(1).cloned();
    let conc: usize = args.get(2).and_then(|s| s.parse().ok()).unwrap_or(16);
    let limit: usize = args.get(3).and_then(|s| s.parse().ok()).unwrap_or(48);
    // 1 = 攒内存一次性写；2 = 每 chunk 一次 tokio::fs::write_all（模拟 one_stream）
    let wmode: u32 = args.get(4).and_then(|s| s.parse().ok()).unwrap_or(1);

    // 从 assets_uris.txt 抽 URI（隔行：url / dir= / out=）
    let list = std::env::temp_dir().join("assets_uris.txt");
    let text = std::fs::read_to_string(&list)
        .map_err(|e| anyhow::anyhow!("读 {} 失败: {e}（先跑生成脚本）", list.display()))?;
    let urls: Vec<String> = text
        .lines()
        .filter(|l| l.starts_with("https://"))
        .take(limit)
        .map(|s| s.to_string())
        .collect();
    println!("[*] 代理={proxy:?} 并发={conc} 文件={} 写模式={wmode}", urls.len());

    // mode=3：走 download::run（内部 buffer_unordered），用来对照手写 spawn 循环
    if wmode == 3 {
        let jobs: Vec<mc_downloader::version::Job> = urls
            .iter()
            .enumerate()
            .map(|(i, u)| mc_downloader::version::Job {
                url: u.clone(),
                path: format!("f{i}"),
                sha1: None,
                size: None,
                mirrors: Vec::new(),
            })
            .collect();
        let root = std::env::temp_dir().join("bench_run_out");
        let _ = std::fs::remove_dir_all(&root);
        let o2 = mc_downloader::download::DownloadOpts {
            proxy: proxy.clone(),
            concurrency: conc,
            ..Default::default()
        };
        let t0 = Instant::now();
        mc_downloader::download::run(&jobs, &root, &o2).await?;
        println!("\n=== download::run 总耗时 {} ms（{} 个文件）===", t0.elapsed().as_millis(), jobs.len());
        let _ = std::fs::remove_dir_all(&root);
        return Ok(());
    }

    let opts = mc_downloader::download::DownloadOpts {
        proxy: proxy.clone(),
        concurrency: conc,
        ..Default::default()
    };
    let client = Arc::new(mc_downloader::download::client(&opts)?);

    let sem = Arc::new(tokio::sync::Semaphore::new(conc));
    let out_dir = std::env::temp_dir().join("bench_assets_out");
    tokio::fs::create_dir_all(&out_dir).await?;

    let t_all = Instant::now();
    let mut tasks = Vec::new();
    for (i, url) in urls.into_iter().enumerate() {
        let client = client.clone();
        let sem = sem.clone();
        let dir = out_dir.clone();
        tasks.push(tokio::spawn(async move {
            let _p = sem.acquire().await.unwrap();

            // ① 建连 + 拿到响应头
            let t0 = Instant::now();
            let resp = match client.get(&url).send().await {
                Ok(r) => r,
                Err(e) => return (i, 0u64, 0u64, 0u64, 0u64, format!("{e}")),
            };
            let t_conn = t0.elapsed().as_millis() as u64;

            // ② 首个 chunk
            let mut stream = resp.bytes_stream();
            let t1 = Instant::now();
            let mut total = 0u64;
            let mut chunks = 0u32;
            let mut buf: Vec<u8> = Vec::with_capacity(256 * 1024);
            let per_chunk_path = dir.join(format!("f{i}.chunked"));
            let mut per_chunk_file = if wmode == 2 {
                tokio::fs::File::create(&per_chunk_path).await.ok()
            } else {
                None
            };
            let mut t_first = 0u64;
            while let Some(c) = stream.next().await {
                let Ok(c) = c else { break };
                if chunks == 0 {
                    t_first = t1.elapsed().as_millis() as u64;
                }
                chunks += 1;
                total += c.len() as u64;
                if let Some(f) = per_chunk_file.as_mut() {
                    let _ = f.write_all(&c).await;   // ← 模拟 one_stream：每 chunk 一次写
                } else {
                    buf.extend_from_slice(&c);
                }
            }
            let t_rest = t1.elapsed().as_millis() as u64 - t_first;

            // ④ 写盘（wmode=1 时一次性写；wmode=2 已在循环里写过了）
            let t3 = Instant::now();
            if wmode == 1 {
                let _ = tokio::fs::write(dir.join(format!("f{i}")), &buf).await;
            }
            let t_write = t3.elapsed().as_millis() as u64;

            (i, t_conn, t_first, t_rest, t_write, format!("{}KB/{}块", total / 1024, chunks))
        }));
    }

    let mut rows = Vec::new();
    for t in tasks {
        rows.push(t.await?);
    }
    let total_ms = t_all.elapsed().as_millis() as u64;

    let ok: Vec<_> = rows.iter().filter(|r| r.5.contains("块")).collect();
    let n = ok.len().max(1) as u64;
    let avg = |f: fn(&(usize, u64, u64, u64, u64, String)) -> u64| {
        ok.iter().map(|r| f(r)).sum::<u64>() / n
    };
    println!("\n=== {} 个文件，总耗时 {} ms ===", ok.len(), total_ms);
    println!("  建连  平均 {:>5} ms", avg(|r| r.1));
    println!("  首块  平均 {:>5} ms", avg(|r| r.2));
    println!("  余下  平均 {:>5} ms", avg(|r| r.3));
    println!("  写盘  平均 {:>5} ms", avg(|r| r.4));
    println!("\n--- 明细（前 8 条）---");
    for r in rows.iter().take(8) {
        println!(
            "  #{:<3} 建连 {:>5} 首块 {:>5} 余下 {:>6} 写盘 {:>4}  {}",
            r.0, r.1, r.2, r.3, r.4, r.5
        );
    }
    let _ = tokio::fs::remove_dir_all(&out_dir).await;
    Ok(())
}
