//! `DownloadOpts::split_for` 的参数化用例（rstest）。
//!
//! **为什么住在 `tests/` 而不是 `src/download.rs` 的 `#[cfg(test)]`**
//! ① `split_for` 是 pub API（`main.rs` 按任务数均分总并发时就这么用），
//!    在库外调用它才是真实调用面；
//! ② issue #3 要的是「加边界只加一行」—— `#[case]` 表本身就是边界清单，
//!    原来三个同形状的 `#[test]` 合成一张表；
//! ③ 不与 `src/` 的写范围抢地方（lead 正在改 `src/download.rs`）。
//!
//! **这里只断言契约，不复制实现**：`(concurrency / n).max(1)` 的分母兜底与
//! 下限 1 是行为契约；具体怎么除不在测试的关心范围内。

use mc_downloader::download::{DEFAULT_CONCURRENCY, DownloadOpts};
use rstest::rstest;

/// 默认 16 并发按任务数均分 —— 边界表。
///
/// 覆盖四类：**退化输入**（0 / `usize::MAX`）、**不变点**（1）、
/// **刚好整除**（2 / 4 / 8 / 16）、**刚过头**（17 / 32）。
///
/// * `0`：`0.max(1)` 兜住除零 —— 当作 1 个任务，并发不变；
/// * `usize::MAX`：任何有理除法都得到 0，必须被 `.max(1)` 兜住；
///   若哪天有人把 `.max(1)` 删掉，`buffer_unordered(0)` 会永不推进。
#[rstest]
#[case(0, DEFAULT_CONCURRENCY)]
#[case(1, DEFAULT_CONCURRENCY)]
#[case(2, DIVIDES_EVENLY_AT_2)]
#[case(4, 4)]
#[case(8, 2)]
#[case(16, 1)]
#[case(17, 1)]
#[case(32, 1)]
#[case(usize::MAX, 1)]
fn split_for_shares_concurrency_by_task_count(#[case] tasks: usize, #[case] expected: usize) {
    let s = DownloadOpts::default().split_for(tasks);
    assert_eq!(s.concurrency, expected, "tasks={tasks}");
    assert!(s.concurrency >= 1, "tasks={tasks} 分出了 0 并发");
}

/// 参数化表的常量：`DEFAULT_CONCURRENCY / 2`。
///
/// 写成常量而不是字面量 8，是为了让「默认并发改了多少，这张表跟着改」
/// 这件事变成一处改动（rstest 的 `#[case]` 只接受常量表达式）。
const DIVIDES_EVENLY_AT_2: usize = DEFAULT_CONCURRENCY / 2;

/// 非默认总并发也要按同样的规则均分（`concurrency` 是用户可配的）。
///
/// 这张表覆盖「小于任务数 / 恰好等于 / 大于」三种关系，
/// 以及 `concurrency = 1` 的退化情形（任何任务数都得保住 1）。
#[rstest]
#[case(5, 1, 5)]
#[case(5, 2, 2)]
#[case(5, 5, 1)]
#[case(5, 6, 1)]
#[case(5, usize::MAX, 1)]
#[case(1, 1, 1)]
#[case(1, 7, 1)]
#[case(32, 4, 8)]
fn split_for_uses_configured_concurrency(
    #[case] concurrency: usize,
    #[case] tasks: usize,
    #[case] expected: usize,
) {
    let o = DownloadOpts {
        concurrency,
        ..Default::default()
    };
    let s = o.split_for(tasks);
    assert_eq!(
        s.concurrency, expected,
        "concurrency={concurrency} tasks={tasks}"
    );
    assert!(
        s.concurrency <= concurrency.max(1),
        "concurrency={concurrency} tasks={tasks} 分出了 {} 条（比总数还多）",
        s.concurrency
    );
}

/// 除 `concurrency` 外的字段一律原样带过去。
///
/// 这条防的是「只改了 concurrency，其它字段忘抄」—— 那种错误在小样本下
/// 不会暴露，只会在真实下载时变成「重试次数莫名回到默认」之类的怪事。
#[test]
fn split_for_keeps_every_other_field() {
    let src = DownloadOpts {
        proxy: Some("http://127.0.0.1:7897".into()),
        retries: 7,
        concurrency: DEFAULT_CONCURRENCY,
        piece_conns: 9,
        adaptive: true,
        timeout_secs: 5,
        continue_on_error: true,
        max_speed: Some(1024 * 1024),
    };
    let s = src.split_for(3);

    assert_eq!(s.concurrency, DEFAULT_CONCURRENCY / 3);
    assert_eq!(s.proxy, src.proxy);
    assert_eq!(s.retries, src.retries);
    assert_eq!(s.piece_conns, src.piece_conns);
    assert_eq!(s.adaptive, src.adaptive);
    assert_eq!(s.timeout_secs, src.timeout_secs);
    assert_eq!(s.continue_on_error, src.continue_on_error);
    assert_eq!(s.max_speed, src.max_speed);
}

/// 单调性：任务越多，单任务分到的并发只会更少（不会反而变多）。
///
/// 用循环而不是 `#[case]`：这是**相邻输入之间的关系**，
/// 摊成 30 行 case 反而看不出「单调」这个性质。
#[test]
fn split_for_is_monotone_in_task_count() {
    let o = DownloadOpts::default();
    let mut prev = o.split_for(0).concurrency;
    for tasks in 1..=64usize {
        let cur = o.split_for(tasks).concurrency;
        assert!(
            cur <= prev,
            "tasks={tasks} 分到 {cur} 条，比 tasks={} 的 {prev} 条还多",
            tasks - 1
        );
        assert!(
            (1..=DEFAULT_CONCURRENCY).contains(&cur),
            "tasks={tasks} 越界：{cur}"
        );
        prev = cur;
    }
    assert_eq!(prev, 1, "tasks=64 时应当钳到下限 1");
}
