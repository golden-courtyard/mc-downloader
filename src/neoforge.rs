//! NeoForge 安装。
//!
//! 和 `forge.rs` **同一个思路**：下载归我们，构建归官方 installer。
//! 保留独立文件而不是抽 `enum Loader` 的理由：`forge.rs` 刚通过真机验收
//! （PCL 能启动），为复用去改它 = 拿已验收的路径冒险。复制主流程约 60 行更便宜。
//!
//! ## 和 Forge 的三处不同
//!
//! | | Forge | NeoForge |
//! |---|---|---|
//! | installer URL | `maven.minecraftforge.net/.../forge/{mc}-{ver}/forge-{mc}-{ver}-installer.jar` | `maven.neoforged.net/releases/net/neoforged/neoforge/{ver}/neoforge-{ver}-installer.jar` |
//! | 版本来源 | `promos.json` 的 `recommended` | `maven-metadata.xml` 取最高 |
//! | 版本映射 | `{mc}-{forge}` | MC 去掉 `1.` 前缀（`1.21.1` → `21.1`）|
//!
//! ## 版本映射的边界
//!
//! **NeoForge 从 MC 1.20.2 开始**（1.20.1 时代还叫 Forge）。
//! 所以 `1.20.1` 会落到 `20.1.` 前缀，而 maven 里没有这个前缀 ⇒ 自然报错。
//! **不特判 1.20.1**，让"版本列表里没有"统一处理，并给出可用前缀提示。

use std::path::Path;
use std::process::Command;

use crate::download::{self, DownloadOpts};
use anyhow::{Context, Result, bail};

/// MC 版本 → NeoForge 版本前缀。
///
/// `1.21.1` → `21.1` · `1.20.2` → `20.2` · `26.3` → `26.3`
///
/// 规则是**去掉 `1.` 前缀**（NeoForge 的命名把 `1.x` 写成 `x`）。
/// 只有 `1.` 开头的才去；`26.3` 这种原样返回。
pub fn neo_prefix(mc: &str) -> &str {
    mc.strip_prefix("1.").unwrap_or(mc)
}

/// 从 `maven-metadata.xml` 里挑该 MC 版本对应的最高 NeoForge 版本。
///
/// ⚠️ **不能按字符串比大小** —— `21.1.99` 字典序大于 `21.1.264`，但版本号更小。
/// 必须逐段按数字比。
pub fn pick_latest(xml: &str, mc: &str) -> Result<String> {
    let prefix = format!("{}.", neo_prefix(mc));

    let mut best: Option<&str> = None;
    for cap in xml.split("<version>").skip(1) {
        let Some(end) = cap.find("</version>") else {
            continue;
        };
        let v = cap[..end].trim();
        if !v.starts_with(&prefix) {
            continue;
        }
        best = Some(match best {
            Some(b) if version_cmp(v, b) != std::cmp::Ordering::Greater => b,
            _ => v,
        });
    }

    match best {
        Some(v) => Ok(v.to_string()),
        None => {
            // 列出 maven 里实际存在的前缀，帮用户判断"是不是这个 MC 版本根本没有 NeoForge"
            let mut prefixes: Vec<&str> = xml
                .split("<version>")
                .skip(1)
                .filter_map(|c| c.find("</version>").map(|e| &c[..e]))
                .filter_map(|v| v.rsplit_once('.').map(|(p, _)| p))
                .collect();
            prefixes.sort_unstable();
            prefixes.dedup();
            let show: Vec<&str> = prefixes.iter().rev().take(6).rev().copied().collect();

            bail!(
                "MC {mc} 没有对应的 NeoForge（找的是 `{prefix}x` 前缀）\n\
                 \x20 Neoforge 从 MC **1.20.2** 开始，1.20.1 及更早请用 `forge`\n\
                 \x20 maven 上现有的前缀（最近几个）: {}",
                show.join(", ")
            )
        }
    }
}

/// 按数字段比较版本号（`21.1.264` > `21.1.99`）。
///
/// 段数不同时短的算小（`21.1` < `21.1.1`）；非数字段按字符串比（兜底，不 panic）。
fn version_cmp(a: &str, b: &str) -> std::cmp::Ordering {
    use std::cmp::Ordering;
    let mut ai = a.split('.');
    let mut bi = b.split('.');
    loop {
        match (ai.next(), bi.next()) {
            (None, None) => return Ordering::Equal,
            (None, Some(_)) => return Ordering::Less,
            (Some(_), None) => return Ordering::Greater,
            (Some(x), Some(y)) => {
                let ord = match (x.parse::<u64>(), y.parse::<u64>()) {
                    (Ok(nx), Ok(ny)) => nx.cmp(&ny),
                    _ => x.cmp(y),
                };
                if ord != Ordering::Equal {
                    return ord;
                }
            }
        }
    }
}

