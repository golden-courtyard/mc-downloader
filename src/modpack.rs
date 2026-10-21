use anyhow::{bail, Context, Result};
use serde::Deserialize;
use std::collections::HashMap;
use std::io::Read;
use std::path::Path;

use crate::version::Job;

#[derive(Debug, Deserialize)]
struct Index {
    #[serde(default)]
    name: Option<String>,
    dependencies: HashMap<String, String>,
    #[serde(default)]
    files: Vec<PackFile>,
}

#[derive(Debug, Deserialize)]
struct PackFile {
    path: String,
    downloads: Vec<String>,
    #[serde(default)]
    hashes: HashMap<String, String>,
    #[serde(default)]
    env: Option<HashMap<String, String>>,
}

/// 装一个 Modrinth 格式整合包（.mrpack / 同样结构的 .zip）
pub async fn install(pack: &Path, root: &Path, proxy: Option<&str>, retries: u32) -> Result<()> {
    println!("[*] 读取 {}", pack.display());

    let file = std::fs::File::open(pack).with_context(|| "打不开整合包文件")?;
    let mut zip = zip::ZipArchive::new(file).context("不是有效的 zip")?;

    let idx: Index = {
        let mut s = String::new();
        zip.by_name("modrinth.index.json")
            .context("不是 Modrinth 整合包：缺少 modrinth.index.json")?
            .read_to_string(&mut s)?;
        serde_json::from_str(&s).context("解析 modrinth.index.json 失败")?
    };

    let mc = idx
        .dependencies
        .get("minecraft")
        .context("整合包未指定 minecraft 版本")?
        .clone();

    if idx.dependencies.contains_key("forge") || idx.dependencies.contains_key("neoforge") {
        bail!(
            "暂不支持 Forge / NeoForge（本工具只做 Fabric）。\
             依赖项: {:?}",
            idx.dependencies
        );
    }

    let loader = idx
        .dependencies
        .get("fabric-loader")
        .cloned()
        .context("整合包未指定 fabric-loader")?;

    println!("[*] 包名  : {}", idx.name.as_deref().unwrap_or("(未命名)"));
    println!("[*] MC    : {mc}");
    println!("[*] Fabric: {loader}");
    println!("[*] 模组  : {} 个", idx.files.len());

    // ① 原版
    crate::vanilla::install(&mc, root, proxy, retries).await?;

    // ② Fabric
    let inst_id = crate::fabric::install(&mc, &loader, root, proxy, retries).await?;

    // ③ 模组（按 Modrinth 的 path 落到实例目录）
    let inst = root.join("versions").join(&inst_id);
    tokio::fs::create_dir_all(&inst).await?;

    let mut jobs = Vec::new();
    let mut skipped = 0usize;
    for f in &idx.files {
        if let Some(env) = &f.env {
            if env.get("client").map(|c| c == "unsupported").unwrap_or(false) {
                skipped += 1;
                continue;
            }
        }
        let Some(url) = f.downloads.first() else {
            continue;
        };
        jobs.push(Job {
            url: url.clone(),
            path: f.path.clone(),
            sha1: f.hashes.get("sha1").cloned(),
                size: None,
        });
    }
    if skipped > 0 {
        println!("[*] 跳过 client=unsupported 的 {skipped} 个");
    }
    crate::download::run(&jobs, &inst, proxy, retries).await?;

    // ④ overrides / client-overrides
    let mut copied = 0usize;
    for i in 0..zip.len() {
        let mut entry = zip.by_index(i)?;
        let name = entry.name().to_string();
        let rel = match name
            .strip_prefix("overrides/")
            .or_else(|| name.strip_prefix("client-overrides/"))
        {
            Some(r) => r,
            None => continue,
        };
        if rel.is_empty() || name.ends_with('/') {
            continue;
        }
        let dst = inst.join(rel.replace('/', std::path::MAIN_SEPARATOR_STR));
        if let Some(parent) = dst.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }
        let mut buf = Vec::new();
        entry.read_to_end(&mut buf)?;
        tokio::fs::write(&dst, buf).await?;
        copied += 1;
    }
    println!("[+] overrides: {copied} 个文件");

    println!("[+] 完成 -> {}", inst.display());
    println!("[i] 把 PCL 的游戏目录指向 {} 即可。", root.display());
    Ok(())
}
