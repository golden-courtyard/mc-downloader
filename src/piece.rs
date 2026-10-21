//! 分片表：固定大小分片 + 位图状态。
//!
//! 设计参照 DeepInfer 的 block table（`csrc/engine/block_manager.hpp`）
//! 与 aria2 的 `min-split-size`：**小文件不分片**。

use std::io;
use std::path::{Path, PathBuf};

/// 小文件阈值：低于此值不分片（握手开销 > 收益）
const NO_SPLIT_BELOW: u64 = 8 * 1024 * 1024; // 8 MB
/// 中等文件分片大小
const PIECE_MID: u64 = 4 * 1024 * 1024; // 4 MB
/// 大文件分片大小
const PIECE_LARGE: u64 = 16 * 1024 * 1024; // 16 MB
/// 进入大文件分片的阈值
const LARGE_ABOVE: u64 = 256 * 1024 * 1024; // 256 MB

#[derive(Debug, Clone)]
pub struct PieceTable {
    file_size: u64,
    piece_size: u64,
    total: u32,
    /// 每 bit 一片：0 未完成 / 1 完成
    bitmap: Vec<u64>,
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

    /// 标记一片完成。
    ///
    /// 「哪片正在下载」不由本表记录 —— 那些片段还在 `PieceScheduler` 的队列里，
    /// 队列本身就是 in-flight 集合。这里只管终态。
    pub fn mark_done(&mut self, idx: u32) {
        if idx >= self.total {
            return;
        }
        let (w, b) = ((idx / 64) as usize, idx % 64);
        self.bitmap[w] |= 1u64 << b;
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

    /// 已完成分片覆盖的字节数（续传时把进度条摆到正确位置）
    pub fn done_bytes(&self) -> u64 {
        (0..self.total)
            .filter(|&i| self.is_done(i))
            .filter_map(|i| self.range(i).map(|(s, e)| e - s))
            .sum()
    }

    pub fn bitmap(&self) -> &[u64] {
        &self.bitmap
    }

    /// 用已保存的位图重建；长度与当前分片数不符则 `None`
    pub fn restore(file_size: u64, bitmap: Vec<u64>) -> Option<Self> {
        let mut t = Self::new(file_size);
        if bitmap.len() != t.bitmap.len() {
            return None;
        }
        t.bitmap = bitmap;
        Some(t)
    }
}

/// `.part.state` 的读写。
///
/// **为什么需要旁挂状态文件**：`set_len` 预分配出来的是稀疏文件，
/// 未写过的区域读出来是 0 —— 与「真的写入了 0 值字节」无法区分。
/// 所以哪些片段已完成**不能从文件内容反推**，只能显式记账。
///
/// 格式（小端）：magic(8) · file_size(8) · piece_size(8) · total(4) · words(4) · bitmap(words×8)
pub struct PieceState;

const STATE_MAGIC: &[u8; 8] = b"MCDLPC01";
const STATE_HEADER: usize = 32;

impl PieceState {
    /// `x.part` -> `x.part.state`
    pub fn path_for(part: &Path) -> PathBuf {
        let mut s = part.as_os_str().to_os_string();
        s.push(".state");
        PathBuf::from(s)
    }

    /// 读回位图。缺失 / 头部不匹配 / 长度不足，一律 `None`（调用方按从头下处理）。
    pub fn load(part: &Path, expect_file_size: u64) -> Option<PieceTable> {
        let raw = std::fs::read(Self::path_for(part)).ok()?;
        if raw.len() < STATE_HEADER || &raw[..8] != STATE_MAGIC {
            return None;
        }
        let file_size = u64::from_le_bytes(raw[8..16].try_into().ok()?);
        let piece_size = u64::from_le_bytes(raw[16..24].try_into().ok()?);
        let total = u32::from_le_bytes(raw[24..28].try_into().ok()?);
        let words = u32::from_le_bytes(raw[28..32].try_into().ok()?) as usize;

        // 与按当前文件大小重新推导的结果逐项对照：任一不符都说明状态过期
        let probe = PieceTable::new(expect_file_size);
        if file_size != expect_file_size
            || piece_size != probe.piece_size()
            || total != probe.total()
            || raw.len() != STATE_HEADER + words * 8
        {
            return None;
        }

        let mut bitmap = Vec::with_capacity(words);
        for i in 0..words {
            let b = STATE_HEADER + i * 8;
            bitmap.push(u64::from_le_bytes(raw[b..b + 8].try_into().ok()?));
        }
        PieceTable::restore(expect_file_size, bitmap)
    }

