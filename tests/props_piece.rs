//! `PieceTable::with_conns` 的属性测试（proptest）。
//!
//! 测的是**不变量**，不是某个输入的期望输出：片大小是
//! `floor(clamp(file_size / conns, MIN, MAX))`（2026-11 起再向下取整到 4 KiB 页），
//! 写死「10 MiB / 4 连接 ⇒ 2621440 字节」这类期望值，会让每一次公式微调
//! 都变成一次测试改写 —— 而 issue #3 要的恰恰是「公式变了，不变量还成立」。
//!
//! 每条不变量一个 `proptest` 函数，失败时 proptest 会把反例**缩小**到最小输入
//! 并打印出来；报告里照抄那组输入就能当普通 `#[test]` 复现。
//!
//! **领域上限 4 GiB**：`total` 是 `u32`，再大的文件会先撞上 `total as u32` 截断
//! （>64 PiB 才可能），那是另一条独立契约，不塞进这里制造与本改动无关的反例。
//!
//! **刻意关闭反例持久化**（`failure_persistence: None`）：默认配置失败时会往
//! 仓库根写 `proptest-regressions/`，而本改动的写范围只有 `Cargo.toml` /
//! `Cargo.lock` / 两个 `tests/*.rs`。反例看测试输出即可。
//!
//! **不引用实现里的常量**：`piece::MIN_PIECE` / `MAX_PIECE` 是私有的；
//! `piece::PAGE` 虽然是 pub，但拿实现自己的常量验证实现自己的不变量是循环论证。
//! 下面的契约值来自 issue #3 与模块文档，改契约时同步这几行。

use mc_downloader::piece::PieceTable;
use proptest::prelude::*;
use proptest::test_runner::Config as ProptestConfig;

/// 4 KiB 页：片起点（= 写偏移）必须落在它的整数倍上。
const PAGE_BYTES: u64 = 4096;
/// 片大小下限（契约语义值，见 `piece.rs` 模块文档）。
const MIN_PIECE: u64 = 1024 * 1024;
/// 片大小上限（契约语义值，见 `piece.rs` 模块文档）。
const MAX_PIECE: u64 = 16 * 1024 * 1024;
/// 低于它不分片（契约语义值，见 `piece.rs` 模块文档）。
const NO_SPLIT_BELOW: u64 = 8 * 1024 * 1024;
/// 本文件覆盖的文件大小上限（见文件头「领域上限」）。
const MAX_FILE_SIZE: u64 = 4 * 1024 * 1024 * 1024;

/// 文件大小：小文件 / 阈值两侧 / 任意字节 / 整页倍数 / 夹逼端点都要出现。
///
/// 整页倍数那一路是必要的：只随机普通字节数的话，「向下取整到页」这一步
/// 对不少输入是恒等变换，取整写错也测不出来。
fn size_strategy() -> impl Strategy<Value = u64> {
    prop_oneof![
        // 空文件与个位数：不分片分支的退化输入
        0u64..=16u64,
        // 小文件分支的其余部分（含远低于阈值）
        0u64..NO_SPLIT_BELOW,
        // 8 MiB 阈值两侧：跨过它才进入"按 conns 切"的分支
        (NO_SPLIT_BELOW - PAGE_BYTES * 2)..=(NO_SPLIT_BELOW + PAGE_BYTES * 2),
        // 主体：任意字节数，多数除不尽
        NO_SPLIT_BELOW..=MAX_FILE_SIZE,
        // 整页倍数：让"除得尽"与"除不尽"两类都出现
        (NO_SPLIT_BELOW / PAGE_BYTES..=MAX_FILE_SIZE / PAGE_BYTES)
            .prop_map(|pages| pages * PAGE_BYTES),
        // 夹逼两个端点的紧邻值
        prop_oneof![
            Just(MIN_PIECE - 1),
            Just(MIN_PIECE),
            Just(MIN_PIECE + 1),
            Just(MAX_PIECE),
            Just(MAX_PIECE + 1),
            Just(MAX_FILE_SIZE),
        ],
    ]
}

/// 并发数：0（必须当 1 处理）、1、常规区间、以及 `usize::MAX`。
fn conns_strategy() -> impl Strategy<Value = usize> {
    prop_oneof![
        Just(0usize),
        Just(1usize),
        1usize..=8usize,
        1usize..=64usize,
        Just(1000usize),
        Just(usize::MAX),
    ]
}

