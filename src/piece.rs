//! 分片表：固定大小分片 + 位图状态。
//!
//! 设计参照 DeepInfer 的 block table（`csrc/engine/block_manager.hpp`）
//! 与 aria2 的 `min-split-size`：**小文件不分片**。

use std::io;
use std::path::{Path, PathBuf};

/// 小文件阈值：低于此值不分片（握手开销 > 收益）
const NO_SPLIT_BELOW: u64 = 8 * 1024 * 1024; // 8 MB
/// 片大小下限：再小的话，每片的 TCP+TLS 握手开销摊不平
const MIN_PIECE: u64 = 1024 * 1024; // 1 MB
/// 片大小上限：与旧的 PIECE_LARGE 对齐，保持大文件行为不变
const MAX_PIECE: u64 = 16 * 1024 * 1024; // 16 MB
/// 默认并发数（与 `download::DEFAULT_PIECE_CONNS` 同值；此处独立定义，
/// 免得 piece.rs 反向依赖 download.rs）
const DEFAULT_CONNS: usize = 4;
/// 页大小：片边界对齐到它。
///
/// **只值三件可测的小事**：
/// ① 相邻两片不再共享同一个页缓存页（少一次页锁串行）；
/// ② 片首尾不会产生「不足整扇区」的下发（`Split IO/Sec` 能看到差异）；
/// ③ 为将来可能的直写（`O_DIRECT` / `FILE_FLAG_NO_BUFFERING`，都要求偏移与
///    长度按页对齐）留出前提。
///
/// **不宣称的东西**：这不等于「HDD 友好」。机械盘的对齐旋钮在**磁道 / 大 extent**
/// 量级（Track-Aligned Extents, FAST '02），4 KiB 远小于那个粒度；而乱序写的
/// 代价由**写顺序**决定，不由页对齐决定（乱序写的立项另见 decision note）。
pub const PAGE: u64 = 4096;

/// 向下取整到页边界。
///
/// 只对**已经很正常**的输入用：`MIN_PIECE` 是页的整数倍，所以 `x >= MIN_PIECE`
/// 时结果仍 `>= MIN_PIECE`；小于一页的值（只可能来自"不分片"分支）不走这里。
fn floor_to_page(x: u64) -> u64 {
    x & !(PAGE - 1)
}

#[derive(Debug, Clone)]
pub struct PieceTable {
    file_size: u64,
    piece_size: u64,
    total: u32,
    /// 每 bit 一片：0 未完成 / 1 完成
    bitmap: Vec<u64>,
}

impl PieceTable {
    /// 默认并发（4）切分 —— 给单测与"不知道并发数"的调用方用。
    pub fn new(file_size: u64) -> Self {
        Self::with_conns(file_size, DEFAULT_CONNS)
    }

