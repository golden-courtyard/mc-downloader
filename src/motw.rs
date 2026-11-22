//! Mark-of-the-Web（`Zone.Identifier`）：把下载来的文件标记成"来自互联网"。
//!
//! 为什么需要：浏览器下载会给文件写入一个 `Zone.Identifier` 备用数据流，之后 Windows
//! 才会对它跑 SmartScreen / 受保护视图 / Office 的信任判断。我们是**自己用 fs 写字节**，
//! 文件因此被当成"本机生成"的 —— 用户双击下载来的 exe 时**不会**看到任何警告。
//! 也就是说，这道系统防线是我们主动丢掉的，不是它不存在。
//!
//! 只标**可执行/可运行**的扩展名（见 [`should_mark`]）：一个整合包动辄几千个 asset，
//! 给每个都写一个 ADS 既慢又没意义。
//!
//! 非 Windows 上 [`mark`] 是 no-op（这个机制是 Windows 专有的）。

use std::io::Write;
use std::path::Path;

/// 会被标记的扩展名（小写比较）。
const MARK_EXTS: &[&str] = &[
    "exe", "dll", "msi", "jar", "bat", "cmd", "ps1", "scr", "vbs", "com", "cpl", "hta",
];

/// 这个文件该不该打 MotW。按扩展名判断，大小写不敏感。
pub fn should_mark(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| MARK_EXTS.iter().any(|m| m.eq_ignore_ascii_case(e)))
}

/// 按需打标记；失败只告警，**不让下载失败**（标记是加固，不是下载的一部分）。
pub fn mark_if_enabled(path: &Path, source_url: &str, enabled: bool) {
    if !enabled || !should_mark(path) {
        return;
    }
    match mark(path, source_url) {
        // 一个整合包可能有几百个 `.jar`，默认静默；诊断时用 MC_TUNE_LOG 打开
        Ok(()) => {
            if std::env::var_os("MC_TUNE_LOG").is_some() {
                eprintln!("[motw] 已标记为互联网来源: {}", path.display());
            }
        }
        Err(e) => eprintln!(
            "[!] 打 MotW 标记失败（不影响文件）: {}: {e}",
            path.display()
        ),
    }
}

/// 写入 `Zone.Identifier` 备用数据流。
///
/// Windows 上 ADS 就是"路径:流名"，用普通 `OpenOptions` 就能写；不需要任何 Win32 crate。
pub fn mark(path: &Path, source_url: &str) -> std::io::Result<()> {
    #[cfg(not(windows))]
    {
        let _ = (path, source_url);
        Ok(())
    }
    #[cfg(windows)]
    {
        use std::ffi::OsString;
        let mut ads: OsString = path.as_os_str().to_os_string();
        ads.push(":Zone.Identifier");
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .open(Path::new(&ads))?;
        // 行尾用 CRLF：这是 Windows 侧读它的程序（Explorer / SmartScreen）的既有格式
        write!(
            f,
            "[ZoneTransfer]\r\nZoneId=3\r\nReferrerUrl={source_url}\r\nHostUrl={source_url}\r\n"
        )
    }
}

/// 读回 `Zone.Identifier`（测试与诊断用）。
pub fn read_zone(path: &Path) -> std::io::Result<String> {
    #[cfg(not(windows))]
    {
        let _ = path;
        Err(std::io::Error::other("只有 Windows 有 Zone.Identifier"))
    }
    #[cfg(windows)]
    {
        use std::ffi::OsString;
        let mut ads: OsString = path.as_os_str().to_os_string();
        ads.push(":Zone.Identifier");
        std::fs::read_to_string(Path::new(&ads))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_executable_like_extensions_are_marked() {
        for p in ["a.exe", "b.DLL", "c.jar", "d.msi", "sub/e.ps1"] {
            assert!(should_mark(Path::new(p)), "应标记: {p}");
        }
        for p in ["a.bin", "b.zip", "c.mrpack", "d.txt", "noext"] {
            assert!(!should_mark(Path::new(p)), "不该标记: {p}");
        }
    }

    #[cfg(windows)]
    #[test]
    fn mark_writes_zone_identifier_stream() {
        let p = std::env::temp_dir().join(format!("mcd-motw-{}.exe", std::process::id()));
        std::fs::write(&p, b"x").unwrap();
        mark(&p, "http://example.com/a.exe").unwrap();
        let z = read_zone(&p).unwrap();
        assert!(z.contains("ZoneId=3"), "{z}");
        assert!(z.contains("ReferrerUrl=http://example.com/a.exe"), "{z}");
        // 文件本体不该被改动
        assert_eq!(std::fs::read(&p).unwrap(), b"x");
        let _ = std::fs::remove_file(&p);
    }

    #[cfg(windows)]
    #[test]
    fn mark_failure_does_not_panic_and_disable_flag_wins() {
        // 目录不存在 ⇒ mark 失败，但 mark_if_enabled 只告警
        let missing = std::env::temp_dir().join("mcd-motw-nope/never.exe");
        mark_if_enabled(&missing, "http://x/", true);
        mark_if_enabled(&missing, "http://x/", false);
    }
}
