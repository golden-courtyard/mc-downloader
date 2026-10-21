//! 整合包安装流水线。
//!
//! ```text
//! 归档 ──parse──▶ PackSpec ──▶ 原版 ──▶ 加载器 ──▶ 文件 ──▶ 覆盖层
//!      (可插拔)   (中间表示)                              (不变)
//! ```
//!
//! 两处接缝：**解析层**（换格式）与**加载器层**（换加载器）。
//! 其余三段对所有格式都一样，不要为了复用去改它们。

pub mod loader;
pub mod mrpack;
pub mod spec;

use anyhow::{bail, Context, Result};
use std::fs::File;
use std::io::Read;
use std::path::Path;

use crate::download::DownloadOpts;
use crate::version::Job;
use spec::PackSpec;

/// CurseForge 格式的标志文件
const CF_MANIFEST: &str = "manifest.json";

pub async fn install(pack: &Path, root: &Path, opts: &DownloadOpts) -> Result<()> {
    println!("[*] 读取 {}", pack.display());

    let file = File::open(pack).with_context(|| format!("打不开整合包: {}", pack.display()))?;
    let mut zip = zip::ZipArchive::new(file).context("不是有效的 zip")?;

    // ① 解析
    let spec = parse(&mut zip)?;

    // ② 支持性检查必须在**任何下载之前** —— 否则下完几个 GB 才说「不支持」
    spec.loader.check_supported()?;
    print_summary(&spec);

    // ③ 原版
    crate::vanilla::install(&spec.mc, root, opts).await?;

    // ④ 加载器
    let inst_id = loader::install(&spec.loader, &spec.mc, root, opts).await?;
    let inst = root.join("versions").join(&inst_id);
    tokio::fs::create_dir_all(&inst).await?;

    // ⑤ 文件（按 path 落到实例目录，即版本隔离布局）
    let jobs: Vec<Job> = spec
        .files
        .iter()
        .map(|e| Job {
            url: e.url.clone(),
            path: e.path.clone(),
            sha1: e.sha1.clone(),
            size: e.size,
            mirrors: Vec::new(),
        })
        .collect();
    crate::download::run(&jobs, &inst, opts).await?;

    // ⑥ 覆盖层：解压可能很重，扔到 blocking 线程池，别占着 async worker
    let prefixes = spec.override_prefixes.clone();
    let inst_for_extract = inst.clone();
    let copied = tokio::task::spawn_blocking(move || {
        extract_overrides(&mut zip, &prefixes, &inst_for_extract)
    })
    .await
    .context("覆盖层解压任务失败")??;
    println!("[+] overrides: {copied} 个文件");

    println!("[+] 完成 -> {}", inst.display());
    println!("[i] 把启动器的游戏目录指向 {} 即可。", root.display());
    Ok(())
}

/// 按**内容**判定格式，不靠扩展名（`.mrpack` 本质就是 zip，改个名也一样）。
fn parse(zip: &mut zip::ZipArchive<File>) -> Result<PackSpec> {
    if mrpack::detect(zip) {
        return mrpack::parse_zip(zip);
    }
    if zip.by_name(CF_MANIFEST).is_ok() {
        bail!(
            "这是 CurseForge 格式的整合包（含 {CF_MANIFEST}），暂不支持。\
             目前只认 Modrinth（.mrpack）。"
        );
    }
    bail!("认不出整合包格式：归档里既没有 {}，也没有 {CF_MANIFEST}", mrpack::INDEX_NAME)
}

fn print_summary(s: &PackSpec) {
    println!("[*] 包名  : {}", s.name);
    println!("[*] MC    : {}", s.mc);
    println!("[*] 加载器: {}", s.loader.label());
    println!("[*] 文件  : {} 个", s.files.len());
    if s.skipped_unsupported > 0 {
        println!("[*] 跳过 client=unsupported 的 {} 个", s.skipped_unsupported);
    }
}

