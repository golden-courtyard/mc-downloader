//! 路径安全：把来源不可信的路径挡在落盘之前。
//!
//! 三处来源都不可信：
//!
//! 1. **URL 末段** —— `fetch` / `urls_file` 推出来的默认文件名（percent-decode 之后才见真章）
//! 2. **整合包清单** —— Modrinth 的 `files[].path`、CurseForge 的 `file_name`
//! 3. **zip 条目名** —— 整合包的 override 拷贝
//!
//! 为什么值得一个单独模块：`PathBuf::join` 碰到**绝对路径会整体替换**
//! （`root.join("C:\\x")` 得到 `C:\x`）、UNC 同理，所以"看上去是相对路径"并不等于
//! "落点还在 root 之下"。判定集中一处，改一次全仓生效，也省得每个调用点各写一遍半对不对的检查。
//!
//! 政策（与 `.agents/notes/implemented/bug-fix/2026-11-22-path-safety.md` 一致）：
//! **下载目标是拒绝整条任务**（大声失败），**zip 条目是跳过并计数**（一个坏条目不该毁掉整包安装）。

use anyhow::{Context, Result, bail};
use std::path::{Component, Path, PathBuf};

/// Windows 设备保留名。比较时不区分大小写，且**带扩展名也算**（`con.txt` 仍是设备）。
const RESERVED: &[&str] = &[
    "con", "prn", "aux", "nul", "com1", "com2", "com3", "com4", "com5", "com6", "com7", "com8",
    "com9", "lpt1", "lpt2", "lpt3", "lpt4", "lpt5", "lpt6", "lpt7", "lpt8", "lpt9",
];

/// 校验**单段文件名**（URL 末段、CurseForge 的 `file_name`）。
///
/// 单段的名字里出现分隔符本身就是可疑的（URL 的末段不该带 `/`），所以这里比
/// [`check_rel`] 更严：连子目录都不接受。
pub fn check_name(raw: &str) -> Result<String> {
    if raw.contains('/') || raw.contains('\\') {
        bail!("文件名里不许带路径分隔符: {raw}");
    }
    check_component(raw).with_context(|| format!("文件名不安全: {raw}"))?;
    Ok(raw.to_string())
}

/// 校验**相对路径**（允许 `mods/x.jar` 这类子目录），返回规范化后的相对路径。
///
/// 规范化只做两件事：去掉空组件（`a//b`）与 `.` 组件。**绝不**尝试"修好"`..`
/// —— 那是穿越，必须拒绝而不是纠正。
pub fn check_rel(raw: &str) -> Result<PathBuf> {
    if raw.trim().is_empty() {
        bail!("路径为空");
    }
    if raw.starts_with('/') || raw.starts_with('\\') {
        bail!("路径不安全（绝对路径）: {raw}");
    }
    // 盘符式：`C:` / `C:\` / `C:foo`（Windows 上 `C:foo` 是"当前盘相对"，同样危险）
    let b = raw.as_bytes();
    if b.len() >= 2 && b[0].is_ascii_alphabetic() && b[1] == b':' {
        bail!("路径不安全（带盘符）: {raw}");
    }
    let mut out = PathBuf::new();
    for part in raw.split('/') {
        if part.is_empty() || part == "." {
            continue; // 空组件与 `.` 是纯噪声，规范化掉；`..` 在下面被拒
        }
        check_component(part).with_context(|| format!("路径不安全: {raw}"))?;
        out.push(part);
    }
    if out.as_os_str().is_empty() {
        bail!("路径规范化后为空: {raw}");
    }
    Ok(out)
}

/// 规范化 `rel` 并确认最终落点仍在 `root` 之下，返回可直接落盘的路径。
///
/// 两层判定：
/// 1. **词法**：逐组件校验（`rel` 里出现绝对/前缀/父级组件就拒），再比前缀 ——
///    这一层不依赖文件系统，`root` 还不存在时也成立。
/// 2. **符号链接**：`root` 已存在时把两边的"最长可解析前缀"canonicalize 后再比一次，
///    防止 root 下面某一层是指向别处的软链。`root` 不存在时只能做第 1 层，
///    错误信息里会写明这一点（不假装做过第二层）。
pub fn under_root(root: &Path, rel: &Path) -> Result<PathBuf> {
    let mut norm = PathBuf::new();
    for c in rel.components() {
        match c {
            Component::Normal(s) => {
                let s = s
                    .to_str()
                    .with_context(|| format!("路径不是合法 UTF-8: {}", rel.display()))?;
                check_component(s)?;
                norm.push(s);
            }
            Component::CurDir => {}
            _ => bail!("目标路径含绝对 / 父级 / 前缀组件，拒绝: {}", rel.display()),
        }
    }
    if norm.as_os_str().is_empty() {
        bail!("目标路径规范化后为空: {}", rel.display());
    }

    let dst = root.join(&norm);

    // 第 1 层：词法（absolutize 只补 cwd，不碰文件系统）
    let root_abs = absolutize(root);
    let dst_abs = absolutize(&dst);
    if !starts_with_path(&dst_abs, &root_abs) {
        bail!(
            "目标路径逃出根目录: root={} 目标={}",
            root.display(),
            dst.display()
        );
    }

    // 第 2 层：符号链接（只在 root 真能 canonicalize 时做，否则如实说明做不到）
    match root.canonicalize() {
        Ok(canon_root) => {
            let canon_dst = canon_prefix(&dst);
            if !starts_with_path(&canon_dst, &canon_root) {
                bail!(
                    "目标路径经符号链接后逃出根目录: root={} 解析为 {}，目标解析为 {}",
                    root.display(),
                    canon_root.display(),
                    canon_dst.display()
                );
            }
        }
        Err(_) => {
            // root 还不存在：第 1 层已成立。这里**不**静默通过一个没做过的检查。
        }
    }
    Ok(dst)
}

