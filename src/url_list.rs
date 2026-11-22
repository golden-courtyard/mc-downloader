//! `urls_file` 的解析：一行一个 URL → `(url, 文件名)`。
//!
//! **对齐 aria2 的 `-i` 列表格式**（P0 的目标：让已有的 aria2 列表文件直接能用）。
//!
//! 三类行：
//! - 空行 / `#` 开头      → 跳过，不计数
//! - 含 `://`             → 当 URL
//! - 其它（如 aria2 的缩进选项 `dir=` / `out=`）→ **跳过并计数**，最后报告
//!
//! 为什么"其它行"是跳过而不是报错：aria2 的列表文件里天然有选项行，
//! 报错会让"直接拿来用"这个目标失效。但**必须报告跳过了多少**——
//! 否则用户拼错一行（比如 `htp://`）会被静默忽略，最后少下一个文件还不知道。

use anyhow::{Result, bail};

/// 一行解析出来的结果
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub url: String,
    /// 相对 `dir` 的文件名（已 percent-decode）
    pub file_name: String,
}

#[derive(Debug, Clone)]
pub struct ParsedList {
    pub entries: Vec<Entry>,
    /// 既不是空行/注释、也不像 URL 的行数（aria2 的选项行会落在这里）
    pub skipped: usize,
}

/// 解析列表文件内容。
///
/// **坏行跳过、不连坐整批**（2026-11-22 起；此前是直接报错，一行坏 URL 会让其余合法的一条都不下）：
///
/// - 推不出文件名（URL 以 `/` 结尾，或只有 host 没有 path）⇒ 跳过并计数
/// - **两个 URL 推出同一个文件名** ⇒ 保留第一条、跳过后一条（同目录会互相覆盖）
/// - 一条有效 URL 都没有 ⇒ **这时才报错**（否则"成功但什么都没下"更难查）
use crate::version::{Job, SizeHint};

pub fn parse(content: &str) -> Result<ParsedList> {
    let mut entries: Vec<Entry> = Vec::new();
    let mut skipped = 0usize;
    let mut seen: std::collections::HashMap<String, usize> = std::collections::HashMap::new();

    for (lineno, raw) in content.lines().enumerate() {
        let lineno = lineno + 1; // 用户看到的行号从 1 开始
        let line = raw.trim();

        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if !line.contains("://") {
            // aria2 的 dir= / out= 之类
            skipped += 1;
            continue;
        }

        // 推不出名字 ⇒ **跳过这一行**，不要连坐整批：`batch_opts` 的「一个失败不拖累其余」
        // 是下载阶段的承诺，解析阶段也该一样（红队 2026-11-22 实测：一行坏 URL 会让合法那条
        // 一个字节都没下）。
        let Some(file_name) = filename_from_url(line) else {
            eprintln!(
                "[!] 第 {lineno} 行推不出文件名，已跳过：{line}\n\
                 \x20 URL 要以文件名结尾（不能以 `/` 结尾，也不能只有主机名）"
            );
            skipped += 1;
            continue;
        };

        if let Some(prev) = seen.insert(file_name.clone(), lineno) {
            // 同名会互相覆盖（后者被当成「已存在」跳过）⇒ 保留第一条、跳过后来者，并说清原因
            eprintln!(
                "[!] 第 {prev} 行和第 {lineno} 行推出同一个文件名 `{file_name}`，已跳过后一条\n\
                 \x20 同一个目录下会互相覆盖。要都下，请拆成两个任务，或用单个 `url` + `name` 分别指定"
            );
            skipped += 1;
            continue;
        }

        entries.push(Entry {
            url: line.to_string(),
            file_name,
        });
    }

    if entries.is_empty() {
        bail!(
            "列表里没有一条有效 URL（跳过了 {skipped} 行非 URL 内容）。\n\
             \x20 每行一个 URL，形如 `https://example.com/a.zip`"
        );
    }

    Ok(ParsedList { entries, skipped })
}

