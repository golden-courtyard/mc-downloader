//! 从 `Content-Disposition` 里取文件名（issue #16）。
//!
//! 为什么要它：我们此前**完全不读**这个头，落地名只从 URL 末段推。而
//! `Content-Disposition: attachment; filename*=UTF-8''…` 恰恰是"服务器决定文件名"的标准手段
//! （GitHub release、部分 CDN、网盘直链都这么干）。于是同一个文件，浏览器落 `报告-v2.zip`、
//! 我们落 `download` 或 `out.bin`；**同名还会让第二次下载被当成「已存在」跳过**。
//!
//! 两条必须守住的边界：
//!
//! 1. **这个名字和 URL 末段一样不可信** —— 必须过 [`crate::pathsafe::check_name`]：
//!    `filename="../.."`、`filename="a/b"`、`filename="C:x"` 一律拒绝，回落到 URL 推导。
//! 2. **只在用户没给 `--name` 时才用它** —— 显式参数永远优先，这条不因为"服务器说得更准"而改变。
//!
//! 只支持 UTF-8（`filename*=UTF-8''…`）与纯 ASCII 的 `filename="…"`；
//! 其它 charset（`iso-8859-1'…'`）直接放弃 —— 猜编码猜错的代价是错误文件名，不如回落。

/// 从响应头值里取文件名。取不到/不合法 ⇒ `None`（调用方回落到 URL 推导）。
pub fn filename_from_content_disposition(v: &str) -> Option<String> {
    // `filename*=` 优先于 `filename=`（RFC 6266 §4.3：两者都给时取 filename*）
    if let Some(raw) = find_param(v, "filename*") {
        if let Some(name) = decode_ext_value(&raw) {
            return crate::pathsafe::check_name(&name).ok();
        }
        return None; // filename* 存在但解不出来 ⇒ 别退回去用 filename（那可能是另一份名字）
    }
    let raw = find_param(v, "filename")?;
    let name = unquote(&raw);
    if name.is_empty() {
        return None;
    }
    crate::pathsafe::check_name(&name).ok()
}

/// 找到 `key=value` 里的 value（原样，不做引号处理）。参数名大小写不敏感。
fn find_param(v: &str, key: &str) -> Option<String> {
    let lower = v.to_ascii_lowercase();
    let mut from = 0usize;
    while let Some(idx) = lower[from..].find(key) {
        let start = from + idx;
        // 必须落在参数边界上：`xfilename=` 不该命中
        let boundary_ok = start == 0
            || matches!(
                lower.as_bytes()[start - 1],
                b';' | b' ' | b'\t' | b',' | b'='
            );
        let after = start + key.len();
        if boundary_ok && lower[after..].starts_with('=') {
            let rest = &v[after + 1..];
            // 值到 `;` 为止（引号内的 `;` 要跳过）
            let mut out = String::new();
            let mut in_quote = false;
            for ch in rest.chars() {
                match ch {
                    '"' => {
                        in_quote = !in_quote;
                        out.push(ch);
                    }
                    ';' if !in_quote => break,
                    _ => out.push(ch),
                }
            }
            return Some(out.trim().to_string());
        }
        from = after;
    }
    None
}

/// `filename*=charset'lang'pct-encoded`（RFC 5987）→ 解码后的名字。
fn decode_ext_value(raw: &str) -> Option<String> {
    let raw = unquote(raw);
    let mut parts = raw.splitn(3, '\'');
    let charset = parts.next()?.trim().to_ascii_lowercase();
    let _lang = parts.next()?;
    let encoded = parts.next()?;
    if charset != "utf-8" {
        return None; // 其它编码一律放弃：猜错就是错误文件名
    }
    percent_decode(encoded)
}

/// 去掉一层双引号，并处理 quoted-pair（`\"` → `"`、`\\` → `\`）。
fn unquote(s: &str) -> String {
    let t = s.trim();
    let inner = t
        .strip_prefix('"')
        .and_then(|x| x.strip_suffix('"'))
        .unwrap_or(t);
    let mut out = String::with_capacity(inner.len());
    let mut it = inner.chars();
    while let Some(c) = it.next() {
        if c == '\\' {
            if let Some(n) = it.next() {
                out.push(n);
            }
        } else {
            out.push(c);
        }
    }
    out
}

