//! 版本一致性闸门（对标 dsh 的 `scripts/verify-version-sync.ts`）。
//!
//! 要防的是这样一种合并事故：A 分支把版本从 `0.6.0` 抬到 `0.7.0`，B 分支在旧版本上加了新章节，
//! 合并后**两者对不上** —— 仓库里读到的版本、CHANGELOG 的标题、打的标签是三个数。
//!
//! 三条判据：
//!
//! 1. `Cargo.toml` 的 `version` 必须是合法 semver（含可选的预发布段）；
//! 2. `CHANGELOG.md` 必须有一节 `## [<version>]`（或 `## [Unreleased]` 并且版本节存在）；
//! 3. **在 tag 上跑时**，标签名必须等于 `mc-v<version>`（`GITHUB_REF_NAME` 由 CI 提供）。

use std::path::PathBuf;

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn cargo_version() -> String {
    let text = std::fs::read_to_string(repo_root().join("Cargo.toml")).expect("读 Cargo.toml");
    let mut in_package = false;
    for line in text.lines() {
        let t = line.trim();
        if t.starts_with('[') {
            in_package = t == "[package]";
            continue;
        }
        if in_package && let Some(rest) = t.strip_prefix("version") {
            let rest = rest.trim_start();
            if let Some(v) = rest.strip_prefix('=') {
                return v.trim().trim_matches('"').to_string();
            }
        }
    }
    panic!("Cargo.toml 里找不到 [package].version");
}

/// 只做**形状**校验：`x.y.z` 或 `x.y.z-<预发布>`，每段都是数字。
fn is_semverish(v: &str) -> bool {
    let (core, pre) = match v.split_once('-') {
        Some((c, p)) => (c, Some(p)),
        None => (v, None),
    };
    let nums: Vec<&str> = core.split('.').collect();
    if nums.len() != 3
        || !nums
            .iter()
            .all(|n| !n.is_empty() && n.chars().all(|c| c.is_ascii_digit()))
    {
        return false;
    }
    if let Some(p) = pre {
        return !p.is_empty()
            && p.chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '-');
    }
    true
}

fn changelog_sections() -> Vec<String> {
    let text = std::fs::read_to_string(repo_root().join("CHANGELOG.md")).expect("读 CHANGELOG.md");
    text.lines()
        .filter_map(|l| {
            let t = l.trim();
            let rest = t.strip_prefix("## [")?;
            let end = rest.find(']')?;
            Some(rest[..end].to_string())
        })
        .collect()
}

#[test]
fn crate_version_is_well_formed_semver() {
    let v = cargo_version();
    assert!(
        is_semverish(&v),
        "Cargo.toml 的 version 不是合法 semver：`{v}`"
    );
}

#[test]
fn changelog_has_a_section_for_the_crate_version() {
    let v = cargo_version();
    let sections = changelog_sections();
    assert!(
        !sections.is_empty(),
        "CHANGELOG.md 里没有任何 `## [x.y.z]` 节"
    );
    assert!(
        sections.iter().any(|s| s == &v),
        "CHANGELOG.md 里找不到当前版本 `{v}` 的节；现有：{sections:?}"
    );
}

/// 在 tag 上跑时，标签必须是 `mc-v<version>`。
///
/// 本地跑（没有 `GITHUB_REF_NAME`）自动跳过 —— 版本闸门不该让日常 `cargo test` 变红。
#[test]
fn tag_matches_crate_version_when_building_a_tag() {
    let Ok(ref_name) = std::env::var("GITHUB_REF_NAME") else {
        eprintln!("[skip] 不在 CI 的 tag 构建里（没有 GITHUB_REF_NAME）");
        return;
    };
    let Ok(ref_type) = std::env::var("GITHUB_REF_TYPE") else {
        eprintln!("[skip] 没有 GITHUB_REF_TYPE");
        return;
    };
    if ref_type != "tag" {
        eprintln!("[skip] 本次构建的 ref 不是 tag（{ref_type}: {ref_name}）");
        return;
    }
    let v = cargo_version();
    assert_eq!(
        ref_name,
        format!("mc-v{v}"),
        "标签与 Cargo.toml 版本不一致：标签 `{ref_name}`，版本 `{v}`"
    );
}

#[test]
fn semver_shape_rejects_obvious_junk() {
    for good in ["0.6.0", "1.0.0", "0.7.0-alpha.1", "0.7.0-rc.2", "12.34.56"] {
        assert!(is_semverish(good), "应当接受 `{good}`");
    }
    for bad in ["0.6", "0.6.0.1", "v0.6.0", "0.6.0-", "a.b.c", ""] {
        assert!(!is_semverish(bad), "应当拒绝 `{bad}`");
    }
}
