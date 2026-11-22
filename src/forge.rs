//! Forge / NeoForge 加载器安装。
//!
//! **分工（DESIGN §15）**：下载归我们，构建归官方 installer。
//!
//! Forge 的安装需要跑 10 个 Java processor（`SpecialSource` / `binarypatcher` 等，
//! 会改客户端字节码）。那是**构建**，不是下载——我们不重写它，而是：
//!
//! 1. 下 installer jar
//! 2. 解析 `install_profile.json` 的 `libraries`，**用我们自己的下载器下**（有重试/续传/进度）
//! 3. `launcher_profiles.json` 不存在才建（那是启动器的状态文件，**绝不覆盖**）
//! 4. `java -jar <installer> --installClient <dir> --offline` —— `--offline` 只跑 processor、不联网
//! 5. 验证产物存在
//!
//! **为什么步骤 2 必须由我们做**：实测 installer 自己下载时，
//! 一个库（`trove:trove:1.0.2`，361 KB）SSL 抖动就 `There was an error during installation`，
//! **无重试、无续传、无进度**。36 个库里只要有 1 个失败，整个安装就废。

use anyhow::{Context, Result, bail};
use serde::Deserialize;
use std::path::Path;
use std::process::Command;

use crate::download::DownloadOpts;
use crate::version::{Job, SizeHint};

#[derive(Debug, Deserialize)]
struct InstallProfile {
    #[serde(default)]
    libraries: Vec<Lib>,
}

#[derive(Debug, Deserialize)]
struct Lib {
    name: String,
    #[serde(default)]
    downloads: Option<LibDownloads>,
}

#[derive(Debug, Deserialize)]
struct LibDownloads {
    #[serde(default)]
    artifact: Option<Artifact>,
}

#[derive(Debug, Deserialize)]
struct Artifact {
    path: String,
    url: String,
    #[serde(default)]
    sha1: Option<String>,
    #[serde(default)]
    size: Option<u64>,
}

/// 从 `promotions_slim.json` 里挑指定 MC 的 latest Forge 版本（**纯函数**，可单测）。
///
/// 响应形如 `{"promos": {"1.20.1-latest": "47.4.10", "1.20.1-recommended": "47.3.0", ...}}`。
/// 先找 `-recommended`（更稳），没有再找 `-latest`。
pub fn pick_promo_version(json: &str, mc: &str) -> Result<String> {
    let v: serde_json::Value = serde_json::from_str(json).context("解析 Forge promotions 失败")?;
    let promos = v
        .get("promos")
        .and_then(|p| p.as_object())
        .context("promotions 里没有 promos 字段")?;

    for key in [format!("{mc}-recommended"), format!("{mc}-latest")] {
        if let Some(ver) = promos.get(&key).and_then(|x| x.as_str())
            && !ver.is_empty()
        {
            return Ok(ver.to_string());
        }
    }

    let mut mcs: Vec<&str> = promos
        .keys()
        .filter_map(|k| k.strip_suffix("-latest"))
        .collect();
    mcs.sort_unstable();
    mcs.dedup();
    bail!(
        "Forge 没有 MC {mc} 的发布版本。已发布的 MC 版本（末尾几个）：{}",
        mcs.iter()
            .rev()
            .take(6)
            .cloned()
            .collect::<Vec<_>>()
            .join(", ")
    )
}

/// library 的 URL 只该来自**用户配的那些源的 host**（纯函数，可单测）。
///
/// 防的是「install_profile 被篡改 / 上游变更，某个 library 的 url 指向别处」
/// —— 我们不希望自己变成一个「照着任意 URL 下载」的执行器。
///
/// **白名单从配置推出，不写死一份**（issue #15）：写死的话，用户把自己的镜像填进 `[source]` 之后，
/// installer 给的库地址会被我们自己的白名单拒掉 —— 那是"配置生效了一半"的典型坑。
pub fn is_allowed_host(url: &str, allowed: &[String]) -> bool {
    let Some(host) = crate::settings::Sources::host_of(url) else {
        return false;
    };
    allowed.iter().any(|a| a.eq_ignore_ascii_case(&host))
}