    /// 先写 `.tmp` 再改名：崩溃时不会留下半截状态被误读成有效进度
    pub fn save(part: &Path, t: &PieceTable) -> io::Result<()> {
        let words = t.bitmap().len();
        let mut buf = Vec::with_capacity(STATE_HEADER + words * 8);
        buf.extend_from_slice(STATE_MAGIC);
        buf.extend_from_slice(&t.file_size().to_le_bytes());
        buf.extend_from_slice(&t.piece_size().to_le_bytes());
        buf.extend_from_slice(&t.total().to_le_bytes());
        buf.extend_from_slice(&(words as u32).to_le_bytes());
        for w in t.bitmap() {
            buf.extend_from_slice(&w.to_le_bytes());
        }

        let path = Self::path_for(part);
        let tmp = path.with_extension("state.tmp");
        std::fs::write(&tmp, &buf)?;
        std::fs::rename(&tmp, &path)
    }

    pub fn remove(part: &Path) {
        let _ = std::fs::remove_file(Self::path_for(part));
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

    /// 每个用例独立的临时 `.part` 路径
    fn tmp_part(tag: &str) -> PathBuf {
        let mut p = std::env::temp_dir();
        p.push(format!("mcd-piece-test-{tag}-{}.part", std::process::id()));
        let _ = std::fs::remove_file(&p);
        PieceState::remove(&p);
        p
    }

    #[test]
    fn state_roundtrip_restores_bitmap() {
        let part = tmp_part("rt");
        let mut t = PieceTable::new(10 * 1024 * 1024); // 3 片
        t.mark_done(0);
        t.mark_done(2);
        PieceState::save(&part, &t).expect("save 应当成功");

        let back = PieceState::load(&part, 10 * 1024 * 1024).expect("load 应当成功");
        assert_eq!(back.total(), 3);
        assert!(back.is_done(0));
        assert!(!back.is_done(1));
        assert!(back.is_done(2));
        assert_eq!(back.pending(), vec![1]);
        PieceState::remove(&part);
    }

    #[test]
    fn state_survives_repetitive_saves() {
        // 模拟 flusher 每秒覆盖写：第二次 save 要能覆盖第一次
        let part = tmp_part("over");
        let mut t = PieceTable::new(10 * 1024 * 1024);
        for i in 0..3u32 {
            t.mark_done(i);
            PieceState::save(&part, &t).expect("重复 save 应当成功");
            let back = PieceState::load(&part, 10 * 1024 * 1024).expect("load 应当成功");
            assert_eq!(back.done_count(), i + 1);
        }
        PieceState::remove(&part);
    }

    #[test]
    fn state_rejects_other_file_size() {
        let part = tmp_part("size");
        let t = PieceTable::new(10 * 1024 * 1024);
        PieceState::save(&part, &t).unwrap();
        // 大小对不上（分片数也变了）-> 必须作废，不能拿旧位图去拼新文件
        assert!(PieceState::load(&part, 40 * 1024 * 1024).is_none());
        PieceState::remove(&part);
    }

    #[test]
    fn state_rejects_garbage_and_missing() {
        let part = tmp_part("bad");
        assert!(PieceState::load(&part, 10 * 1024 * 1024).is_none(), "缺失应为 None");
        std::fs::write(PieceState::path_for(&part), b"not a state file").unwrap();
        assert!(PieceState::load(&part, 10 * 1024 * 1024).is_none());
        std::fs::write(PieceState::path_for(&part), b"MCDLPC01garbage").unwrap();
        assert!(PieceState::load(&part, 10 * 1024 * 1024).is_none());
        PieceState::remove(&part);
    }

    #[test]
    fn done_bytes_matches_completed_ranges() {
        let mut t = PieceTable::new(9 * 1024 * 1024); // 4+4+1
        assert_eq!(t.done_bytes(), 0);
        t.mark_done(0);
        assert_eq!(t.done_bytes(), 4 * 1024 * 1024);
        t.mark_done(2); // 最后一片只有 1MB
        assert_eq!(t.done_bytes(), 5 * 1024 * 1024);
        t.mark_done(1);
        assert_eq!(t.done_bytes(), 9 * 1024 * 1024);
    }

    #[test]
    fn restore_rejects_wrong_word_count() {
        assert!(PieceTable::restore(10 * 1024 * 1024, vec![]).is_none());
        assert!(PieceTable::restore(10 * 1024 * 1024, vec![0u64; 7]).is_none());
        assert!(PieceTable::restore(10 * 1024 * 1024, vec![0u64; 1]).is_some());
    }
}