/// 从 URL 推文件名。
///
/// 去掉 `#fragment` 和 `?query` 后取最后一段，再 percent-decode。
/// `%20` 这类不解码的话，用户会在磁盘上看到 `a%20b.zip` 并以为下错了。
///
/// **`fetch` 也用它**（issue #1）：单文件下载的默认落地名不该是常量 `out.bin` ——
/// 同目录连下两个文件会互相覆盖，而且进度行里看不出这次下的是什么。
/// 文件名推导的规则只留这一份，`urls_file` 与 `fetch` 共用。
pub fn filename_from_url(url: &str) -> Option<String> {
    let s = url.split('#').next()?;
    let s = s.split('?').next()?;
    let after_scheme = s.split_once("://").map(|(_, r)| r)?;
    // 只有 host（`https://example.com`）⇒ 没有 path ⇒ 推不出
    let (_, path) = after_scheme.split_once('/')?;
    let last = path.rsplit('/').next()?;
    if last.is_empty() {
        return None;
    }
    // 解码**之后**才见真章：`%2e%2e%2f%2e%2e%2fpwned.bin` 解出来是 `../../pwned.bin`。
    // 校验放在解码之后、返回之前 —— 这一层是"最后一个能拦住它的地方"。
    let decoded = percent_decode(last);
    match crate::pathsafe::check_name(&decoded) {
        Ok(name) => Some(name),
        Err(_) => {
            // 不静默改名、也不静默丢弃：说清是哪一段被拒，交给调用方回落到 `out.bin`
            eprintln!("[!] URL 末段解出来不是合法文件名，已忽略：{decoded:?}");
            None
        }
    }
}

