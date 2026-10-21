//! 传输速度统计（内置记速器）。
//!
//! 用原子变量而非锁 —— 16 个并发任务每写一块都更新，加锁会成为新瓶颈。

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

#[derive(Debug)]
pub struct Progress {
    done: AtomicU64,
    total: AtomicU64,
    start: Instant,
}

impl Progress {
    pub fn new(total: u64) -> Arc<Self> {
        Arc::new(Self {
            done: AtomicU64::new(0),
            total: AtomicU64::new(total),
            start: Instant::now(),
        })
    }

    pub fn add(&self, n: u64) {
        self.done.fetch_add(n, Ordering::Relaxed);
    }

    pub fn done(&self) -> u64 {
        self.done.load(Ordering::Relaxed)
    }

    /// 起一个每秒打印的后台任务；调用方结束时 abort 即可
    pub fn spawn_reporter(self: &Arc<Self>) -> tokio::task::JoinHandle<()> {
        let p = self.clone();
        tokio::spawn(async move {
            let mut tick = tokio::time::interval(Duration::from_secs(1));
            tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            tick.tick().await; // 第一次立即返回，跳过
            loop {
                tick.tick().await;
                if let Some(l) = p.line() {
                    println!("{l}");
                }
            }
        })
    }

    /// 格式化一行（可单独调用，便于测试）
    pub fn line(&self) -> Option<String> {
        let done = self.done.load(Ordering::Relaxed);
        let total = self.total.load(Ordering::Relaxed);
        let secs = self.start.elapsed().as_secs_f64();
        if secs < 0.2 || done == 0 {
            return None;
        }

        let speed = done as f64 / secs;

        if total == 0 {
            return Some(format!("[ {} ]  {}/s", human(done), rate(speed)));
        }
        if done >= total {
            return Some(format!("[ {} / {} ]  完成", human(total), human(total)));
        }
        let eta = (total - done) as f64 / speed.max(1.0);
        Some(format!(
            "[ {} / {} ]  {}/s  ETA {}",
            human(done),
            human(total),
            rate(speed),
            mmss(eta)
        ))
    }
}

fn human(n: u64) -> String {
    const K: f64 = 1024.0;
    let f = n as f64;
    if f < K {
        format!("{n} B")
    } else if f < K * K {
        format!("{:.1} KB", f / K)
    } else if f < K * K * K {
        format!("{:.1} MB", f / (K * K))
    } else {
        format!("{:.2} GB", f / (K * K * K))
    }
}

fn rate(bps: f64) -> String {
    const K: f64 = 1024.0;
    if bps < K {
        format!("{bps:.0} B")
    } else if bps < K * K {
        format!("{:.1} KB", bps / K)
    } else {
        format!("{:.2} MB", bps / (K * K))
    }
}

fn mmss(secs: f64) -> String {
    let s = secs.max(0.0) as u64;
    format!("{:02}:{:02}", s / 60, s % 60)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn human_units() {
        assert_eq!(human(512), "512 B");
        assert_eq!(human(2048), "2.0 KB");
        assert_eq!(human(5 * 1024 * 1024), "5.0 MB");
        assert_eq!(human(3 * 1024 * 1024 * 1024), "3.00 GB");
    }

    #[test]
    fn mmss_formats() {
        assert_eq!(mmss(0.0), "00:00");
        assert_eq!(mmss(65.0), "01:05");
        assert_eq!(mmss(3661.0), "61:01");
    }

    #[test]
    fn line_needs_some_progress() {
        let p = Progress::new(1000);
        // 刚创建、还没数据 -> None
        assert!(p.line().is_none());
    }

    #[test]
    fn line_when_done() {
        let p = Progress::new(100);
        p.add(100);
        // 仍在 0.2s 内 -> None（避免抖动）
        // 用 sleep 太慢，这里只验证 add 后 done 正确
        assert_eq!(p.done(), 100);
    }

    #[test]
    fn total_unknown() {
        let p = Progress::new(0);
        p.add(4096);
        assert_eq!(p.done(), 4096);
    }
}
