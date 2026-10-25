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
/// 报错条件（都是"不许猜"的情形）：
/// - 推不出文件名（URL 以 `/` 结尾，或只有 host 没有 path）
/// - **两个 URL 推出同一个文件名** —— 否则后者会被当成"已存在"跳过，静默丢一个
/// - 一条有效 URL 都没有
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

        let Some(file_name) = filename_from_url(line) else {
            bail!(
                "第 {lineno} 行推不出文件名：{line}\n\
                 \x20 URL 要以文件名结尾（不能以 `/` 结尾，也不能只有主机名）。\n\
                 \x20 需要自定义名字的话，请用单个 `url` + `name` 的写法"
            );
        };

        if let Some(prev) = seen.insert(file_name.clone(), lineno) {
            bail!(
                "第 {prev} 行和第 {lineno} 行推出同一个文件名 `{file_name}`。\n\
                 \x20 同一个目录下会互相覆盖（后者被当成“已存在”跳过），\n\
                 \x20 所以这里直接报错。请拆成两个任务，或用单个 `url` + `name` 分别指定"
            );
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
fn filename_from_url(url: &str) -> Option<String> {
    let s = url.split('#').next()?;
    let s = s.split('?').next()?;
    let after_scheme = s.split_once("://").map(|(_, r)| r)?;
    // 只有 host（`https://example.com`）⇒ 没有 path ⇒ 推不出
    let (_, path) = after_scheme.split_once('/')?;
    let last = path.rsplit('/').next()?;
    if last.is_empty() {
        return None;
    }
    Some(percent_decode(last))
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
    fn trailing_slash_has_no_filename() {
        let e = parse("https://a.com/dir/\n").unwrap_err().to_string();
        assert!(e.contains("推不出文件名"), "{e}");
        assert!(e.contains("第 1 行"), "要给行号: {e}");
    }

    #[test]
    fn host_only_has_no_filename() {
        assert!(parse("https://example.com\n").is_err());
    }

    #[test]
    fn duplicate_filename_is_an_error() {
        let e = parse("https://a.com/x/1.zip\nhttps://b.com/y/1.zip\n")
            .unwrap_err()
            .to_string();
        assert!(e.contains("同一个文件名"), "{e}");
        assert!(e.contains("1.zip"), "{e}");
        assert!(
            e.contains("第 1 行") && e.contains("第 2 行"),
            "要列两个行号: {e}"
        );
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
}