/// 版本目录 id：`{mc}-neoforge-{ver}`（和 Forge 的 `{mc}-forge-{ver}` 对齐）。
pub fn version_id(mc: &str, ver: &str) -> String {
    format!("{mc}-neoforge-{ver}")
}

/// 查该 MC 版本最新的 NeoForge（不指定版本时用）。
pub async fn latest(mc: &str, opts: &DownloadOpts) -> Result<String> {
    let url = "https://maven.neoforged.net/releases/net/neoforged/neoforge/maven-metadata.xml";
    let client = download::client(opts)?;
    let xml = client
        .get(url)
        .send()
        .await
        .with_context(|| format!("请求 {url} 失败"))?
        .error_for_status()
        .with_context(|| format!("{url} 返回错误状态"))?
        .text()
        .await?;
    pick_latest(&xml, mc)
}

/// 装 NeoForge 到 `root`，返回版本目录 id。
///
/// 流程和 `forge::install` 一致：下 installer → 读两个 json → 我们下 libraries
/// → 建 launcher_profiles → 跑 installer `--offline` → 验产物。
pub async fn install(mc: &str, ver: &str, root: &Path, opts: &DownloadOpts) -> Result<String> {
    let id = version_id(mc, ver);
    let client = download::client(opts)?;

    // ① 下 installer jar
    let ins_name = format!("neoforge-{ver}-installer.jar");
    let ins_url =
        format!("https://maven.neoforged.net/releases/net/neoforged/neoforge/{ver}/{ins_name}");
    let cache = root.join(".cache-installers");
    tokio::fs::create_dir_all(&cache).await?;
    let ins_path = cache.join(&ins_name);
    if !ins_path.exists() {
        println!("[*] 下载 NeoForge installer: {ins_url}");
        let body = client
            .get(&ins_url)
            .send()
            .await
            .with_context(|| format!("请求 installer 失败: {ins_url}"))?
            .error_for_status()
            .with_context(|| format!("installer 返回错误状态: {ins_url}"))?
            .bytes()
            .await?;
        tokio::fs::write(&ins_path, &body).await?;
    }
    println!(
        "[*] installer: {}（{} KB）",
        ins_path.display(),
        std::fs::metadata(&ins_path)?.len() / 1024
    );

    // ② 读 install_profile.json + version.json（都在 installer jar 里）
    let pf = ins_path.clone();
    let (profile_json, version_json) =
        tokio::task::spawn_blocking(move || -> Result<(String, String)> {
            let f = std::fs::File::open(&pf)?;
            let mut z = zip::ZipArchive::new(f).context("installer 不是合法 zip")?;
            let mut read = |name: &str| -> Result<String> {
                let mut e = z
                    .by_name(name)
                    .with_context(|| format!("installer 里没有 {name}"))?;
                let mut s = String::new();
                std::io::Read::read_to_string(&mut e, &mut s)?;
                Ok(s)
            };
            Ok((read("install_profile.json")?, read("version.json")?))
        })
        .await??;

    // 两个 json 都要下（理由同 forge.rs：--offline 阶段校验的是 version.json 的运行时库）
    let jobs = crate::forge::jobs_from_profiles(&[
        ("install_profile.json", &profile_json),
        ("version.json", &version_json),
    ])?;
    println!(
        "[*] 要下的 library: {} 个（install_profile + version.json 合并去重）",
        jobs.len()
    );

    // ③ 我们下（有重试/续传/进度/限速）
    download::run(&jobs, root, &download::batch_opts(opts)).await?;

    // ④ launcher_profiles.json —— 不存在才建（那是启动器状态文件，覆盖会毁账号配置）
    let lp = root.join("launcher_profiles.json");
    if !lp.exists() {
        tokio::fs::write(&lp, b"{\"profiles\":{},\"settings\":{},\"version\":3}\n").await?;
        println!("[+] 建了空的 launcher_profiles.json（installer 要求）");
    }

    // ⑤ 跑 installer --offline
    if !crate::forge::has_java() {
        bail!(
            "NeoForge 安装需要 Java（找不到 `java` 命令）\n\
             \x20 下载：https://adoptium.net/  （选 JRE 21 或更新；1.21.1 需要 Java 21）"
        );
    }

    let mut cmd = Command::new("java");
    if let Some((h, p)) = crate::forge::proxy_parts(opts) {
        cmd.arg(format!("-Dhttp.proxyHost={h}"))
            .arg(format!("-Dhttp.proxyPort={p}"))
            .arg(format!("-Dhttps.proxyHost={h}"))
            .arg(format!("-Dhttps.proxyPort={p}"));
    }
    println!("[*] 跑 installer --offline（只跑 processor，不联网）");
    let out = cmd
        .arg("-jar")
        .arg(&ins_path)
        .arg("--installClient")
        .arg(root)
        .arg("--offline")
        .output()
        .with_context(|| "启动 java 失败（检查 `java -version`）")?;

    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        let tail: String = stderr.lines().rev().take(8).collect::<Vec<_>>().join("\n");
        bail!(
            "NeoForge installer 失败（退出码 {:?}）：\n{tail}",
            out.status.code()
        );
    }

    // ⑥ 验产物
    let vdir = root.join("versions").join(&id);
    let vjson = vdir.join(format!("{id}.json"));
    if !vjson.exists() {
        bail!(
            "installer 跑完了，但 {}/ 里没有 {id}.json\n\
             \x20 这说明 processor 没真正跑成功",
            vdir.display()
        );
    }
    let text = tokio::fs::read_to_string(&vjson).await?;
    let parsed: serde_json::Value = serde_json::from_str(&text).context("版本 json 解析失败")?;
    let main = parsed
        .get("mainClass")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    if main.is_empty() {
        bail!("{id}.json 里没有 mainClass（产物不完整）");
    }

    // patched client jar：NeoForge 的路径也在 net/neoforged/neoforge/{ver}/
    let patched = root
        .join("libraries/net/neoforged/neoforge")
        .join(ver)
        .join(format!("neoforge-{ver}-client.jar"));
    if !patched.exists() {
        println!(
            "[!] 没找到 {}（processor 可能没生成，但 mainClass 有值）",
            patched.display()
        );
    }

    println!("[+] NeoForge {ver} 装好 -> versions/{id}/");
    println!("[i] mainClass = {main}");
    Ok(id)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prefix_strips_leading_one_only() {
        assert_eq!(neo_prefix("1.21.1"), "21.1");
        assert_eq!(neo_prefix("1.20.2"), "20.2");
        assert_eq!(neo_prefix("26.3"), "26.3", "没有 1. 前缀的原样返回");
        assert_eq!(
            neo_prefix("1.20.1"),
            "20.1",
            "1.20.1 也去前缀（但没有对应版本，由 pick_latest 报错）"
        );
    }

    #[test]
    fn version_compare_is_numeric_not_lexicographic() {
        use std::cmp::Ordering;
        assert_eq!(
            version_cmp("21.1.264", "21.1.99"),
            Ordering::Greater,
            "264 > 99，字符串比会错"
        );
        assert_eq!(version_cmp("21.1.99", "21.1.264"), Ordering::Less);
        assert_eq!(version_cmp("26.3.0.98", "26.3.0.97"), Ordering::Greater);
        assert_eq!(
            version_cmp("21.1", "21.1.1"),
            Ordering::Less,
            "段数少的算小"
        );
        assert_eq!(version_cmp("21.1.1", "21.1.1"), Ordering::Equal);
    }

    const SAMPLE: &str = r#"
<metadata>
<versioning>
<versions>
<version>20.2.86</version>
<version>20.2.92</version>
<version>21.1.99</version>
<version>21.1.264</version>
<version>26.3.0.98</version>
</versions>
</versioning>
</metadata>
"#;

    #[test]
    fn picks_highest_for_that_mc() {
        assert_eq!(
            pick_latest(SAMPLE, "1.21.1").unwrap(),
            "21.1.264",
            "不是 21.1.99"
        );
        assert_eq!(pick_latest(SAMPLE, "1.20.2").unwrap(), "20.2.92");
        assert_eq!(pick_latest(SAMPLE, "26.3").unwrap(), "26.3.0.98");
    }

    #[test]
    fn reports_missing_prefix_with_hint() {
        let e = pick_latest(SAMPLE, "1.20.1").unwrap_err().to_string();
        assert!(e.contains("没有对应的 NeoForge"), "{e}");
        assert!(e.contains("1.20.2"), "要提示从 1.20.2 开始: {e}");
    }

    #[test]
    fn version_id_matches_forge_convention() {
        assert_eq!(version_id("1.21.1", "21.1.264"), "1.21.1-neoforge-21.1.264");
    }
}