/// 单个路径组件的判定。两个入口共用，规则只有一份。
fn check_component(c: &str) -> Result<()> {
    if c.is_empty() {
        bail!("路径里有空组件");
    }
    if c == "." || c == ".." {
        bail!("路径里有 `{c}` 组件（穿越）");
    }
    // 控制字符（含 NUL）：终端里看不见，落盘后能骗过 `ls`
    if c.chars().any(|ch| ch.is_control()) {
        bail!("路径里有控制字符");
    }
    // 反斜杠：Windows 上是分隔符，Linux 上是普通字符 —— 两边语义不一致，直接拒
    if c.contains('\\') {
        bail!("路径里有反斜杠");
    }
    if cfg!(windows) {
        // NTFS 备用数据流：`a.txt:hidden` 会写出一个看不见的流
        if c.contains(':') {
            bail!("Windows 上文件名不许含 `:`（NTFS 备用数据流）");
        }
        // 尾部点/空格：Win32 会静默去掉，于是 "a. " 与 "a" 指向同一文件
        if c.ends_with('.') || c.ends_with(' ') {
            bail!("Windows 上文件名不许以点或空格结尾");
        }
        // 设备保留名（带扩展名也算）
        let stem = c.split('.').next().unwrap_or(c).trim_end_matches(' ');
        if RESERVED.iter().any(|r| stem.eq_ignore_ascii_case(r)) {
            bail!("Windows 设备保留名: {c}");
        }
    }
    Ok(())
}

/// 相对路径补上 cwd（**只做字符串拼接，不碰文件系统**）。
fn absolutize(p: &Path) -> PathBuf {
    if p.is_absolute() {
        return p.to_path_buf();
    }
    match std::env::current_dir() {
        Ok(cwd) => cwd.join(p),
        Err(_) => p.to_path_buf(),
    }
}

/// `p` 是否落在 `base` 之下。Windows 上大小写不敏感（`C:\Out` 与 `c:\out` 是同一处）。
fn starts_with_path(p: &Path, base: &Path) -> bool {
    if !cfg!(windows) {
        return p.starts_with(base);
    }
    let norm = |x: &Path| {
        x.to_string_lossy()
            .to_ascii_lowercase()
            .replace('/', "\\")
            .trim_end_matches('\\')
            .to_string()
    };
    let b = norm(base);
    let q = norm(p);
    if !q.starts_with(&b) {
        return false;
    }
    // 组件边界：相等，或下一个字符正是分隔符（否则 `C:\out2` 会被当成 `C:\out` 的子路径）
    q.len() == b.len() || q.as_bytes().get(b.len()) == Some(&b'\\')
}