/// 最小 percent-decode（只处理 `%XX`，其余原样）。
///
/// 不引入 `urlencoding` 依赖：这里只用于**文件名末段**，输入范围可控。
/// 非法 `%XX`（如 `%zz`、截断的 `%2`）原样保留，不报错——文件名里出现 `%` 是合法的。
fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%'
            && i + 2 < b.len()
            && let (Some(h), Some(l)) = (hex_val(b[i + 1]), hex_val(b[i + 2]))
        {
            out.push(h * 16 + l);
            i += 3;
            continue;
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn hex_val(c: u8) -> Option<u8> {
    match c {
        b'0'..=b'9' => Some(c - b'0'),
        b'a'..=b'f' => Some(c - b'a' + 10),
        b'A'..=b'F' => Some(c - b'A' + 10),
        _ => None,
    }
}

/// `fetch` 的默认落地名：能推就推，推不出回落 `out.bin`。
///
/// 返回 `(名字, 是否回落)` —— 第二个值是给调用方**在输出里说明**用的：
/// 回落本身不是错（URL 可能真的以 `/` 结尾），但**静默**回落会让用户
/// 在磁盘上看到 `out.bin` 却不知道为什么（issue #1 的另一半）。
///
/// 放这里而不是 `commands/fetch.rs`：那是 bin 侧，`cargo test --lib` 测不到。
pub fn name_or_out_bin(url: &str) -> (String, bool) {
    match filename_from_url(url) {
        Some(n) => (n, false),
        None => ("out.bin".to_string(), true),
    }
}

/// 把一个 URL（+ 可选覆盖项）变成 [`Job`]。**`fetch` 与 `run` 共用的唯一推导口**。
///
/// 为什么必须收进 lib（审查 A2）：`commands/run.rs` 曾自己写
/// `url.rsplit('/').next()`，于是同一个 URL 在两条路上的落盘名不一样 ——
/// 它不剥 `?query`、不做 percent-decode（`f.zip?se=2026&sig=…` 会整段进文件名），
/// 也拿不到"推不出 ⇒ 回落 out.bin"这个信号。
///
/// 名字**不在这里做安全校验**：`--name` 允许带子目录（`sub/x.bin`），落点校验由
/// `pathsafe::under_root` 在碰文件系统之前统一做（见 `commands/fetch.rs` 与 `download::one`）。
pub fn job_from_url(
    url: &str,
    name: Option<&str>,
    size: SizeHint,
    mirrors: Vec<String>,
) -> (Job, bool) {
    let (path, fell_back) = match name {
        Some(n) => (n.to_string(), false),
        None => name_or_out_bin(url),
    };
    let job = Job {
        url: url.to_string(),
        path,
        sha1: None,
        size,
        mirrors,
    };
    (job, fell_back)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_urls() {
        let p = parse("https://a.com/x/1.zip\nhttps://b.com/2.tar.gz\n").unwrap();
        assert_eq!(p.entries.len(), 2);
        assert_eq!(p.entries[0].file_name, "1.zip");
        assert_eq!(p.entries[1].file_name, "2.tar.gz");
        assert_eq!(p.skipped, 0);
    }

    #[test]
    fn strips_query_and_fragment() {
        // GitHub 的签名 URL 就长这样
        let p = parse("https://a.com/f.zip?se=2026&sig=abc#frag").unwrap();
        assert_eq!(p.entries[0].file_name, "f.zip");
    }

    #[test]
    fn percent_decodes_the_name() {
        let p = parse("https://a.com/a%20b.zip").unwrap();
        assert_eq!(
            p.entries[0].file_name, "a b.zip",
            "%20 要解码，否则用户以为下错了"
        );
    }

    #[test]
    fn skips_comments_blank_and_aria2_options() {
        let content = "\
# 注释
https://a.com/1.zip

  dir=/downloads
  out=renamed.zip
https://a.com/2.zip
";
        let p = parse(content).unwrap();
        assert_eq!(p.entries.len(), 2, "只有两条真 URL");
        assert_eq!(p.skipped, 2, "aria2 的两行选项要被计数并报告");
    }

    #[test]
    fn trailing_slash_line_is_skipped_but_batch_continues() {
        // 以 `/` 结尾的行推不出名字 ⇒ 跳过它，别把同批合法的那条也拖死
        let p = parse("https://a.com/dir/\nhttps://a.com/ok.zip\n").unwrap();
        assert_eq!(p.entries.len(), 1, "合法那条必须留着");
        assert_eq!(p.entries[0].file_name, "ok.zip");
        assert_eq!(p.skipped, 1);
        // 只有这一条时才是错误（见 `no_valid_url_is_still_an_error`）
        assert!(parse("https://a.com/dir/\n").is_err());
    }

    #[test]
    fn host_only_has_no_filename() {
        assert!(parse("https://example.com\n").is_err());
    }

    #[test]
    fn empty_or_all_skipped_is_an_error() {
        assert!(parse("").is_err());
        assert!(parse("# 只有注释\n\n").is_err(), "没有 URL 要报错");
        let e = parse("dir=/x\nout=y\n").unwrap_err().to_string();
        assert!(e.contains("跳过了 2 行"), "要报告跳过数: {e}");
    }

    #[test]
    fn bad_percent_sequence_is_kept_verbatim() {
        // %zz 不是合法转义 ⇒ 原样保留，不报错（文件名里出现 % 是合法的）
        let p = parse("https://a.com/a%zzb.zip").unwrap();
        assert_eq!(p.entries[0].file_name, "a%zzb.zip");
    }

    // ── name_or_out_bin：fetch 的默认落地名（issue #1） ──

    #[test]
    fn name_from_url_is_used_by_fetch() {
        let (n, fell_back) = name_or_out_bin("https://a.com/d/2015.260564.History_text.pdf");
        assert_eq!(n, "2015.260564.History_text.pdf");
        assert!(!fell_back);
    }

    #[test]
    fn name_drops_query_even_when_query_contains_slashes() {
        let (n, _) = name_or_out_bin("https://a.com/f.zip?path=/x/y&sig=1");
        assert_eq!(n, "f.zip", "query 里的斜杠不能影响末段");
    }

    /// 一行坏 URL 不许连坐整批（红队 2026-11-22 实测：此前会整批 `bail`，合法那条一个字节都不下）。
    #[test]
    fn bad_line_is_skipped_not_fatal() {
        let list = "https://h/a/..%2f..%2fpwned.bin\nhttps://h/ok.bin\n";
        let p = parse(list).unwrap();
        assert_eq!(p.entries.len(), 1, "合法那条必须留着");
        assert_eq!(p.entries[0].file_name, "ok.bin");
        assert_eq!(p.skipped, 1, "坏行要计数");
    }

    /// 重名也走「保留第一条 + 跳过后一条」，而不是炸掉整批。
    #[test]
    fn duplicate_name_keeps_first_and_skips_rest() {
        let list = "https://h/a/x.bin\nhttps://h/b/x.bin\n";
        let p = parse(list).unwrap();
        assert_eq!(p.entries.len(), 1);
        assert_eq!(p.entries[0].url, "https://h/a/x.bin");
        assert_eq!(p.skipped, 1);
    }

    /// 但"一条有效的都没有"仍然是错误 —— 否则会变成"成功但什么都没下"。
    #[test]
    fn no_valid_url_is_still_an_error() {
        assert!(parse("https://h/dir/\n").is_err());
        assert!(parse("# 只有注释\n").is_err());
    }

    #[test]
    fn unnamed_url_falls_back_to_out_bin_and_says_so() {
        // 两个典型：以 `/` 结尾、只有主机名
        for url in ["https://a.com/dir/", "https://example.com"] {
            let (n, fell_back) = name_or_out_bin(url);
            assert_eq!(n, "out.bin", "{url}");
            assert!(fell_back, "{url} 必须报告回落，不能静默");
        }
    }

    /// 攻击者可控的落点名：末段 percent-decode 之后是 `../..`，必须推不出名字。
    ///
    /// 这条锁的是"解码之后才校验"的顺序——先校验再解码等于没校验。
    /// 注意只有**末段**参与取名：`http://h/a/%2e%2e/%2e%2e/x.bin` 的落点名是 `x.bin`（合法），
    /// 中间段的 `..` 由 HTTP 服务器自己解析，不是我们的文件名问题。
    #[test]
    fn traversal_in_last_segment_is_rejected() {
        for url in [
            "http://h/loot/%2e%2e%2f%2e%2e%2fpwned.bin",
            "http://h/x/%2e%2e%5c%2e%2e%5cpwned.bin",
            "http://h/a/..%2f..%2fpwned.bin",
            "http://h/a/%2e%2e%2fx.bin",
        ] {
            assert!(filename_from_url(url).is_none(), "应拒: {url}");
        }
        // 合法名不受影响（含中文与空格）
        assert_eq!(filename_from_url("http://h/a/b.bin").unwrap(), "b.bin");
        assert_eq!(
            filename_from_url("http://h/a/%E4%B8%AD%20%E6%96%87.jar").unwrap(),
            "中 文.jar"
        );
        // 中间段的 `..` 与我们无关：HTTP 服务器解析路径，落点名只看末段
        assert_eq!(
            filename_from_url("http://h/a/%2e%2e/%2e%2e/x.bin").unwrap(),
            "x.bin"
        );
    }
}

#[cfg(test)]
mod job_from_url_tests {
    use super::*;

    /// A2 的判据：`fetch` 与 `run` 共用**同一份**推导。这三条正是当初两边不一致的地方。
    #[test]
    fn job_from_url_is_the_single_derivation() {
        // ① 剥 `?query` —— 旧 `run` 实现会把 `?se=2026&sig=abc` 整段带进文件名
        let (j, fell_back) = job_from_url(
            "https://h/f.zip?se=2026&sig=abc",
            None,
            SizeHint::Unknown,
            vec![],
        );
        assert_eq!(j.path, "f.zip");
        assert!(!fell_back);

        // ② percent-decode —— 旧实现留在磁盘上的是 `a%20b.zip`
        let (j, _) = job_from_url("https://h/a%20b.zip", None, SizeHint::Unknown, vec![]);
        assert_eq!(j.path, "a b.zip");

        // ③ 推不出 ⇒ 回落 `out.bin`，而且**要能被调用方说明**（旧实现给 `download.bin` 且静默）
        let (j, fell_back) = job_from_url("https://h/dir/", None, SizeHint::Unknown, vec![]);
        assert_eq!(j.path, "out.bin");
        assert!(
            fell_back,
            "回落必须让调用方能说明，否则用户不知道为什么叫 out.bin"
        );

        // ④ 用户给名字时优先（允许子目录），其余字段原样带过去
        let (j, fell_back) = job_from_url(
            "https://h/dir/",
            Some("sub/x.bin"),
            SizeHint::Hint(9),
            vec!["https://mirror/x.bin".to_string()],
        );
        assert_eq!(j.path, "sub/x.bin");
        assert!(!fell_back);
        assert_eq!(j.size.bytes(), Some(9));
        assert_eq!(
            j.size.exact(),
            None,
            "用户给的是提示，不该进 Content-Range 比对"
        );
        assert_eq!(j.mirrors.len(), 1);
    }
}
