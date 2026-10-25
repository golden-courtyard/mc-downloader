//! 传输速度统计（内置记速器）。
//!
//! 用原子变量而非锁 —— 16 个并发任务每写一块都更新，加锁会成为新瓶颈。

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// 令牌桶限速。
///
/// **只在设了 `max_speed` 时才存在**（`Progress::throttle` 为 `Some`），
/// 未设时 `Progress::add_throttled` 只多一次 `Option` 分支就返回 —— 不给
/// 「没设限速」的路径引入每 chunk 的锁开销。
///
/// 令牌上限取 **1 秒的量**：否则长时间空闲后 `tokens` 攒满，恢复的第一秒会跑 2 倍速。
#[derive(Debug)]
pub struct Throttle {
    rate: u64, // 字节/秒
    state: Mutex<ThrottleState>,
}

#[derive(Debug)]
struct ThrottleState {
    tokens: f64,
    last: Instant,
}

impl Throttle {
    pub fn new(rate: u64) -> Self {
        Self {
            rate: rate.max(1),
            state: Mutex::new(ThrottleState {
                tokens: rate as f64,
                last: Instant::now(),
            }),
        }
    }

    /// 消费 `n` 个令牌；不够就等到够。
    ///
    /// 用 `std::sync::Mutex` 而不是 async mutex：临界区只有几微秒（算数 + 赋值），
    /// async mutex 的调度开销反而更大。**不要在持锁期间 await** —— 上面那个
    /// `wait` 是算完就放锁，sleep 在锁外。
    pub async fn acquire(&self, n: u64) {
        loop {
            let wait_secs = {
                let mut st = self.state.lock().expect("throttle 锁中毒");
                let now = Instant::now();
                let elapsed = now.duration_since(st.last).as_secs_f64();
                let cap = self.rate as f64; // 最多攒 1 秒的量
                st.tokens = (st.tokens + elapsed * self.rate as f64).min(cap);
                st.last = now;

                if st.tokens >= n as f64 {
                    st.tokens -= n as f64;
                    return;
                }
                (n as f64 - st.tokens) / self.rate as f64
            };
            // 锁已释放，再 sleep
            tokio::time::sleep(Duration::from_secs_f64(wait_secs)).await;
        }
    }
}

/// 解析 `5M` / `500K` / `1048576` 这类写法（纯数字 = 字节/秒）。
///
/// 单位用 1024 进制（和磁盘/网络的常见口径一致）。**非法输入返回 None**，
/// 由调用方决定报错 —— 这里不 panic。
pub fn parse_speed(s: &str) -> Option<u64> {
    let t = s.trim();
    if t.is_empty() {
        return None;
    }
    let (num, mult) = match t.chars().last()? {
        'k' | 'K' => (&t[..t.len() - 1], 1024u64),
        'm' | 'M' => (&t[..t.len() - 1], 1024 * 1024),
        'g' | 'G' => (&t[..t.len() - 1], 1024 * 1024 * 1024),
        _ => (t, 1),
    };
    let v: f64 = num.trim().parse().ok()?;
    if !v.is_finite() || v <= 0.0 {
        return None;
    }
    Some((v * mult as f64) as u64)
}

#[derive(Debug)]
pub struct Progress {
    done: AtomicU64,
    total: AtomicU64,
    start: Instant,
    /// `None` = 不限速。设了才走令牌桶。
    throttle: Option<Arc<Throttle>>,
}

impl Progress {
    pub fn new(total: u64) -> Arc<Self> {
        Arc::new(Self {
            done: AtomicU64::new(0),
            total: AtomicU64::new(total),
            start: Instant::now(),
            throttle: None,
        })
    }

    /// 带总带宽上限（字节/秒）。
    pub fn with_limit(total: u64, bytes_per_sec: u64) -> Arc<Self> {
        Arc::new(Self {
            done: AtomicU64::new(0),
            total: AtomicU64::new(total),
            start: Instant::now(),
            throttle: Some(Arc::new(Throttle::new(bytes_per_sec))),
        })
    }

    /// 限速版：先等令牌，再记账。**未设限速时只有一次 `Option` 分支**。
    pub async fn add_throttled(&self, n: u64) {
        if let Some(t) = &self.throttle {
            t.acquire(n).await;
        }
        self.add(n);
    }

    pub fn is_limited(&self) -> bool {
        self.throttle.is_some()
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
    fn parse_speed_units() {
        assert_eq!(parse_speed("5M"), Some(5 * 1024 * 1024));
        assert_eq!(parse_speed("500K"), Some(500 * 1024));
        assert_eq!(parse_speed("1048576"), Some(1048576));
        assert_eq!(parse_speed("1G"), Some(1024 * 1024 * 1024));
        assert_eq!(parse_speed(" 2m "), Some(2 * 1024 * 1024), "要容忍空白");
        assert_eq!(parse_speed("0"), None, "0 无意义");
        assert_eq!(parse_speed(""), None);
        assert_eq!(parse_speed("abc"), None);
        assert_eq!(parse_speed("5X"), None, "未知单位要拒绝，不能当字节");
        assert_eq!(parse_speed("-5M"), None);
    }

    #[tokio::test]
    async fn throttle_allows_initial_burst_then_blocks() {
        // 速率 1000 B/s，初始令牌 1000 ⇒ 先给 1000 不该等
        let t = Throttle::new(1000);
        let t0 = Instant::now();
        t.acquire(1000).await;
        assert!(
            t0.elapsed() < Duration::from_millis(50),
            "初始桶内不该等: {:?}",
            t0.elapsed()
        );

        // 再要 500 ⇒ 桶空了，要等约 0.5s
        let t1 = Instant::now();
        t.acquire(500).await;
        let waited = t1.elapsed();
        assert!(
            waited >= Duration::from_millis(400),
            "应该等约 0.5s，实际 {waited:?}"
        );
    }

    #[tokio::test]
    async fn unlimited_add_does_not_block() {
        let p = Progress::new(1000);
        assert!(!p.is_limited());
        let t0 = Instant::now();
        for _ in 0..1000 {
            p.add_throttled(1024).await;
        }
        assert!(t0.elapsed() < Duration::from_millis(50), "未限速不该有等待");
        assert_eq!(p.done(), 1000 * 1024);
    }

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
