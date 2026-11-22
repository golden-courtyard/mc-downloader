//! 回归锁：CLI 表面（子命令名 + 全局参数名）是对外契约。
//!
//! 改名一个全局参数，用户的脚本直接坏，而此前**没有任何测试碰过 CLI 表面**。
//! `tests/linux_semantics.rs` 虽然在用 `CARGO_BIN_EXE_*` 起子进程，但整个文件带 `#![cfg(target_os = "linux")]`，
//! 所以那条路径在 Windows 上根本不编译 —— 这个文件刻意**不带任何 cfg**。
//!
//! 判据只查"名字还在"，不查文案：这是契约，不是快照（文案会改，改名不该悄悄发生）。

use std::process::Command;

fn help() -> String {
    let out = Command::new(env!("CARGO_BIN_EXE_mc-downloader"))
        .arg("--help")
        .output()
        .expect("跑 --help 失败");
    assert!(out.status.success());
    String::from_utf8_lossy(&out.stdout).into_owned()
}

#[test]
fn every_subcommand_is_still_reachable() {
    let h = help();
    for cmd in ["vanilla", "run", "fetch", "probe", "pack"] {
        assert!(h.contains(cmd), "子命令 `{cmd}` 不在 --help 里了：\n{h}");
    }
}

#[test]
fn every_global_flag_is_still_documented() {
    let h = help();
    for flag in [
        "--proxy",
        "--retry",
        "--concurrency",
        "--piece-conns",
        "--adaptive",
        "--timeout",
        "--max-speed",
        "--config",
        "--print-config",
        "--no-motw",
    ] {
        assert!(
            h.contains(flag),
            "全局参数 `{flag}` 不在 --help 里了：\n{h}"
        );
    }
}

/// `--print-config` 必须能**单独**跑（不需要子命令），并且每个键都出现在表里。
///
/// 这不是快照：只断言"键在"，不断言值（值取决于机器与环境）。
#[test]
fn print_config_works_without_a_subcommand_and_lists_every_key() {
    let out = Command::new(env!("CARGO_BIN_EXE_mc-downloader"))
        .arg("--print-config")
        .output()
        .expect("跑 --print-config 失败");
    assert!(out.status.success(), "--print-config 不该要求子命令");
    let s = String::from_utf8_lossy(&out.stdout);
    for key in [
        "proxy",
        "retries",
        "concurrency",
        "piece_conns",
        "adaptive",
        "timeout_secs",
        "max_speed",
        "motw",
    ] {
        assert!(s.contains(key), "配置表里缺 `{key}`：\n{s}");
    }
    // 来源列必须存在（cli / config / default 三态之一）
    assert!(
        s.contains("default") || s.contains("config") || s.contains("cli"),
        "来源列缺失：\n{s}"
    );
}