/// 把归档里匹配 `prefixes` 的条目原样铺到实例目录，返回文件数。
fn extract_overrides(
    zip: &mut zip::ZipArchive<File>,
    prefixes: &[String],
    inst: &Path,
) -> Result<usize> {
    let mut copied = 0usize;
    for i in 0..zip.len() {
        let mut entry = zip.by_index(i)?;
        let name = entry.name().to_string();
        if name.ends_with('/') {
            continue;
        }
        let Some(rel) = prefixes
            .iter()
            .find_map(|p| name.strip_prefix(p.as_str()))
        else {
            continue;
        };
        if rel.is_empty() {
            continue;
        }
        let dst = inst.join(rel.replace('/', std::path::MAIN_SEPARATOR_STR));
        if let Some(parent) = dst.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let mut buf = Vec::new();
        entry.read_to_end(&mut buf)?;
        std::fs::write(&dst, buf)?;
        copied += 1;
    }
    Ok(copied)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    /// 造一个内存 zip 落盘，返回路径
    fn make_zip(tag: &str, entries: &[(&str, &str)]) -> std::path::PathBuf {
        let mut p = std::env::temp_dir();
        p.push(format!("mcd-pack-test-{tag}-{}.zip", std::process::id()));
        let f = File::create(&p).unwrap();
        let mut w = zip::ZipWriter::new(f);
        let opt: zip::write::SimpleFileOptions = Default::default();
        for (name, body) in entries {
            w.start_file(*name, opt).unwrap();
            w.write_all(body.as_bytes()).unwrap();
        }
        w.finish().unwrap();
        p
    }

    #[test]
    fn detects_modrinth() {
        let p = make_zip("mr", &[(mrpack::INDEX_NAME, r#"{"dependencies":{"minecraft":"1.21.1"}}"#)]);
        let mut z = zip::ZipArchive::new(File::open(&p).unwrap()).unwrap();
        assert!(mrpack::detect(&mut z));
        let spec = parse(&mut z).unwrap();
        assert_eq!(spec.mc, "1.21.1");
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn curseforge_gets_explicit_message() {
        let p = make_zip("cf", &[(CF_MANIFEST, r#"{"minecraft":{}}"#)]);
        let mut z = zip::ZipArchive::new(File::open(&p).unwrap()).unwrap();
        let e = parse(&mut z).unwrap_err().to_string();
        assert!(e.contains("CurseForge"), "{e}");
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn unknown_archive_is_rejected() {
        let p = make_zip("unk", &[("readme.txt", "hello")]);
        let mut z = zip::ZipArchive::new(File::open(&p).unwrap()).unwrap();
        let e = parse(&mut z).unwrap_err().to_string();
        assert!(e.contains("认不出"), "{e}");
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn overrides_are_flattened_into_instance_dir() {
        let p = make_zip(
            "ov",
            &[
                (mrpack::INDEX_NAME, r#"{"dependencies":{"minecraft":"1.21.1"}}"#),
                ("overrides/config/a.toml", "x=1"),
                ("client-overrides/options.txt", "fov:90"),
                ("mods/not-an-override.jar", "ignored"),
            ],
        );
        let mut z = zip::ZipArchive::new(File::open(&p).unwrap()).unwrap();
        let spec = parse(&mut z).unwrap();

        let inst = std::env::temp_dir().join(format!("mcd-inst-{}", std::process::id()));
        std::fs::create_dir_all(&inst).unwrap();
        let n = extract_overrides(&mut z, &spec.override_prefixes, &inst).unwrap();

        assert_eq!(n, 2);
        assert_eq!(std::fs::read_to_string(inst.join("config").join("a.toml")).unwrap(), "x=1");
        assert_eq!(std::fs::read_to_string(inst.join("options.txt")).unwrap(), "fov:90");
        assert!(!inst.join("mods").exists(), "非覆盖层的条目不该被解出来");

        let _ = std::fs::remove_file(&p);
        let _ = std::fs::remove_file(inst.join("options.txt"));
        let _ = std::fs::remove_file(inst.join("config").join("a.toml"));
        let _ = std::fs::remove_dir(inst.join("config"));
        let _ = std::fs::remove_dir(&inst);
    }
}