/// 查该 MC 版本最新的 Forge（先 recommended，后 latest）。网络失败 ⇒ 报错，**不静默跳过**。
pub async fn latest_forge(mc: &str, opts: &DownloadOpts, meta_base: &str) -> Result<String> {
    let client = crate::download::client(opts)?;
    let url = format!("{meta_base}/net/minecraftforge/forge/promotions_slim.json");
    let text = client
        .get(&url)
        .send()
        .await
        .with_context(|| format!("请求 Forge promotions 失败: {url}"))?
        .error_for_status()
        .with_context(|| format!("Forge promotions 返回错误状态: {url}"))?
        .text()
        .await?;
    let v = pick_promo_version(&text, mc).with_context(|| format!("MC {mc} 的 Forge 版本"))?;
    println!("[*] Forge 版本: {v}（自动选的最新 recommended/latest）");
    Ok(v)
}

/// 从 `install_profile.json` 抽出要下载的 libraries（**纯函数**，可单测）。
///
/// 跳过没有 `downloads.artifact` 的条目：那些是 processor 的运行依赖，
/// 由 installer 自己在 `--offline` 阶段从已就位的 `libraries/` 里取；
/// 没有 `url` 的（本地文件）也跳过。
pub fn jobs_from_profiles(jsons: &[(&str, &str)], allowed: &[String]) -> Result<Vec<Job>> {
    let mut seen = std::collections::HashSet::new();
    let mut jobs = Vec::new();
    let mut total_libs = 0usize;
    for (label, json) in jsons {
        let prof: InstallProfile =
            serde_json::from_str(json).with_context(|| format!("解析 {label} 失败"))?;
        total_libs += prof.libraries.len();
        for lib in &prof.libraries {
            let Some(art) = lib.downloads.as_ref().and_then(|d| d.artifact.as_ref()) else {
                continue;
            };
            if art.path.is_empty() || art.url.is_empty() {
                continue;
            }
            // 只认官方 maven；不认识的源跳过并警告（一个库不合格不该毁掉整次安装）
            if !is_allowed_host(&art.url, allowed) {
                eprintln!("[!] 跳过非官方源的 library: {} -> {}", lib.name, art.url);
                continue;
            }
            // 两个 json 会有重叠（如 forge / fmlcore），按 maven path 去重
            if !seen.insert(art.path.clone()) {
                continue;
            }
            jobs.push(Job {
                url: art.url.clone(),
                path: format!("libraries/{}", art.path),
                sha1: art.sha1.clone(),
                size: SizeHint::from_exact(art.size),
                mirrors: Vec::new(),
            });
        }
    }
    if jobs.is_empty() {
        bail!("两个 json 里没有任何可下载的 library（共 {total_libs} 条，全是 processor 依赖？）");
    }
    Ok(jobs)
}

/// Forge 的版本 id 形如 `1.20.1-forge-47.4.10`（installer 自己生成的目录名）。
pub fn version_id(mc: &str, forge: &str) -> String {
    format!("{mc}-forge-{forge}")
}

/// 从 `opts.proxy` 拆出 host/port，给 JVM 的 `-Dhttp(s).proxyHost/Port` 用。
///
/// **刻意不引入 `url` crate**：只支持 `http(s)://host:port` 这种形态，
/// 与 `download.rs` 里 `reqwest::Proxy::all()` 接受的范围一致。
/// IPv6 字面量（`[::1]:7897`）暂不支持 —— MC 场景里不会出现，真遇到再加。
pub fn proxy_parts(opts: &DownloadOpts) -> Option<(String, u16)> {
    let raw = opts.proxy.as_deref()?;
    let rest = raw
        .strip_prefix("http://")
        .or_else(|| raw.strip_prefix("https://"))
        .unwrap_or(raw);
    let authority = rest.split('/').next()?;
    match authority.rsplit_once(':') {
        Some((h, p)) if !h.is_empty() => Some((h.to_string(), p.parse().ok()?)),
        Some(_) => None,
        // 没写端口：http 默认 80
        None if !authority.is_empty() => Some((authority.to_string(), 80)),
        None => None,
    }
}

