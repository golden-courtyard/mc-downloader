use crate::download::DownloadOpts;
use anyhow::Result;
use std::path::Path;

/// 下载原版：版本 JSON + client jar + libraries（含 native）+ assets
pub async fn install(mc: &str, root: &Path, opts: &DownloadOpts) -> Result<()> {
    let client = crate::download::client(opts)?;

    let mf = crate::manifest::fetch(&client).await?;
    let ver = crate::manifest::find(&mf, mc)?;
    println!("[*] {}  ({})", ver.id, ver.kind);

    let (vj, raw) = crate::version::fetch(&client, &ver.url).await?;

    // 版本 JSON 必须先落盘：PCL / 官方启动器都靠它识别版本
    let vdir = root.join("versions").join(&vj.id);
    tokio::fs::create_dir_all(&vdir).await?;
    tokio::fs::write(vdir.join(format!("{}.json", vj.id)), raw.as_bytes()).await?;
    println!("[+] 版本 JSON -> versions/{0}/{0}.json", vj.id);

    let jobs = crate::version::collect(&client, &vj).await?;
    println!("[*] 待下载: {} 个文件", jobs.len());

    crate::download::run(&jobs, root, opts).await
}
