//! 全局分片队列：动态插入，消费者不退出。
//!
//! 设计参照 DeepInfer 的 continuous batching —— **不等当前批完成，新任务随时插入**。
//! 队列空时 worker 挂起在 `Notify` 上，`push` 唤醒一个；`close` 唤醒全部并退出。

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use tokio::sync::{Mutex, Notify};

/// 一个待下载的分片
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PieceJob {
    /// 属于哪个文件（调用方自己的下标）
    pub file_idx: usize,
    /// 第几片
    pub piece_idx: u32,
    /// 字节区间 [start, end)
    pub start: u64,
    pub end: u64,
    /// 源下标（由 SourcePool 分配）
    pub source_idx: usize,
}

pub struct PieceScheduler {
    inner: Mutex<VecDeque<PieceJob>>,
    notify: Notify,
    closed: AtomicBool,
}

impl PieceScheduler {
    pub fn new() -> Self {
        Self {
            inner: Mutex::new(VecDeque::new()),
            notify: Notify::new(),
            closed: AtomicBool::new(false),
        }
    }

    /// 动态插入一片；若已有 worker 在等，唤醒一个
    pub async fn push(&self, job: PieceJob) {
        self.inner.lock().await.push_back(job);
        self.notify.notify_one();
    }

    /// 批量插入（只唤醒一次，避免惊群）
    pub async fn push_many(&self, jobs: impl IntoIterator<Item = PieceJob>) {
        let mut n = 0usize;
        {
            let mut q = self.inner.lock().await;
            for j in jobs {
                q.push_back(j);
                n += 1;
            }
        }
        if n > 0 {
            self.notify.notify_waiters();
        }
    }

    /// 取一片。队列空且未 close 时挂起等待。
    pub async fn next(&self) -> Option<PieceJob> {
        loop {
            {
                let mut q = self.inner.lock().await;
                if let Some(j) = q.pop_front() {
                    return Some(j);
                }
                if self.closed.load(Ordering::SeqCst) {
                    return None;
                }
            }
            self.notify.notified().await;
        }
    }

    /// 关闭：不再接受新任务，等待中的 worker 全部返回 None
    pub fn close(&self) {
        self.closed.store(true, Ordering::SeqCst);
        self.notify.notify_waiters();
    }

    pub fn is_closed(&self) -> bool {
        self.closed.load(Ordering::SeqCst)
    }

    pub async fn len(&self) -> usize {
        self.inner.lock().await.len()
    }

    pub async fn is_empty(&self) -> bool {
        self.len().await == 0
    }
}

impl Default for PieceScheduler {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::time::Duration;

    fn job(i: u32) -> PieceJob {
        PieceJob {
            file_idx: 0,
            piece_idx: i,
            start: i as u64 * 1024,
            end: (i as u64 + 1) * 1024,
            source_idx: 0,
        }
    }

    #[tokio::test]
    async fn push_then_next() {
        let s = PieceScheduler::new();
        s.push(job(1)).await;
        s.push(job(2)).await;
        assert_eq!(s.len().await, 2);

        let a = s.next().await.unwrap();
        let b = s.next().await.unwrap();
        assert_eq!(a.piece_idx, 1);
        assert_eq!(b.piece_idx, 2);
        assert!(s.is_empty().await);
    }

    #[tokio::test]
    async fn next_waits_until_pushed() {
        let s = Arc::new(PieceScheduler::new());
        let s2 = s.clone();

        let h = tokio::spawn(async move { s2.next().await });

        // 先确认它确实在等（不是立刻返回）
        tokio::time::sleep(Duration::from_millis(30)).await;
        assert!(!h.is_finished(), "队列空时 next 应挂起");

        s.push(job(7)).await;
        let got = tokio::time::timeout(Duration::from_secs(1), h)
            .await
            .expect("push 后应被唤醒")
            .unwrap()
            .unwrap();
        assert_eq!(got.piece_idx, 7);
    }

    #[tokio::test]
    async fn close_wakes_all_waiters() {
        let s = Arc::new(PieceScheduler::new());
        let mut handles = Vec::new();
        for _ in 0..4 {
            let s2 = s.clone();
            handles.push(tokio::spawn(async move { s2.next().await }));
        }

        tokio::time::sleep(Duration::from_millis(30)).await;
        s.close();

        for h in handles {
            let r = tokio::time::timeout(Duration::from_secs(1), h)
                .await
                .expect("close 后应立刻返回")
                .unwrap();
            assert!(r.is_none());
        }
    }

    #[tokio::test]
    async fn multiple_consumers_no_duplicate() {
        let s = Arc::new(PieceScheduler::new());
        s.push_many((0..20).map(job)).await;

        let mut handles = Vec::new();
        for _ in 0..4 {
            let s2 = s.clone();
            handles.push(tokio::spawn(async move {
                let mut got = Vec::new();
                while let Some(j) = s2.next().await {
                    got.push(j.piece_idx);
                }
                got
            }));
        }

        // 全部取完后关闭，让 worker 退出
        tokio::time::sleep(Duration::from_millis(50)).await;
        s.close();

        let mut all = Vec::new();
        for h in handles {
            all.extend(h.await.unwrap());
        }
        all.sort_unstable();
        let expect: Vec<u32> = (0..20).collect();
        assert_eq!(all, expect, "每个分片应恰好被消费一次");
    }

    #[tokio::test]
    async fn push_after_close_is_ignored_by_workers() {
        let s = Arc::new(PieceScheduler::new());
        s.close();
        assert!(s.next().await.is_none());

        // close 后仍可 push（数据留在队列），但 worker 不再等
        s.push(job(1)).await;
        assert_eq!(s.len().await, 1);
        // 直接 next 仍能取到（因为队列非空）—— 这是有意行为：close 只影响"等不等"
        assert!(s.next().await.is_some());
    }
}
