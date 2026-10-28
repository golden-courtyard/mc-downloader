//! 多源分配（EPLB 思路的简化版）。
//!
//! 每个源维护一个 ewma 吞吐估计；分配时优先给吞吐最高的源。
//! 连续失败的源会被降权（吞吐按比例衰减），恢复后会自动回升。

use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

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
    /// 轮转游标：所有源吞吐全等时用它把分配摊开。
    ///
    /// 没有它 `pick()` 会恒返回 0 —— 所有源起点都是 `INITIAL_BPS`、比较又是严格 `>`，
    /// 谁都不超过下标 0，多源就等价单源（2026-10-28 修的就是这个）。
    cursor: AtomicUsize,
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
            cursor: AtomicUsize::new(0),
        }
    }

    pub fn len(&self) -> usize {
        self.sources.lock().map(|s| s.len()).unwrap_or(0)
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// 当前吞吐估计最高的源下标；**全等时按轮转依次返回**。
    ///
    /// 空池返回 0（历史契约，调用方靠 `url_of` 报"源不存在"）。
    pub fn pick(&self) -> usize {
        self.pick_excluding(&[]).unwrap_or(0)
    }

    /// 在**未被排除**的源里选：同一套规则（游标轮转 + 全扫描取最大）。
    ///
    /// 扫描从游标位置开始、扫完一整圈，取遇到的第一个最大值：
    /// - 某个源严格最大 ⇒ 无论从哪开始都会在那一圈被选中（全扫描保证）
    /// - 剩下的全部相等 ⇒ 谁都不严格大于起点，返回起点（未被排除的那个）⇒ 天然轮流
    /// - 全部被排除 / 空池 ⇒ `None`
    pub fn pick_excluding(&self, skip: &[usize]) -> Option<usize> {
        let g = match self.sources.lock() {
            Ok(g) => g,
            Err(p) => p.into_inner(),
        };
        let n = g.len();
        if n == 0 {
            return None;
        }
        let start = self.cursor.fetch_add(1, Ordering::Relaxed) % n;
        let mut best: Option<usize> = None;
        // 必须扫完一整圈，**不能"找到更大的就 break"** —— 提前退会破坏轮转
        for k in 0..n {
            let i = (start + k) % n;
            if skip.contains(&i) {
                continue;
            }
            match best {
                None => best = Some(i),
                Some(b) if g[i].ewma_bps > g[b].ewma_bps => best = Some(i),
                _ => {}
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

    fn pool3() -> SourcePool {
        SourcePool::new(&[
            "https://a.example/f".to_string(),
            "https://b.example/f".to_string(),
            "https://c.example/f".to_string(),
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

    #[test]
    fn pick_rotates_when_all_equal() {
        let p = pool3();
        // 全等时必须依次 0→1→2 循环。**断言完整序列**：只断言"落在 0..3 内"恒真，等于没测
        let got: Vec<usize> = (0..6).map(|_| p.pick()).collect();
        assert_eq!(got, vec![0, 1, 2, 0, 1, 2]);
    }

    #[test]
    fn pick_prefers_higher_ewma() {
        let p = pool();
        // 把 b 抬成唯一最大（100 MB/s 采样 5 次 ⇒ ~35 MB/s，其余仍是 8 MB/s）
        for _ in 0..5 {
            p.record_success(1, 100 * 1048576, 1.0);
        }
        // 严格最大时，无论游标停在哪、连续调多少次都该选中它
        for _ in 0..5 {
            assert_eq!(p.pick(), 1, "唯一最大源应被稳定选中，与游标位置无关");
        }
    }

    #[test]
    fn pick_handles_empty_pool() {
        let p = SourcePool::new(&[]);
        assert_eq!(p.pick(), 0, "空池应返回 0 而不是 panic");
        assert_eq!(p.pick(), 0, "第二次同样安全");
        // pick() 现在委托给它，空池的 None 分支也要真跑到
        assert_eq!(p.pick_excluding(&[]), None, "空池没有可选源，应返回 None");
    }

    #[test]
    fn pick_excluding_skips_listed() {
        let p = pool3();
        // 3 源全等、排除 0：结果只能在 {1,2}，且两个都得用上（否则等于换个恒真断言）
        let got: Vec<usize> = (0..6).map(|_| p.pick_excluding(&[0]).unwrap()).collect();
        assert!(!got.contains(&0), "被排除的 0 不该被选中: {got:?}");
        assert!(got.iter().all(|i| i == &1 || i == &2), "越界: {got:?}");
        assert!(
            got.contains(&1) && got.contains(&2),
            "两个可用源都该轮到: {got:?}"
        );
    }

    #[test]
    fn pick_excluding_returns_none_when_all_skipped() {
        let p = pool3();
        assert_eq!(p.pick_excluding(&[0, 1, 2]), None, "全部排除时无源可选");
    }

    #[test]
    fn pick_excluding_still_prefers_max() {
        let p = pool3();
        // 把 c（下标 2）抬成唯一最大，再排除 a（下标 0）
        for _ in 0..5 {
            p.record_success(2, 100 * 1048576, 1.0);
        }
        for _ in 0..5 {
            assert_eq!(
                p.pick_excluding(&[0]),
                Some(2),
                "唯一最大源即便被跳过列表绕开也要选它"
            );
        }
    }
}
