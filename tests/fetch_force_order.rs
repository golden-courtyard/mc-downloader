//! 回归锁：`fetch --force` 必须在**碰文件系统之前**校验落点。
//!
//! 红队 2026-11-22 的 F1：`fetch.rs` 曾在 `download::run` 之前就 `dir.join(name)` → `exists()` →
//! `remove_file()`，而 `pathsafe::under_root` 要到 `download::one` 才跑。于是
//! `--force -n "..\victim.txt"` 会**先删掉 `--dir` 之外的文件**，然后才报「目标路径含父级组件」。
//!
//! 那个修复当时**没有任何测试能锁住它的顺序**（`fetch.rs` 里的两条单测只测 `resolve_dst` 本身）。
//! 把顺序改回去，全仓测试照样全绿，而后果是删除 `--dir` 之外的文件 —— 所以这里用**编译出来的
//! 二进制**做端到端断言：这是唯一能覆盖 `commands::fetch` 编排顺序的办法（它住在 bin target 里）。

use std::path::{Path, PathBuf};
use std::process::Command;

/// 一个必然连不上的地址：路径校验发生在任何网络动作之前，所以"连不上"不会掩盖我们要验的东西。
const NEVER: &str = "http://127.0.0.1:1/never";

fn exe() -> &'static str {
    env!("CARGO_BIN_EXE_mc-downloader")
}

fn scratch(tag: &str) -> PathBuf {
    let p = std::env::temp_dir().join(format!("mcd-fetchorder-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&p);
    std::fs::create_dir_all(p.join("dir")).unwrap();
    p
}

fn fetch(dir: &Path, name: &str, force: bool) -> std::process::Output {
    let mut cmd = Command::new(exe());
    cmd.args(["fetch", NEVER, "--dir"])
        .arg(dir)
        .args(["-s", "4096", "-n", name]);
    if force {
        cmd.arg("--force");
    }
    cmd.output().unwrap()
}

#[test]
fn force_with_traversal_name_must_not_delete_outside_dir() {
    let root = scratch("traversal");
    let victim = root.join("victim.txt");
    std::fs::write(&victim, b"DO-NOT-DELETE").unwrap();

    let out = fetch(&root.join("dir"), r"..\victim.txt", true);
    assert!(!out.status.success(), "越界名字必须被拒绝");
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(
        err.contains("落地路径不安全"),
        "错误应指向落地路径（而不是网络）: {err}"
    );
    assert!(
        victim.exists(),
        "**victim 被删掉了** —— 校验跑在 `--force` 的删除之后了（F1 回归）"
    );
    assert_eq!(std::fs::read(&victim).unwrap(), b"DO-NOT-DELETE");

    let _ = std::fs::remove_dir_all(&root);
}

/// 反向：合法名字 + `--force` 时，旧文件**必须真的被删掉**。
///
/// 防的是"为了修 F1 把删除整个挪到下载成功之后" —— 那样下载失败时旧文件会留着，
/// 与 `--force` 的语义相反。
#[test]
fn force_with_legal_name_still_deletes_before_download() {
    let root = scratch("legal");
    let dst = root.join("dir").join("a.bin");
    std::fs::write(&dst, b"OLD").unwrap();

    let out = fetch(&root.join("dir"), "a.bin", true);
    // 下载必然失败（连不上），但删除应当已经发生
    assert!(!out.status.success(), "连不上必然失败");
    assert!(
        !dst.exists(),
        "`--force` 必须先删旧文件；现在它还在 ⇒ 删除被挪到了下载成功之后"
    );

    let _ = std::fs::remove_dir_all(&root);
}
