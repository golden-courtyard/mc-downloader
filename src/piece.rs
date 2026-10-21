//! 分片表：固定大小分片 + 位图状态。
//!
//! 设计参照 DeepInfer 的 block table（`csrc/engine/block_manager.hpp`）
//! 与 aria2 的 `min-split-size`：**小文件不分片**。

use std::collections::HashSet;

/// 小文件阈值：低于此值不分片（握手开销 > 收益）
const NO_SPLIT_BELOW: u64 = 8 * 1024 * 1024; // 8 MB
/// 中等文件分片大小
const PIECE_MID: u64 = 4 * 1024 * 1024; // 4 MB
/// 大文件分片大小
const PIECE_LARGE: u64 = 16 * 1024 * 1024; // 16 MB
/// 进入大文件分片的阈值
const LARGE_ABOVE: u64 = 256 * 1024 * 1024; // 256 MB

#[derive(Debug)]
pub struct PieceTable {
    file_size: u64,
    piece_size: u64,
    total: u32,
    /// 每 bit 一片：0 未完成 / 1 完成
    bitmap: Vec<u64>,
    inflight: HashSet<u32>,
}

impl PieceTable {
    pub fn new(file_size: u64) -> Self {
        let piece_size = if file_size < NO_SPLIT_BELOW {
            // 不分片：一片覆盖整个文件
            file_size.max(1)
        } else if file_size <= LARGE_ABOVE {
            PIECE_MID
        } else {
            PIECE_LARGE
        };

        let total = file_size.div_ceil(piece_size).max(1) as u32;
        let words = total.div_ceil(64) as usize;

        Self {
            file_size,
            piece_size,
            total,
            bitmap: vec![0u64; words],
            inflight: HashSet::new(),
        }
    }

    pub fn file_size(&self) -> u64 {
        self.file_size
    }

    pub fn piece_size(&self) -> u64 {
        self.piece_size
    }

    pub fn total(&self) -> u32 {
        self.total
    }

    /// 是否真的分了片（小文件为 false）
    pub fn is_split(&self) -> bool {
        self.total > 1
    }

    /// 第 idx 片的字节区间 `[start, end)`，闭开区间
    pub fn range(&self, idx: u32) -> Option<(u64, u64)> {
        if idx >= self.total {
            return None;
        }
        let start = idx as u64 * self.piece_size;
        let end = (start + self.piece_size).min(self.file_size);
        Some((start, end))
    }

    pub fn is_done(&self, idx: u32) -> bool {
        if idx >= self.total {
            return false;
        }
        let (w, b) = ((idx / 64) as usize, idx % 64);
        self.bitmap[w] & (1u64 << b) != 0
    }

    pub fn mark_inflight(&mut self, idx: u32) {
        if idx < self.total {
            self.inflight.insert(idx);
        }
    }

    pub fn mark_done(&mut self, idx: u32) {
        if idx >= self.total {
            return;
        }
        let (w, b) = ((idx / 64) as usize, idx % 64);
        self.bitmap[w] |= 1u64 << b;
        self.inflight.remove(&idx);
    }

    /// 失败时退回 Pending（从 inflight 移除，位图不变）
    pub fn release(&mut self, idx: u32) {
        self.inflight.remove(&idx);
    }

    pub fn all_done(&self) -> bool {
        let total = self.total as usize;
        let full = total / 64;
        let rest = total % 64;

        if !self.bitmap[..full].iter().all(|&w| w == u64::MAX) {
            return false;
        }
        if rest == 0 {
            return true;
        }
        let mask = (1u64 << rest) - 1;
        self.bitmap[full] & mask == mask
    }

    /// 尚未完成的分片列表
    pub fn pending(&self) -> Vec<u32> {
        (0..self.total).filter(|&i| !self.is_done(i)).collect()
    }

    pub fn done_count(&self) -> u32 {
        (0..self.total).filter(|&i| self.is_done(i)).count() as u32
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn small_file_is_single_piece() {
        for size in [0u64, 1, 1024, 7 * 1024 * 1024] {
            let t = PieceTable::new(size);
            assert_eq!(t.total(), 1, "size={size} 应不分片");
            assert!(!t.is_split());
        }
    }

    #[test]
    fn medium_file_uses_4m_pieces() {
        // 10 MB -> 4MB 分片 -> 3 片
        let t = PieceTable::new(10 * 1024 * 1024);
        assert_eq!(t.piece_size(), PIECE_MID);
        assert_eq!(t.total(), 3);
        assert!(t.is_split());
    }

    #[test]
    fn large_file_uses_16m_pieces() {
        // 10 GB / 16MB = 640 片
        let size = 10u64 * 1024 * 1024 * 1024;
        let t = PieceTable::new(size);
        assert_eq!(t.piece_size(), PIECE_LARGE);
        assert_eq!(t.total(), 640);
    }

    #[test]
    fn range_covers_whole_file() {
        let size = 10 * 1024 * 1024;
        let t = PieceTable::new(size);
        let mut covered = 0u64;
        let mut prev_end = 0u64;
        for i in 0..t.total() {
            let (s, e) = t.range(i).unwrap();
            assert_eq!(s, prev_end, "分片 {i} 起点不连续");
            assert!(e > s);
            covered += e - s;
            prev_end = e;
        }
        assert_eq!(covered, size);
        assert_eq!(prev_end, size);
    }

    #[test]
    fn last_piece_is_truncated() {
        // 9 MB -> 4+4+1
        let t = PieceTable::new(9 * 1024 * 1024);
        let (s, e) = t.range(t.total() - 1).unwrap();
        assert_eq!(e - s, 1024 * 1024, "最后一片应只有 1MB");
    }

    #[test]
    fn bitmap_marks_and_all_done() {
        let mut t = PieceTable::new(10 * 1024 * 1024);
        assert!(!t.all_done());
        for i in 0..t.total() {
            assert!(!t.is_done(i));
            t.mark_inflight(i);
            t.mark_done(i);
            assert!(t.is_done(i));
        }
        assert!(t.all_done());
        assert!(t.pending().is_empty());
        assert_eq!(t.done_count(), t.total());
    }

    #[test]
    fn all_done_at_exact_word_boundary() {
        // 64 片整（256 MB / 4MB），验证 bitmap[full] 不越界
        let t = PieceTable::new(256 * 1024 * 1024);
        assert_eq!(t.total(), 64);
        assert!(!t.all_done());
    }

    #[test]
    fn release_returns_to_pending() {
        let mut t = PieceTable::new(10 * 1024 * 1024);
        t.mark_inflight(0);
        t.release(0);
        assert!(!t.is_done(0));
        assert!(t.pending().contains(&0));
    }
}