    /// 按目标并发数切分：**让片数 ≥ conns**，使每个连接都有活干。
    ///
    /// 旧实现是"先按文件大小切死（4 MB / 16 MB），再看片数够不够"，
    /// 结果 `download.rs` 里的 `min(piece_conns, n_pieces)` 会让片数成为并发上限的
    /// 隐性天花板：**256 MB 的文件只有 16 片，配 32 连接也只有 16 个在用**。
    ///
    /// 现在把因果反过来：**先知道要几个连接，再决定片多大**。
    /// 夹逼 [MIN_PIECE, MAX_PIECE] 是因为两头都有代价 ——
    /// 片太小则每片的握手开销占比过高，片太大则并发用不满。
    pub fn with_conns(file_size: u64, conns: usize) -> Self {
        let piece_size = if file_size < NO_SPLIT_BELOW {
            // 不分片：一片覆盖整个文件（起点 0，本身在页边界上）
            file_size.max(1)
        } else {
            let target = conns.max(1) as u64;
            // 夹逼之后**向下取整到页**。为什么是向下：
            //   片变小只会让片数变多，「片数 ≥ conns」这个契约只会更稳；
            //   向上取整会把片数压下去，正好与契约相反。
            // 为什么取整不会越界：MIN_PIECE / MAX_PIECE 本身就是页的整数倍
            //   （1 MiB = 256 页、16 MiB = 4096 页），所以 floor 之后仍落在区间内。
            floor_to_page((file_size / target).clamp(MIN_PIECE, MAX_PIECE))
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
        Self::restore_with_conns(file_size, DEFAULT_CONNS, bitmap)
    }

    /// 同上，但按**调用方实际用的并发数**重建切片 —— 片大小是 `size / conns`
    /// 的函数，conns 不同则片边界不同，位图的含义也随之不同。
    ///
    /// 之前这里只有 `DEFAULT_CONNS = 4` 的版本，而 `download::one_split` 用的是
    /// `with_conns(size, opts.piece_conns)`：`--piece-conns` 一旦不是 4，
    /// 续传就会把**自己的**状态判成过期，从头重下（静默失效，不报错）。
    pub fn restore_with_conns(file_size: u64, conns: usize, bitmap: Vec<u64>) -> Option<Self> {
        let mut t = Self::with_conns(file_size, conns);
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
    ///
    /// ⚠️ 这条只按 `DEFAULT_CONNS` 重建切片；**下载路径请用 [`Self::load_with_conns`]**，
    /// 否则 `--piece-conns` 不是 4 时会把有效状态判成过期。
    pub fn load(part: &Path, expect_file_size: u64) -> Option<PieceTable> {
        Self::load_with_conns(part, expect_file_size, DEFAULT_CONNS)
    }

    /// 同上，但按调用方**实际使用的并发数**重建切片后逐项对照。
    pub fn load_with_conns(part: &Path, expect_file_size: u64, conns: usize) -> Option<PieceTable> {
        let raw = std::fs::read(Self::path_for(part)).ok()?;
        if raw.len() < STATE_HEADER || &raw[..8] != STATE_MAGIC {
            return None;
        }
        let file_size = u64::from_le_bytes(raw[8..16].try_into().ok()?);
        let piece_size = u64::from_le_bytes(raw[16..24].try_into().ok()?);
        let total = u32::from_le_bytes(raw[24..28].try_into().ok()?);
        let words = u32::from_le_bytes(raw[28..32].try_into().ok()?) as usize;

        // 与按当前文件大小 + 当前并发数重新推导的结果逐项对照：任一不符都说明状态过期。
        // 片边界一致 ⇒ 位图含义一致；边界变了（公式改了 / conns 变了）⇒ 宁可重下。
        let probe = PieceTable::with_conns(expect_file_size, conns);
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
        PieceTable::restore_with_conns(expect_file_size, conns, bitmap)
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
    fn medium_file_splits_by_conns() {
        // 新契约：片大小 = size / conns（夹逼 [1MB, 16MB]），目的是让片数 >= conns
        // 10 MB / 4 连接 = 2.5 MB 一片 -> 4 片
        let t = PieceTable::new(10 * 1024 * 1024);
        assert_eq!(t.piece_size(), 10 * 1024 * 1024 / 4);
        assert_eq!(t.total(), 4);
        assert!(t.is_split());
    }

    #[test]
    fn large_file_uses_16m_pieces() {
        // 10 GB / 16MB = 640 片
        let size = 10u64 * 1024 * 1024 * 1024;
        let t = PieceTable::new(size);
        // 10GB/4 = 2.5GB，被 MAX_PIECE 夹到 16MB -> 仍 640 片（与旧行为一致）
        assert_eq!(t.piece_size(), MAX_PIECE);
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
        // 用"除不尽"的组合：10MB / 3 连接 = 3.33MB 片 -> 4 片，最后一片比片大小短。
        // **不再硬编码具体字节数**（片大小现在是 size/conns 算出来的）
        let size = 10 * 1024 * 1024;
        let t = PieceTable::with_conns(size, 3);
        let ps = t.piece_size();
        let (s0, e0) = t.range(t.total() - 1).unwrap();
        assert!(
            e0 - s0 < ps,
            "最后一片应短于片大小（末片 {} 字节 < {}）",
            e0 - s0,
            ps
        );
        assert_eq!(e0, size, "最后一片的 end 必须等于文件大小");
        assert_eq!(s0, ps * (t.total() as u64 - 1));
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
        // bitmap 按 64 bit/word 分配；用"片数恰好落在 word 内"的尺寸验不越界。
        // 256MB / 4 连接 = 64MB，被 MAX_PIECE 夹到 16MB -> 16 片（1 word 足够）
        let t = PieceTable::new(256 * 1024 * 1024);
        assert_eq!(t.total(), 16);
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
    fn piece_size_is_page_aligned() {
        // 片大小 = floor(clamp(size/conns, 1MiB, 16MiB))，再取整到 PAGE。
        // 只有**除不尽**的组合才真正测到取整（能整除时取整是恒等）。
        for (size_mb, conns) in [
            (9u64, 3usize),
            (16, 3),
            (97, 7),
            (100, 3),
            (256, 5),
            (1024, 3),
        ] {
            let size = size_mb * 1024 * 1024;
            let t = PieceTable::with_conns(size, conns);
            assert_eq!(
                t.piece_size() % PAGE,
                0,
                "{size_mb} MiB / {conns} 连接 ⇒ 片大小 {} 不是 {PAGE} 的倍数",
                t.piece_size()
            );
        }
    }

    #[test]
    fn every_piece_starts_on_page_boundary() {
        // 片起点就是写偏移（download.rs 按 `pj.start` seek 后整片写出），
        // 所以这条等价于「每片写入的起点都在页边界上」。
        for (size_mb, conns) in [(9u64, 3usize), (97, 7), (100, 3), (256, 5)] {
            let size = size_mb * 1024 * 1024;
            let t = PieceTable::with_conns(size, conns);
            for i in 0..t.total() {
                let (s, e) = t.range(i).unwrap();
                assert_eq!(s % PAGE, 0, "片 {i} 起点 {s} 不在页边界");
                // 末片止于 EOF，长度天然可以不是整页；其余片都应当是整页
                if i + 1 < t.total() {
                    assert_eq!((e - s) % PAGE, 0, "片 {i} 长度 {} 不是整页", e - s);
                }
            }
        }
    }

    #[test]
    fn floor_to_page_rounds_down_and_keeps_the_floor() {
        assert_eq!(floor_to_page(PAGE - 1), 0);
        assert_eq!(floor_to_page(PAGE), PAGE);
        assert_eq!(floor_to_page(PAGE + 1), PAGE);
        // 除不尽的典型值：10 MiB / 3
        let raw = 10 * 1024 * 1024 / 3;
        let f = floor_to_page(raw);
        assert!(f <= raw && raw - f < PAGE);
        assert_eq!(f % PAGE, 0);
        // MIN_PIECE 本身就是页的整数倍 ⇒ 取整不会把它压到区间下界以下
        assert_eq!(floor_to_page(MIN_PIECE), MIN_PIECE);
        assert_eq!(MIN_PIECE % PAGE, 0);
        assert_eq!(MAX_PIECE % PAGE, 0);
    }

    #[test]
    fn state_roundtrip_preserves_non_default_conns() {
        // 回归：`--piece-conns 7` 写下的状态必须能被 load_with_conns(…, 7) 读回。
        // 旧实现固定按 DEFAULT_CONNS=4 重建判据 ⇒ 这里会 None，续传静默失效、每次从头下。
        let size = 100 * 1024 * 1024;
        let part = tmp_part("conns");
        let mut t = PieceTable::with_conns(size, 7);
        t.mark_done(0);
        t.mark_done(t.total() - 1);
        PieceState::save(&part, &t).expect("save 应当成功");

        let back = PieceState::load_with_conns(&part, size, 7).expect("按 7 连应当读回");
        assert_eq!(back.total(), t.total());
        assert_eq!(back.piece_size(), t.piece_size());
        assert!(back.is_done(0));
        assert!(back.is_done(t.total() - 1));

        // 用错的 conns 去读 ⇒ 片边界不同 ⇒ 必须判过期（宁重下，不拼坏文件）
        assert!(PieceState::load_with_conns(&part, size, 4).is_none());
        PieceState::remove(&part);
    }

    #[test]
    fn state_with_unaligned_piece_size_is_rejected() {
        // 手写一个「页对齐之前」的状态：piece_size 不是 PAGE 的倍数。
        // 公式改过之后这种状态会出现在用户磁盘上 —— 必须被拒，而不是当成有效进度去拼文件。
        let size = 100 * 1024 * 1024;
        let part = tmp_part("unaligned");
        let old_piece = size / 3; // 34_952_533：旧公式的输出，故意不是整页
        assert_ne!(old_piece % PAGE, 0);
        let total = size.div_ceil(old_piece) as u32;
        let mut buf = Vec::new();
        buf.extend_from_slice(STATE_MAGIC);
        buf.extend_from_slice(&size.to_le_bytes());
        buf.extend_from_slice(&old_piece.to_le_bytes());
        buf.extend_from_slice(&total.to_le_bytes());
        buf.extend_from_slice(&1u32.to_le_bytes());
        buf.extend_from_slice(&u64::MAX.to_le_bytes());
        std::fs::write(PieceState::path_for(&part), &buf).unwrap();

        assert!(PieceState::load_with_conns(&part, size, 3).is_none());
        assert!(PieceState::load(&part, size).is_none());
        PieceState::remove(&part);
    }

    #[test]
    fn piece_count_meets_conn_target() {
        // **这是本次改动的契约**：片大小 = size/conns（夹逼 [1MB,16MB]），
        // 目的是让片数 >= 目标连接数 —— 否则 download.rs 的
        // `min(piece_conns, n_pieces)` 会让片数成为并发的隐性天花板。
        for (size_mb, conns) in [(100u64, 32usize), (256, 32), (1024, 32), (100, 8)] {
            let t = PieceTable::with_conns(size_mb * 1024 * 1024, conns);
            assert!(
                t.total() as usize >= conns,
                "{size_mb}MB / {conns} 连接 => 只有 {} 片（片大小 {} 字节）",
                t.total(),
                t.piece_size()
            );
        }
    }

    #[test]
    fn piece_size_is_clamped_both_ends() {
        // 下限：小文件配 32 连接，片不会小于 1MB（否则握手开销吃掉收益）
        let t = PieceTable::with_conns(16 * 1024 * 1024, 32);
        assert_eq!(t.piece_size(), MIN_PIECE);
        // 上限：超大文件配 4 连接，片不会大于 16MB
        let t2 = PieceTable::with_conns(10 * 1024 * 1024 * 1024, 4);
        assert_eq!(t2.piece_size(), MAX_PIECE);
    }

    #[test]
    fn state_roundtrip_restores_bitmap() {
        let part = tmp_part("rt");
        let mut t = PieceTable::new(10 * 1024 * 1024); // 4 片（size/conns=2.5MB）
        t.mark_done(0);
        t.mark_done(2);
        PieceState::save(&part, &t).expect("save 应当成功");

        let back = PieceState::load(&part, 10 * 1024 * 1024).expect("load 应当成功");
        assert_eq!(back.total(), 4);
        assert!(back.is_done(0));
        assert!(!back.is_done(1));
        assert!(back.is_done(2));
        assert_eq!(back.pending(), vec![1, 3]);
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
        assert!(
            PieceState::load(&part, 10 * 1024 * 1024).is_none(),
            "缺失应为 None"
        );
        std::fs::write(PieceState::path_for(&part), b"not a state file").unwrap();
        assert!(PieceState::load(&part, 10 * 1024 * 1024).is_none());
        std::fs::write(PieceState::path_for(&part), b"MCDLPC01garbage").unwrap();
        assert!(PieceState::load(&part, 10 * 1024 * 1024).is_none());
        PieceState::remove(&part);
    }

    #[test]
    fn done_bytes_matches_completed_ranges() {
        let size = 10 * 1024 * 1024;
        let mut t = PieceTable::with_conns(size, 3); // 末片比片大小短
        let ps = t.piece_size();
        let last_len = size - ps * (t.total() as u64 - 1);

        assert_eq!(t.done_bytes(), 0);
        t.mark_done(0);
        assert_eq!(t.done_bytes(), ps);
        t.mark_done(t.total() - 1); // 最后一片短一截
        assert_eq!(t.done_bytes(), ps + last_len);
        for i in 1..t.total() - 1 {
            t.mark_done(i);
        }
        assert_eq!(t.done_bytes(), size, "全标完后应正好等于文件大小");
    }

    #[test]
    fn restore_rejects_wrong_word_count() {
        assert!(PieceTable::restore(10 * 1024 * 1024, vec![]).is_none());
        assert!(PieceTable::restore(10 * 1024 * 1024, vec![0u64; 7]).is_none());
        assert!(PieceTable::restore(10 * 1024 * 1024, vec![0u64; 1]).is_some());
    }
}