fn percent_decode(s: &str) -> Option<String> {
    let b = s.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() {
            let hi = (b[i + 1] as char).to_digit(16)?;
            let lo = (b[i + 2] as char).to_digit(16)?;
            out.push((hi * 16 + lo) as u8);
            i += 3;
        } else {
            out.push(b[i]);
            i += 1;
        }
    }
    String::from_utf8(out).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_filename_is_taken() {
        assert_eq!(
            filename_from_content_disposition(r#"attachment; filename="x.zip""#).as_deref(),
            Some("x.zip")
        );
        // 不带引号也合法
        assert_eq!(
            filename_from_content_disposition("attachment; filename=y.jar").as_deref(),
            Some("y.jar")
        );
        // 参数名大小写不敏感
        assert_eq!(
            filename_from_content_disposition(r#"Attachment; FileName="z.bin""#).as_deref(),
            Some("z.bin")
        );
    }

    #[test]
    fn ext_value_is_preferred_and_utf8_decoded() {
        let h = r#"attachment; filename="fallback.zip"; filename*=UTF-8''%E6%8A%A5%E5%91%8A.zip"#;
        assert_eq!(
            filename_from_content_disposition(h).as_deref(),
            Some("报告.zip"),
            "filename* 优先于 filename"
        );
    }

    #[test]
    fn only_attachment_without_filename_is_none() {
        assert_eq!(filename_from_content_disposition("attachment"), None);
        assert_eq!(filename_from_content_disposition("inline"), None);
        assert_eq!(filename_from_content_disposition(""), None);
    }

    /// 这条是 #16 与 #9 的接缝：**这个头也是不可信输入**。
    #[test]
    fn hostile_names_are_rejected_so_caller_falls_back() {
        for h in [
            r#"attachment; filename="../../evil""#,
            r#"attachment; filename="a/b""#,
            r#"attachment; filename="..""#,
            r#"attachment; filename="sub\\evil""#,
            "attachment; filename*=UTF-8''..%2F..%2Fevil",
        ] {
            assert_eq!(
                filename_from_content_disposition(h),
                None,
                "应当拒绝并让调用方回落: {h}"
            );
        }
    }

    /// 盘符这条是 **Windows 专属**规则（Linux 上 `:` 是合法文件名字符）。
    ///
    /// 2026-11-23 CI 在 ubuntu 上抓到过：把它写进上面那个无条件拒绝的列表，本地绿、CI 红 ——
    /// 与 `pathsafe` 那批单测踩过的是同一个坑，所以这里按平台分开断言。
    #[test]
    fn drive_letter_is_rejected_only_on_windows() {
        let got = filename_from_content_disposition(r#"attachment; filename="C:x""#);
        if cfg!(windows) {
            assert_eq!(got, None, "Windows 上带盘符的名字必须拒绝");
        } else {
            assert_eq!(got.as_deref(), Some("C:x"), "非 Windows 上它是合法的单段名");
        }
    }

    #[test]
    fn non_utf8_ext_charset_is_given_up() {
        // iso-8859-1 一律不解 —— 猜错编码的代价是错误文件名，不如回落
        assert_eq!(
            filename_from_content_disposition(r#"attachment; filename*=iso-8859-1'en'%A3.zip"#),
            None
        );
    }

    #[test]
    fn quoted_pair_and_spaces() {
        assert_eq!(
            filename_from_content_disposition(r#"attachment; filename="a \"q\" b.zip""#).as_deref(),
            Some("a \"q\" b.zip")
        );
    }

    #[test]
    fn semicolon_inside_quotes_does_not_end_the_value() {
        let h = r#"attachment; filename="a;b.zip"; size=1"#;
        assert_eq!(
            filename_from_content_disposition(h).as_deref(),
            Some("a;b.zip")
        );
    }
}