proptest! {
    #![proptest_config(ProptestConfig {
        cases: 256,
        failure_persistence: None,
        ..ProptestConfig::default()
    })]

    /// ① 片区间首尾相接、覆盖整个 `[0, file_size)`，总和恰好等于 `file_size`。
    ///
    /// 这条是「续传拼不拼得出正确文件」的地基：任何缝隙或重叠都会让
    /// `.part.state` 的位图与实际写出范围失去对应关系。
    #[test]
    fn ranges_tile_the_whole_file(size in size_strategy(), conns in conns_strategy()) {
        let t = PieceTable::with_conns(size, conns);
        let piece_size = t.piece_size();
        prop_assert!(piece_size >= 1, "file_size={} conns={} 片大小为 0", size, conns);
        prop_assert!(t.total() >= 1, "file_size={} conns={} 片数为 0", size, conns);
        prop_assert_eq!(
            t.total() as u64,
            size.div_ceil(piece_size).max(1),
            "file_size={} conns={} 片数 {} 与 ceil(size/piece_size) 不一致",
            size, conns, t.total()
        );
        // 位图按 64 片/字分配：字数必须刚好够，多一个少一个都会让 is_done 越界或漏片
        prop_assert_eq!(
            t.bitmap().len() as u64,
            (t.total() as u64).div_ceil(64),
            "file_size={} conns={} 位图字数不对",
            size, conns
        );

        let mut cursor = 0u64;
        let mut covered = 0u64;
        for idx in 0..t.total() {
            let (start, end) = t.range(idx).expect("分片下标必须可取");
            prop_assert_eq!(
                start, cursor,
                "file_size={} conns={} 第 {} 片起点不等于上一片终点",
                size, conns, idx
            );
            if size > 0 {
                prop_assert!(end > start, "非空文件的第 {} 片是 0 长", idx);
            }
            covered += end - start;
            cursor = end;
        }
        prop_assert_eq!(
            cursor, size,
            "file_size={} conns={} 末片终点未到 EOF",
            size, conns
        );
        prop_assert_eq!(
            covered, size,
            "file_size={} conns={} 覆盖字节数不等于文件大小",
            size, conns
        );
        prop_assert!(t.range(t.total()).is_none(), "越界下标必须返回 None");
    }

    /// ② 小文件不分片：`total == 1` 且唯一一片就是 `[0, file_size)`。
    ///
    /// 阈值是设计选择（握手开销 > 收益），不是"顺手"—— 它决定了 8 MiB 以下的
    /// 文件永远只有一条连接，也就是 `piece_conns` 对小文件无效。
    #[test]
    fn small_files_are_one_piece(size in 0u64..NO_SPLIT_BELOW, conns in conns_strategy()) {
        let t = PieceTable::with_conns(size, conns);
        prop_assert_eq!(t.total(), 1, "file_size={} conns={} 不该分片", size, conns);
        prop_assert!(!t.is_split(), "file_size={} conns={} is_split 应为 false", size, conns);
        prop_assert_eq!(
            t.range(0), Some((0, size)),
            "file_size={} conns={} 单片的区间应为整文件",
            size, conns
        );
    }

    /// ③ 片大小落在契约区间的语义内：分片时 ∈ `[MIN_PIECE, MAX_PIECE]`，
    ///    且永远不超过文件本身，也永远不是 0（否则 `div_ceil` 会除零）。
    #[test]
    fn piece_size_stays_within_contract_bounds(
        size in NO_SPLIT_BELOW..=MAX_FILE_SIZE,
        conns in conns_strategy(),
    ) {
        let t = PieceTable::with_conns(size, conns);
        let ps = t.piece_size();
        prop_assert!(
            (MIN_PIECE..=MAX_PIECE).contains(&ps),
            "file_size={} conns={} 片大小 {} 越出 [{}, {}]",
            size, conns, ps, MIN_PIECE, MAX_PIECE
        );
        prop_assert!(
            ps <= size,
            "file_size={} conns={} 片大小 {} 超过文件本身",
            size, conns, ps
        );
    }

    /// ④ 片数不小于「连接数」与「按最小片切得动」二者的小者：
    ///    `total >= min(conns, file_size / MIN_PIECE)`。
    ///
    /// 这是把因果关系摆正的契约：**先知道要几个连接，再决定片多大**。
    /// 旧公式（先按大小切死）会让片数成为并发的隐性天花板，
    /// 这一条就是那个天花板的回归判据。
    #[test]
    fn piece_count_meets_fanout_target(
        size in NO_SPLIT_BELOW..=MAX_FILE_SIZE,
        conns in conns_strategy(),
    ) {
        let t = PieceTable::with_conns(size, conns);
        let target = (conns as u64).min(size / MIN_PIECE);
        prop_assert!(
            u64::from(t.total()) >= target,
            "file_size={} conns={} 只切出 {} 片，低于目标 {} 片（片大小 {}）",
            size, conns, t.total(), target, t.piece_size()
        );
    }

    /// ⑤ 每片起点（= 写偏移）都在 4 KiB 页边界上；非末片长度也是整页。
    ///
    /// 末片止于 EOF，长度天然可以不是整页 —— 它是唯一合法的例外。
    /// 这条**依赖 lead 的 4 KiB 对齐改动**（`floor_to_page`）：未落地时它会红，
    /// 那是对的，不许为了变绿去放宽断言。
    #[test]
    fn every_piece_starts_on_page_boundary(
        size in size_strategy(),
        conns in conns_strategy(),
    ) {
        let t = PieceTable::with_conns(size, conns);
        for idx in 0..t.total() {
            let (start, end) = t.range(idx).expect("分片下标必须可取");
            prop_assert_eq!(
                start % PAGE_BYTES, 0,
                "file_size={} conns={} 第 {} 片起点 {} 不在 {} 字节页边界上（片大小 {}）",
                size, conns, idx, start, PAGE_BYTES, t.piece_size()
            );
            if idx + 1 < t.total() {
                prop_assert_eq!(
                    (end - start) % PAGE_BYTES, 0,
                    "file_size={} conns={} 第 {} 片长度 {} 不是整页",
                    size, conns, idx, end - start
                );
            }
        }
    }
}