/// 把"最长的可解析前缀"canonicalize，再把剩下的组件接回去。
///
/// 用途：目标文件大多还不存在，不能直接 `canonicalize`；但它的祖先目录通常存在，
/// 软链就藏在那几层里。
fn canon_prefix(p: &Path) -> PathBuf {
    let mut cur = absolutize(p);
    let mut tail: Vec<std::ffi::OsString> = Vec::new();
    loop {
        if let Ok(c) = cur.canonicalize() {
            let mut out = c;
            for t in tail.iter().rev() {
                out.push(t);
            }
            return out;
        }
        let Some(name) = cur.file_name().map(|s| s.to_os_string()) else {
            return absolutize(p);
        };
        let Some(parent) = cur.parent() else {
            return absolutize(p);
        };
        tail.push(name);
        cur = parent.to_path_buf();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_plain_name() {
        assert_eq!(check_name("mod.jar").unwrap(), "mod.jar");
        assert_eq!(
            check_name("中文名 含空格.jar").unwrap(),
            "中文名 含空格.jar"
        );
    }

    #[test]
    fn accepts_relative_subdir() {
        let p = check_rel("mods/a/b.jar").unwrap();
        assert_eq!(p, PathBuf::from("mods/a/b.jar"));
        // 重复分隔符与 `.` 组件可以被规范化掉
        assert_eq!(
            check_rel("mods//a/./b.jar").unwrap(),
            PathBuf::from("mods/a/b.jar")
        );
    }

    #[test]
    fn rejects_traversal_in_both_separators() {
        for bad in ["../x", "..\\x", "a/../../x", "a/..", ".."] {
            assert!(check_rel(bad).is_err(), "应拒绝: {bad}");
        }
        assert!(check_name("..").is_err());
        assert!(check_name(".").is_err());
    }

    /// 本模块**不做 percent 解码**：解码是调用方的事，解开之后必须再过一次。
    ///
    /// 所以字面量 `%2e%2e` 在这里是合法的**普通文件名**（落盘会得到一个真叫 `%2e%2e` 的目录），
    /// 而 `..%2f..` 解码成 `../..` 之后必须被这一层拒 —— 两条都要钉住，
    /// 否则以后有人把解码挪到本模块前面，检查就形同虚设。
    #[test]
    fn percent_encoded_dots_are_only_dangerous_after_decoding() {
        assert!(check_rel("a/%2e%2e/x").is_ok(), "未解码的字面量不是穿越");
        assert!(check_rel("a/../x").is_err(), "解码后是穿越就必须拒");
    }

    #[test]
    fn rejects_absolute_and_drive_and_unc() {
        for bad in ["/etc/passwd", "\\windows\\x", "C:/x", "C:x", "c:\\x"] {
            assert!(check_rel(bad).is_err(), "应拒绝: {bad}");
        }
        // 盘符是 **Windows 专属**规则：Linux 上 `C:x` 只是一个合法文件名，
        // 拿 Windows 的规则去拒它属于张冠李戴。两个平台各自的判据都要钉住。
        if cfg!(windows) {
            assert!(check_name("C:x").is_err(), "Windows 上盘符名危险");
        } else {
            assert!(check_name("C:x").is_ok(), "Linux 上它是普通文件名");
        }
    }

    #[test]
    fn rejects_control_chars_and_backslash_and_separators_in_name() {
        assert!(check_name("a\u{0}b").is_err());
        assert!(check_name("a\nb").is_err());
        assert!(check_name("a\\b").is_err(), "文件名里不许有反斜杠");
        assert!(check_name("a/b").is_err(), "文件名里不许有正斜杠");
    }

    #[cfg(windows)]
    #[test]
    fn windows_only_rules() {
        assert!(check_name("a.txt:hidden").is_err(), "NTFS ADS");
        assert!(check_name("trailing.").is_err(), "尾部点");
        assert!(check_name("trailing ").is_err(), "尾部空格");
        for n in ["CON", "con.txt", "Nul", "COM1", "lpt9.log"] {
            assert!(check_name(n).is_err(), "保留名应拒: {n}");
        }
        assert!(check_name("console.txt").is_ok(), "console 不是保留名");
    }

    #[test]
    fn under_root_joins_and_normalizes() {
        let root = std::env::temp_dir().join("mcd-pathsafe-test");
        let dst = under_root(&root, Path::new("mods/a.jar")).unwrap();
        assert!(dst.ends_with("mods/a.jar"));
        // root 不存在也要成立（词法判定），不能因为"目录还没建"就报错
        assert!(!root.exists());
    }

    #[test]
    fn under_root_rejects_absolute_rel() {
        let root = std::env::temp_dir().join("mcd-pathsafe-test2");
        let abs = if cfg!(windows) {
            Path::new("C:\\Windows\\Temp\\evil.bin")
        } else {
            Path::new("/tmp/evil.bin")
        };
        let e = under_root(&root, abs).unwrap_err();
        assert!(
            format!("{e:#}").contains("绝对"),
            "错误应说明是绝对路径: {e:#}"
        );
    }

    #[test]
    fn prefix_check_needs_component_boundary() {
        // `out2` 不是 `out` 的子路径 —— 这条必须在两个平台都成立，所以路径按平台取。
        // （CI 在 Linux 上跑：早先这里写死了 Windows 路径，本地绿、CI 红。）
        let (base, child, sibling) = if cfg!(windows) {
            (
                Path::new("D:\\tmp\\out"),
                Path::new("D:\\tmp\\out\\x"),
                Path::new("D:\\tmp\\out2\\x"),
            )
        } else {
            (
                Path::new("/tmp/out"),
                Path::new("/tmp/out/x"),
                Path::new("/tmp/out2/x"),
            )
        };
        assert!(!starts_with_path(sibling, base), "out2 不是 out 的子路径");
        assert!(starts_with_path(child, base), "out/x 应在 out 之下");
    }
}
