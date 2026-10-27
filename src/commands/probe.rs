//! `probe` —— 串行测多个源的吞吐，输出排名（填 `mirrors` 前先跑它）。

use anyhow::Result;
use futures::StreamExt;

use crate::commands::Ctx;
use mc_downloader::download;

pub async fn run(urls: Vec<String>, seconds: u64, ctx: &Ctx) -> Result<()> {
    if urls.len() < 2 {
        anyhow::bail!(
            "probe 至少给两个 URL 才有意义（现在 {} 个）\n  \
             用法：mc-downloader probe <url1> <url2> [--seconds 5]",
            urls.len()
        );
    }
    let secs = seconds.clamp(2, 60);
    println!("[*] 探测 {secs} 秒/个（串行，避免互相抢带宽）");
    let client = download::client(&ctx.opts)?;
    let mut results: Vec<(String, Option<u64>)> = Vec::new();
    for (i, u) in urls.iter().enumerate() {
        print!("  [{}/{}] {} ... ", i + 1, urls.len(), truncate(u, 58));
        match probe_one(&client, u, secs).await {
            Ok(n) => {
                println!("{}", rate_str(n, secs));
                results.push((u.clone(), Some(n)));
            }
            Err(e) => {
                println!("失败: {e}");
                results.push((u.clone(), None));
            }
        }
    }
    // 按吞吐降序，失败的排最后
    results.sort_by_key(|x| std::cmp::Reverse(x.1.unwrap_or(0)));
    println!("\n[*] 排名:");
    for (i, (u, n)) in results.iter().enumerate() {
        let r = match n {
            Some(v) => rate_str(*v, secs),
            None => "失败".to_string(),
        };
        println!("  {}. {:>12}  {u}", i + 1, r);
    }
    println!("\n[i] 把快的填进 `mirrors = [\"...\"]`；下载时 SourcePool 会按实测吞吐分配分片");
    Ok(())
}

/// **不要求服务器支持 Range** —— 直接 GET 读一段时间就断，最简单也最通用。
/// 多给 5 秒余量给建连（慢源建连可能就花掉 2-3 秒）。
async fn probe_one(client: &reqwest::Client, url: &str, secs: u64) -> Result<u64> {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(secs);
    let fut = async {
        let resp = client.get(url).send().await?.error_for_status()?;
        let mut stream = resp.bytes_stream();
        let mut total = 0u64;
        while let Some(c) = stream.next().await {
            total += c?.len() as u64;
            if std::time::Instant::now() >= deadline {
                break;
            }
        }
        Ok::<u64, anyhow::Error>(total)
    };
    match tokio::time::timeout(std::time::Duration::from_secs(secs + 5), fut).await {
        Ok(r) => r,
        Err(_) => anyhow::bail!("探测超时（{secs}s + 5s 余量）"),
    }
}

fn rate_str(bytes: u64, secs: u64) -> String {
    format!("{}/s", download::human_rate(bytes / secs.max(1)))
}

fn truncate(s: &str, n: usize) -> String {
    if s.chars().count() <= n {
        s.to_string()
    } else {
        format!("{}…", s.chars().take(n).collect::<String>())
    }
}