pub fn has_java() -> bool {
    Command::new("java")
        .arg("-version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// 安装 Forge 到 `root`。返回版本 id。
pub async fn install(
    mc: &str,
    forge: &str,
    root: &Path,
    opts: &DownloadOpts,
    src: &crate::settings::Sources,
) -> Result<String> {
    let id = version_id(mc, forge);
    let client = crate::download::client(opts)?;

    // ① 下 installer jar
    let ins_name = format!("forge-{mc}-{forge}-installer.jar");
    let ins_url = format!(
        "{}/net/minecraftforge/forge/{mc}-{forge}/{ins_name}",
        src.forge_maven
    );
    let cache = root.join(".cache-installers");
    tokio::fs::create_dir_all(&cache).await?;
    let ins_path = cache.join(&ins_name);
    if !ins_path.exists() {
        println!("[*] 下载 Forge installer: {ins_url}");
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

    // ② 读 install_profile.json（在 installer jar 里）
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

    // ⚠️ **两个 json 都要下**：`install_profile` 是安装用的 36 个，
    // 但 `--offline` 阶段 installer 检查的是 `version.json` 的 29 个**运行时**库
    // （`net.minecraftforge:eventbus` / `cpw.mods:modlauncher` 那些）。
    // 只下前者会得到 "These libraries failed to download"（实测）。
    // 白名单从**用户配的源**推出：换镜像后 installer 给的库地址不该被自己拒掉
    let allowed = src.hosts();
    let jobs = jobs_from_profiles(
        &[
            ("install_profile.json", &profile_json),
            ("version.json", &version_json),
        ],
        &allowed,
    )?;
    println!(
        "[*] 要下的 library: {} 个（install_profile + version.json 合并去重；我们自己下，比 installer 稳）",
        jobs.len()
    );

    // ③ 用我们的下载器下（有重试/续传/进度）
    crate::download::run(&jobs, root, &crate::download::batch_opts(opts)).await?;

    // ④ launcher_profiles.json —— **不存在才建**。那是启动器的状态文件，
    //    覆盖会毁掉用户的账号和启动配置。
    let lp = root.join("launcher_profiles.json");
    if !lp.exists() {
        tokio::fs::write(&lp, b"{\"profiles\":{},\"settings\":{},\"version\":3}\n").await?;
        println!("[+] 建了空的 launcher_profiles.json（installer 要求有这个文件）");
    }

    // ⑤ 调 installer：--offline = 只跑 processor、不联网（下载我们已经做完）
    if !has_java() {
        bail!(
            "Forge 安装需要 Java（找不到 `java` 命令）\n\
             \x20 下载：https://adoptium.net/  （选 JRE 17 或更新）\n\
             \x20 装完确认 `java -version` 有输出，再重跑本命令"
        );
    }

    // ⚠️ **不给 installer 传代理参数**。
    //
    // `--offline` 的语义是"只跑 processor、不联网"，库我们已经下完了 ⇒ 它不需要代理。
    // 而实测（2026-10-25，NeoForge 21.1.265）：**传了代理参数反而诱使它去联网**，
    // 结果是 `SSLHandshakeException: Remote host terminated the handshake` 装不上；
    // 不传代理，同一个 installer 65 秒跑完并 `Successfully installed client into launcher`。
    // 代理是给【我们的下载器】用的，不是给 installer 用的 —— 两件事别混。
    let mut cmd = Command::new("java");
    println!("[*] 跑 installer --offline（只跑 processor，不联网）");
    let out = cmd
        .arg("-jar")
        .arg(&ins_path)
        .arg("--installClient")
        .arg(root)
        .arg("--offline")
        .output()
        .context("启动 java installer 失败")?;

    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    if !out.status.success() {
        // 只贴最后 20 行，避免刷屏；但必须让用户看到真实原因
        let tail: Vec<&str> = stdout.lines().rev().take(20).collect();
        let tail: Vec<&str> = tail.into_iter().rev().collect();
        bail!(
            "Forge installer 失败（exit {:?}）\n最后 20 行：\n{}\n{}",
            out.status.code(),
            tail.join("\n"),
            stderr
                .lines()
                .rev()
                .take(5)
                .collect::<Vec<_>>()
                .into_iter()
                .rev()
                .collect::<Vec<_>>()
                .join("\n")
        );
    }

    // ⑥ 验证产物 —— 缺任一就报错，不许静默成功
    let vjson = root.join("versions").join(&id).join(format!("{id}.json"));
    if !vjson.exists() {
        bail!("installer 声称成功，但 {} 不存在", vjson.display());
    }
    let vtxt = tokio::fs::read_to_string(&vjson).await?;
    let v: serde_json::Value = serde_json::from_str(&vtxt)?;
    let main_class = v.get("mainClass").and_then(|x| x.as_str()).unwrap_or("");
    if main_class.is_empty() {
        bail!("{} 里 mainClass 为空", vjson.display());
    }

    let patched = root
        .join("libraries/net/minecraftforge/forge")
        .join(format!("{mc}-{forge}"))
        .join(format!("forge-{mc}-{forge}-client.jar"));
    if !patched.exists() {
        bail!(
            "installer 跑完了，但 patched client jar 不存在：{}\n\
             （这说明 processor 没真正跑成功）",
            patched.display()
        );
    }

    println!("[+] Forge {forge} 装好 -> versions/{id}/");
    println!("[i] mainClass = {main_class}");
    println!("[i] 用启动器（PCL/HMCL）选 {id} 即可（本工具只保证目录结构，未验证能否启动）");
    Ok(id)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn picks_recommended_before_latest() {
        let json = r#"{"promos":{
            "1.20.1-latest":"47.4.10",
            "1.20.1-recommended":"47.3.0",
            "1.19.2-latest":"43.3.0"
        }}"#;
        assert_eq!(pick_promo_version(json, "1.20.1").unwrap(), "47.3.0");
    }

    #[test]
    fn falls_back_to_latest_without_recommended() {
        let json = r#"{"promos":{"1.20.1-latest":"47.4.10"}}"#;
        assert_eq!(pick_promo_version(json, "1.20.1").unwrap(), "47.4.10");
    }

    #[test]
    fn unknown_mc_lists_available() {
        let json = r#"{"promos":{"1.20.1-latest":"47.4.10","1.19.2-latest":"43.3.0"}}"#;
        let e = pick_promo_version(json, "9.9.9").unwrap_err().to_string();
        assert!(e.contains("没有 MC 9.9.9"), "{e}");
        assert!(e.contains("1.20.1"), "要列出可选版本: {e}");
    }

    #[test]
    fn profile_skips_processor_deps_without_artifact() {
        // 第一条有 artifact，第二条没有（processor 依赖），第三条 url 为空
        let json = r#"{"libraries":[
            {"name":"a:b:1","downloads":{"artifact":{"path":"a/b/1/b-1.jar","url":"https://maven.example.invalid/b-1.jar","sha1":"aa","size":10}}},
            {"name":"c:d:2"},
            {"name":"e:f:3","downloads":{"artifact":{"path":"e/f/3/f-3.jar","url":""}}}
        ]}"#;
        let jobs = jobs_from_profiles(&[("install_profile.json", json)], &allowed()).unwrap();
        assert_eq!(jobs.len(), 1);
        assert_eq!(jobs[0].path, "libraries/a/b/1/b-1.jar");
        assert_eq!(jobs[0].url, "https://maven.example.invalid/b-1.jar");
        assert_eq!(jobs[0].sha1.as_deref(), Some("aa"));
        assert_eq!(jobs[0].size, SizeHint::Exact(10));
    }

    #[test]
    fn profile_without_downloadable_libs_is_an_error() {
        let json = r#"{"libraries":[{"name":"a:b:1"},{"name":"c:d:2"}]}"#;
        let e = jobs_from_profiles(&[("install_profile.json", json)], &allowed())
            .unwrap_err()
            .to_string();
        assert!(e.contains("没有任何可下载的 library"), "{e}");
    }

    #[test]
    fn merges_two_jsons_and_dedups_by_path() {
        // a 在两个 json 里都有（重复），b 只在第二个里
        let p1 = r#"{"libraries":[
            {"name":"x:a:1","downloads":{"artifact":{"path":"x/a/1/a-1.jar","url":"https://maven.example.invalid/a-1.jar"}}}
        ]}"#;
        let p2 = r#"{"libraries":[
            {"name":"x:a:1","downloads":{"artifact":{"path":"x/a/1/a-1.jar","url":"https://maven.example.invalid/a-1.jar"}}},
            {"name":"y:b:2","downloads":{"artifact":{"path":"y/b/2/b-2.jar","url":"https://maven.example.invalid/b-2.jar"}}}
        ]}"#;
        let jobs = jobs_from_profiles(&[("p1", p1), ("p2", p2)], &allowed()).unwrap();
        assert_eq!(
            jobs.len(),
            2,
            "重复的 path 要去掉: {:?}",
            jobs.iter().map(|j| &j.path).collect::<Vec<_>>()
        );
        assert!(jobs.iter().any(|j| j.path == "libraries/x/a/1/a-1.jar"));
        assert!(jobs.iter().any(|j| j.path == "libraries/y/b/2/b-2.jar"));
    }

    /// 白名单是**从配置推出来的**：只认配置里那些 host，大小写不敏感，`host.evil.com` 不算。
    fn allowed() -> Vec<String> {
        [
            "maven.example.invalid",
            "files.example.invalid",
            "libraries.example.invalid",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect()
    }

    #[test]
    fn only_configured_hosts_are_allowed() {
        let a = allowed();
        assert!(is_allowed_host("https://maven.example.invalid/a/b.jar", &a));
        assert!(is_allowed_host("https://files.example.invalid/x", &a));
        // 大小写不敏感（host 里大写是合法的）
        assert!(is_allowed_host("https://MAVEN.EXAMPLE.INVALID/x.jar", &a));
        // 不在配置里的一律不信 —— 包括"长得像"的
        assert!(!is_allowed_host("https://evil.example.com/x.jar", &a));
        assert!(!is_allowed_host(
            "https://maven.example.invalid.evil.com/x",
            &a
        ));
        assert!(!is_allowed_host("http://127.0.0.1:8080/x.jar", &a));
        assert!(!is_allowed_host("", &a), "空串要判 false");
        assert!(!is_allowed_host("not a url", &a));
        // 空白名单 = 什么都不放行（不是"全放行"）
        assert!(!is_allowed_host("https://maven.example.invalid/x.jar", &[]));
    }

    #[test]
    fn version_id_matches_installer_convention() {
        assert_eq!(version_id("1.20.1", "47.4.10"), "1.20.1-forge-47.4.10");
    }

    #[test]
    fn proxy_parts_parses_host_and_port() {
        let o = DownloadOpts {
            proxy: Some("http://127.0.0.1:7897".into()),
            ..Default::default()
        };
        assert_eq!(proxy_parts(&o), Some(("127.0.0.1".into(), 7897)));
    }

    #[test]
    fn proxy_parts_none_when_absent() {
        let o = DownloadOpts {
            proxy: None,
            ..Default::default()
        };
        assert_eq!(proxy_parts(&o), None);
    }
}
