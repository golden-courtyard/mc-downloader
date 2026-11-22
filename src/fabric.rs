use anyhow::{Context, Result, bail};
use serde::Deserialize;
use std::path::Path;

use crate::download::DownloadOpts;
use crate::version::{Job, SizeHint};

#[derive(Debug, Deserialize)]
struct Profile {
    id: String,
    #[serde(default)]
    libraries: Vec<Lib>,
}

#[derive(Debug, Deserialize)]
struct Lib {
    name: String,
    url: String,
    /// Fabric meta 的 profile 里实际给了 `sha1` / `sha256` / `size`，
    /// 但**不是每个库都有**（实测 8 个里 6 个有 sha1）。
    /// 有就保留：既能做完整性校验，也能让 >8MB 的库走分片。
    #[serde(default)]
    sha1: Option<String>,
    #[serde(default)]
    size: Option<u64>,
}

#[derive(Debug, Deserialize)]
struct LoaderListEntry {
    loader: LoaderInfo,
}

#[derive(Debug, Deserialize)]
struct LoaderInfo {
    version: String,
    #[serde(default)]
    stable: bool,
}

/// 从 `GET /v2/versions/loader/{mc}` 的响应里挑**第一个 stable**（**纯函数**，可单测）。
///
/// 一个 stable 都没有 ⇒ **明确报错并列出可选版本**，不许瞎选第一个
/// （那会把用户带到 beta 上，而且 `stable` 这个字段正是用来区分这个的）。
pub fn pick_stable_loader(json: &str) -> Result<String> {
    let list: Vec<LoaderListEntry> =
        serde_json::from_str(json).context("解析 Fabric loader 列表失败")?;
    if list.is_empty() {
        bail!("Fabric meta 返回了空的 loader 列表");
    }
    if let Some(l) = list.iter().find(|e| e.loader.stable) {
        return Ok(l.loader.version.clone());
    }
    let shown: Vec<&str> = list
        .iter()
        .take(5)
        .map(|e| e.loader.version.as_str())
        .collect();
    bail!(
        "Fabric meta 没有标记为 stable 的 loader（前 5 个：{}）。\n\
         请显式指定版本：loader = \"fabric:<版本>\"",
        shown.join(", ")
    )
}

/// 查该 MC 版本最新的 stable Fabric loader。网络失败 ⇒ 报错，**不静默跳过**。
pub async fn latest_stable_loader(mc: &str, opts: &DownloadOpts, base: &str) -> Result<String> {
    let client = crate::download::client(opts)?;
    let url = format!("{base}/v2/versions/loader/{mc}");
    let text = client
        .get(&url)
        .send()
        .await
        .with_context(|| format!("请求 Fabric loader 列表失败: {url}"))?
        .error_for_status()
        .with_context(|| format!("Fabric loader 列表返回错误状态: {url}"))?
        .text()
        .await?;
    let v = pick_stable_loader(&text).with_context(|| format!("MC {mc} 的 loader 列表"))?;
    println!("[*] Fabric loader: {v}（自动选的最新 stable）");
    Ok(v)
}

/// 从 Fabric meta 拉 profile，把 json 落盘，并把它的 libraries 下好。
/// 返回实例 id（fabric-loader-<loader>-<mc>）。
pub async fn install(
    mc: &str,
    loader: &str,
    root: &Path,
    opts: &DownloadOpts,
    base: &str,
) -> Result<String> {
    let client = crate::download::client(opts)?;
    let url = format!("{base}/v2/versions/loader/{mc}/{loader}/profile/json");

    let text = client
        .get(&url)
        .send()
        .await
        .with_context(|| format!("请求 Fabric meta 失败: {url}"))?
        .error_for_status()?
        .text()
        .await?;

    let prof: Profile = serde_json::from_str(&text).context("解析 Fabric profile 失败")?;

    let vdir = root.join("versions").join(&prof.id);
    tokio::fs::create_dir_all(&vdir).await?;
    tokio::fs::write(vdir.join(format!("{}.json", prof.id)), text.as_bytes()).await?;
    println!("[+] Fabric profile -> versions/{0}/{0}.json", prof.id);

    let mut jobs = Vec::new();
    for lib in &prof.libraries {
        let parts: Vec<&str> = lib.name.split(':').collect();
        if parts.len() < 3 {
            continue;
        }
        let (g, a, v) = (parts[0], parts[1], parts[2]);
        let fname = match parts.get(3) {
            Some(c) => format!("{a}-{v}-{c}.jar"),
            None => format!("{a}-{v}.jar"),
        };
        let rel = format!("{}/{}/{}/{}", g.replace('.', "/"), a, v, fname);
        jobs.push(Job {
            url: format!("{}/{rel}", lib.url.trim_end_matches('/')),
            path: format!("libraries/{rel}"),
            sha1: lib.sha1.clone(),
            size: SizeHint::from_exact(lib.size),
            mirrors: Vec::new(),
        });
    }

    println!("[*] Fabric 库: {} 个", jobs.len());
    crate::download::run(&jobs, root, &crate::download::batch_opts(opts)).await?;

    Ok(prof.id)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn picks_first_stable() {
        let json = r#"[
            {"loader":{"version":"0.17.0-beta.1","stable":false}},
            {"loader":{"version":"0.16.5","stable":true}},
            {"loader":{"version":"0.15.0","stable":true}}
        ]"#;
        assert_eq!(pick_stable_loader(json).unwrap(), "0.16.5");
    }

    #[test]
    fn no_stable_is_an_error_listing_options() {
        let json = r#"[{"loader":{"version":"0.17.0-beta.1","stable":false}}]"#;
        let e = pick_stable_loader(json).unwrap_err().to_string();
        assert!(e.contains("没有标记为 stable"), "{e}");
        assert!(e.contains("0.17.0-beta.1"), "要列出可选版本: {e}");
        assert!(e.contains("fabric:<版本>"), "要给出怎么显式指定: {e}");
    }

    #[test]
    fn empty_or_bad_json_is_an_error() {
        assert!(pick_stable_loader("[]").is_err());
        assert!(pick_stable_loader("not json").is_err());
    }
}
