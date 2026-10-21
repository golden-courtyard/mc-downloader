//! 多源分配（EPLB 思路的简化版）。
//!
//! 每个源维护一个 ewma 吞吐估计；分配时优先给吞吐最高的源。
//! 连续失败的源会被降权（吞吐按比例衰减），恢复后会自动回升。

use std::sync::Mutex;

/// ewma 平滑系数（越大越灵敏）
const ALPHA: f64 = 0.3;
/// 初始吞吐估计（MB/s），给所有源一个相同的起点
const INITIAL_BPS: f64 = 8.0 * 1024.0 * 1024.0;
/// 失败一次，吞吐估计乘这个系数
const FAIL_PENALTY: f64 = 0.5;
/// 吞吐下限，避免被压到 0 后永不复用
const MIN_BPS: f64 = 64.0 * 1024.0;

#[derive(Debug, Clone)]
struct SourceState {
    url: String,
    ewma_bps: f64,
    fail_streak: u32,
    used: u64,
}

#[derive(Debug)]
pub struct SourcePool {
    sources: Mutex<Vec<SourceState>>,
}

impl SourcePool {
    pub fn new(mirrors: &[String]) -> Self {
        let list = mirrors
            .iter()
            .map(|u| SourceState {
                url: u.clone(),
                ewma_bps: INITIAL_BPS,
                fail_streak: 0,
                used: 0,
            })
            .collect();
        Self {
            sources: Mutex::new(list),
        }
    }

    pub fn len(&self) -> usize {
        self.sources.lock().map(|s| s.len()).unwrap_or(0)
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// 当前吞吐估计最高的源下标
    pub fn pick(&self) -> usize {
        let g = match self.sources.lock() {
            Ok(g) => g,
            Err(p) => p.into_inner(),
        };
        if g.is_empty() {
            return 0;
        }
        let mut best = 0usize;
        let mut best_bps = -1.0f64;
        for (i, s) in g.iter().enumerate() {
            if s.ewma_bps > best_bps {
                best_bps = s.ewma_bps;
                best = i;
            }
        }
        best
    }

    pub fn url_of(&self, idx: usize) -> Option<String> {
        let g = self.sources.lock().ok()?;
        g.get(idx).map(|s| s.url.clone())
    }

    /// 记录一次成功：更新该源的 ewma 吞吐
    pub fn record_success(&self, idx: usize, bytes: u64, secs: f64) {
        let mut g = match self.sources.lock() {
            Ok(g) => g,
            Err(p) => p.into_inner(),
        };
        if let Some(s) = g.get_mut(idx) {
            s.used += 1;
            s.fail_streak = 0;
            if secs > 0.0 {
                let sample = bytes as f64 / secs;
                s.ewma_bps = if s.ewma_bps <= 0.0 {
                    sample
                } else {
                    ALPHA * sample + (1.0 - ALPHA) * s.ewma_bps
                };
                if s.ewma_bps < MIN_BPS {
                    s.ewma_bps = MIN_BPS;
                }
            }
        }
    }

    /// 记录一次失败：降权
    pub fn record_failure(&self, idx: usize) {
        let mut g = match self.sources.lock() {
            Ok(g) => g,
            Err(p) => p.into_inner(),
        };
        if let Some(s) = g.get_mut(idx) {
            s.fail_streak += 1;
            s.ewma_bps = (s.ewma_bps * FAIL_PENALTY).max(MIN_BPS);
        }
    }

    /// 调试用：当前的源与吞吐估计
    pub fn snapshot(&self) -> Vec<(String, f64, u64)> {
        let g = match self.sources.lock() {
            Ok(g) => g,
            Err(p) => p.into_inner(),
        };
        g.iter()
            .map(|s| (s.url.clone(), s.ewma_bps / 1048576.0, s.used))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pool() -> SourcePool {
        SourcePool::new(&[
            "https://a.example/f".to_string(),
            "https://b.example/f".to_string(),
        ])
    }

    #[test]
    fn initial_pick_is_first() {
        let p = pool();
        assert_eq!(p.len(), 2);
        assert_eq!(p.pick(), 0);
    }

    #[test]
    fn faster_source_gets_picked() {
        let p = pool();
        // b 报 100 MB/s，a 报 1 MB/s
        p.record_success(1, 100 * 1048576, 1.0);
        p.record_success(0, 1048576, 1.0);
        // 反复采样，让 ewma 收敛
        for _ in 0..5 {
            p.record_success(1, 100 * 1048576, 1.0);
            p.record_success(0, 1048576, 1.0);
        }
        assert_eq!(p.pick(), 1, "应该挑更快的 b");
    }

    #[test]
    fn failure_penalizes() {
        let p = pool();
        p.record_success(0, 50 * 1048576, 1.0);
        for _ in 0..3 {
            p.record_failure(0);
        }
        // a 被压下去后，b（初始值）应该胜出
        assert_eq!(p.pick(), 1);
    }

    #[test]
    fn penalty_has_floor() {
        let p = pool();
        for _ in 0..50 {
            p.record_failure(0);
        }
        let snap = p.snapshot();
        assert!(snap[0].1 > 0.0, "吞吐下限不该归零");
    }

    #[test]
    fn success_restores() {
        let p = pool();
        for _ in 0..5 {
            p.record_failure(0);
        }
        let low = p.snapshot()[0].1;
        for _ in 0..5 {
            p.record_success(0, 100 * 1048576, 1.0);
        }
        let high = p.snapshot()[0].1;
        assert!(high > low, "成功后应恢复: {low} -> {high}");
    }

    #[test]
    fn empty_pool_is_safe() {
        let p = SourcePool::new(&[]);
        assert!(p.is_empty());
        assert_eq!(p.pick(), 0);
        assert!(p.url_of(0).is_none());
    }
}
