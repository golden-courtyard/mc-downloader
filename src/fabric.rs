use anyhow::{Context, Result};
use serde::Deserialize;
use std::path::Path;

use crate::version::Job;

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
}

/// 从 Fabric meta 拉 profile，把 json 落盘，并把它的 libraries 下好。
/// 返回实例 id（fabric-loader-<loader>-<mc>）。
pub async fn install(mc: &str, loader: &str, root: &Path) -> Result<String> {
    let url = format!(
        "https://meta.fabricmc.net/v2/versions/loader/{mc}/{loader}/profile/json"
    );

    let text = reqwest::get(&url)
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
            sha1: None,
        });
    }

    println!("[*] Fabric 库: {} 个", jobs.len());
    crate::download::run(&jobs, root).await?;

    Ok(prof.id)
}
