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

use anyhow::{Context, Result, bail};
use std::fs::File;
use std::io::Read;
use std::path::Path;

use crate::curseforge;
use crate::download::DownloadOpts;
use crate::version::{Job, SizeHint};
use spec::PackSpec;

pub async fn install(pack: &Path, root: &Path, opts: &DownloadOpts) -> Result<()> {
    println!("[*] 读取 {}", pack.display());

    let file = File::open(pack).with_context(|| format!("打不开整合包: {}", pack.display()))?;
    let mut zip = zip::ZipArchive::new(file).context("不是有效的 zip")?;

    // ① 解析（按**内容**分流：Modrinth 直接给成品 spec；CurseForge 只给本地解析结果，
    //    取直链要发网络请求，留给 async 的这一层做）
    let spec = match parse(&mut zip)? {
        Parsed::Ready(spec) => spec,
        Parsed::CurseForge(m) => return install_curseforge(m, &mut zip, root, opts).await,
    };

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
            size: SizeHint::from_exact(e.size),
            mirrors: Vec::new(),
        })
        .collect();
    crate::download::run(&jobs, &inst, &crate::download::batch_opts(opts)).await?;

    // ⑥ 覆盖层：解压可能很重，扔到 blocking 线程池，别占着 async worker
    let prefixes = spec.override_prefixes.clone();
    let inst_for_extract = inst.clone();
    let (copied, skipped_during_extract) = tokio::task::spawn_blocking(move || {
        extract_overrides(&mut zip, &prefixes, &inst_for_extract)
    })
    .await
    .context("覆盖层解压任务失败")??;
    println!("[+] overrides: {copied} 个文件");
    if skipped_during_extract > 0 {
        println!(
            "[!] overrides: 跳过 {skipped_during_extract} 个路径不安全的条目（包里可能塞了 `..` / 绝对路径）"
        );
    }

    println!("[+] 完成 -> {}", inst.display());
    println!("[i] 把启动器的游戏目录指向 {} 即可。", root.display());
    Ok(())
}

/// 解析结果：Modrinth 能一路走到成品 `PackSpec`；CurseForge 只能到"清单 + id"，
/// 直链要问 API（网络 ≠ 这一层的职责）。
#[derive(Debug)]
enum Parsed {
    Ready(PackSpec),
    CurseForge(curseforge::CfManifest),
}

/// 按**内容**判定格式，不靠扩展名（`.mrpack` 本质就是 zip，改个名也一样）。
fn parse(zip: &mut zip::ZipArchive<File>) -> Result<Parsed> {
    if mrpack::detect(zip) {
        return Ok(Parsed::Ready(mrpack::parse_zip(zip)?));
    }
    if zip.by_name(curseforge::CF_MANIFEST).is_ok() {
        return Ok(Parsed::CurseForge(curseforge::parse_manifest(zip)?));
    }
    // 都没有：把**实际**根目录列出来，别让用户对着"认不出"猜
    //
    // zip 9 起 `file_names()` / `name()` 都返回 `Result`（非 UTF-8 名字不再被静默 lossy 掉），
    // 所以这里逐个取、取不到就跳过 —— 列表只用于报错文案，不必因一个坏名字整包失败。
    let mut tops: Vec<String> = Vec::new();
    for i in 0..zip.len() {
        let Ok(entry) = zip.by_index(i) else { continue };
        let Ok(name) = entry.name() else { continue };
        let top = name.split('/').next().unwrap_or("");
        if !top.is_empty() {
            tops.push(top.to_string());
        }
    }
    tops.sort();
    tops.dedup();
    bail!(
        "认不出整合包格式：根目录既没有 {} 也没有 {}；实际有: {:?}",
        mrpack::INDEX_NAME,
        curseforge::CF_MANIFEST,
        tops
    )
}

/// CurseForge 分支：**只做 mods + overrides + manual.txt**。
///
/// 为什么不像 Modrinth 那样装原版与加载器：
/// - §14.7 明确把 Forge/NeoForge 安装排除在本 feature 之外；
/// - 而且 manifest 的 `minecraft.version` **不可信**（实测 `IWS` 那个包写的是 `"26.3"`），
///   真拿它去 `vanilla::install` 会失败。⇒ 宁可不装，打印出来让用户自己装。
async fn install_curseforge(
    m: curseforge::CfManifest,
    zip: &mut zip::ZipArchive<File>,
    root: &Path,
    opts: &DownloadOpts,
) -> Result<()> {
    println!("[*] 格式  : CurseForge（manifest.json）");
    println!("[*] 包名  : {}", m.name);
    if crate::curseforge::looks_like_mc_version(&m.mc) {
        println!("[*] MC    : {}", m.mc);
    } else {
        println!(
            "[*] MC    : {}  (manifest 里写的是这个，不像 MC 版本号，可能不可靠)",
            m.mc
        );
    }
    println!(
        "[*] 加载器: {} —— 需自行安装（本次不装，见 DESIGN §14.7）",
        m.loader_label
    );
    println!("[*] 文件  : {} 个", m.refs.len());

    // key 检查必须**在任何网络请求之前**
    let key = curseforge::api_key_from_env()?;
    let client = crate::download::client(opts)?;
    let mut cache = std::collections::HashMap::new();
    let resolved = curseforge::resolve_all(&client, &key, &m.refs, &mut cache).await?;

    let (entries, manual) = curseforge::split(resolved);
    println!(
        "[*] 可自动: {} 个；需手动: {} 个",
        entries.len(),
        manual.len()
    );
    if let Some(p) = curseforge::write_manual(root, &manual)? {
        println!("[!] 手动下载清单 -> {}", p.display());
    }
    if entries.is_empty() {
        println!("[i] 没有可自动下载的文件，先按 manual.txt 手动补齐");
        return Ok(());
    }

    let jobs: Vec<Job> = entries
        .iter()
        .map(|e| Job {
            url: e.url.clone(),
            path: e.path.clone(),
            sha1: e.sha1.clone(),
            size: SizeHint::from_exact(e.size),
            mirrors: Vec::new(),
        })
        .collect();
    // ★ 完全复用现有下载器，不新写任何下载逻辑
    crate::download::run(&jobs, root, &crate::download::batch_opts(opts)).await?;

    // 覆盖层：CF 的 `overrides` 值实测都是 "overrides"
    let prefixes = vec!["overrides/".to_string()];
    let (copied, skipped) = extract_overrides(zip, &prefixes, root)?;
    println!("[+] overrides: {copied} 个文件");
    if skipped > 0 {
        // 跳过而不是整包失败：一个坏条目不该毁掉整次安装；但必须让人看见
        println!(
            "[!] overrides: 跳过 {skipped} 个路径不安全的条目（可能有人在包里塞了 `..` 或绝对路径）"
        );
    }
    println!("[+] 完成 -> {}", root.join("mods").display());
    Ok(())
}

fn print_summary(s: &PackSpec) {
    println!("[*] 包名  : {}", s.name);
    println!("[*] MC    : {}", s.mc);
    println!("[*] 加载器: {}", s.loader.label());
    println!("[*] 文件  : {} 个", s.files.len());
    if s.skipped_unsupported > 0 {
        println!(
            "[*] 跳过 client=unsupported 的 {} 个",
            s.skipped_unsupported
        );
    }
}

/// 把归档里匹配 `prefixes` 的条目原样铺到实例目录，返回文件数。
fn extract_overrides(
    zip: &mut zip::ZipArchive<File>,
    prefixes: &[String],
    inst: &Path,
) -> Result<(usize, usize)> {
    let mut copied = 0usize;
    let mut skipped = 0usize;
    for i in 0..zip.len() {
        let mut entry = zip.by_index(i)?;
        // zip 9 起 `name()` 返回 `Result`：非 UTF-8 名字不再被静默替换成 `?`。
        // 名字取不出来就没法校验落点 ⇒ 当成"可疑条目"跳过（与 `..` 同一处置）。
        let Ok(name_cow) = entry.name() else {
            eprintln!("[!] 跳过名字不是合法 UTF-8 的 zip 条目（第 {i} 个）");
            skipped += 1;
            continue;
        };
        let name = name_cow.to_string();
        if name.ends_with('/') {
            continue;
        }
        let Some(rel) = prefixes.iter().find_map(|p| name.strip_prefix(p.as_str())) else {
            continue;
        };
        if rel.is_empty() {
            continue;
        }
        // zip 条目名是**包作者写的**：`overrides/../../evil` 就能逃出实例目录。
        // `zip` crate 的 `enclosed_name()` 做的是同一件事，但它对 `C:` 之类返回 None
        // 而不给原因；这里过一遍 `pathsafe` 才能把"为什么拒"讲清楚，并统一 Windows 规则。
        let rel_path = match crate::pathsafe::check_rel(rel) {
            Ok(p) => p,
            Err(e) => {
                eprintln!("[!] 跳过可疑 override 条目 {name:?}: {e:#}");
                skipped += 1;
                continue;
            }
        };
        // 再用同一套判定确认落点仍在实例目录之下（符号链接那层也一并走过）
        let dst = crate::pathsafe::under_root(inst, &rel_path)
            .with_context(|| format!("override 条目落点不安全: {name:?}"))?;
        if let Some(parent) = dst.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let mut buf = Vec::new();
        entry.read_to_end(&mut buf)?;
        std::fs::write(&dst, buf)?;
        copied += 1;
    }
    Ok((copied, skipped))
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
        let p = make_zip(
            "mr",
            &[(
                mrpack::INDEX_NAME,
                r#"{"dependencies":{"minecraft":"1.21.1"}}"#,
            )],
        );
        let mut z = zip::ZipArchive::new(File::open(&p).unwrap()).unwrap();
        assert!(mrpack::detect(&mut z));
        let Parsed::Ready(spec) = parse(&mut z).unwrap() else {
            panic!("Modrinth 归档应走 Ready 分支");
        };
        assert_eq!(spec.mc, "1.21.1");
        let _ = std::fs::remove_file(&p);
    }

    /// 行为已变：CF 从"暂不支持"变成"走 CurseForge 分支"（DESIGN §14）。
    /// 这条测试**替换**原来的 `curseforge_gets_explicit_message`，不是简单删掉。
    #[test]
    fn curseforge_goes_to_its_own_branch() {
        let p = make_zip(
            "cf",
            &[(
                curseforge::CF_MANIFEST,
                r#"{"minecraft":{"version":"1.20.1","modLoaders":[{"id":"forge-47.4.10","primary":true}]},
                    "files":[{"projectID":1066065,"fileID":6752633,"required":true}]}"#,
            )],
        );
        let mut z = zip::ZipArchive::new(File::open(&p).unwrap()).unwrap();
        let Parsed::CurseForge(m) = parse(&mut z).unwrap() else {
            panic!("CF 归档应走 CurseForge 分支");
        };
        assert_eq!(m.mc, "1.20.1");
        assert_eq!(m.loader_raw, "forge-47.4.10");
        assert_eq!(m.refs.len(), 1);
        assert_eq!(m.refs[0].project_id, 1066065);
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn unknown_archive_is_rejected() {
        let p = make_zip("unk", &[("readme.txt", "hello")]);
        let mut z = zip::ZipArchive::new(File::open(&p).unwrap()).unwrap();
        let e = parse(&mut z).unwrap_err().to_string();
        assert!(e.contains("认不出"), "{e}");
        // 报错必须把**实际**根目录列出来，否则用户只能猜
        assert!(e.contains("readme.txt"), "应列出实际内容: {e}");
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn overrides_are_flattened_into_instance_dir() {
        let p = make_zip(
            "ov",
            &[
                (
                    mrpack::INDEX_NAME,
                    r#"{"dependencies":{"minecraft":"1.21.1"}}"#,
                ),
                ("overrides/config/a.toml", "x=1"),
                ("client-overrides/options.txt", "fov:90"),
                ("mods/not-an-override.jar", "ignored"),
            ],
        );
        let mut z = zip::ZipArchive::new(File::open(&p).unwrap()).unwrap();
        let Parsed::Ready(spec) = parse(&mut z).unwrap() else {
            panic!("Modrinth 归档应走 Ready 分支");
        };

        let inst = std::env::temp_dir().join(format!("mcd-inst-{}", std::process::id()));
        std::fs::create_dir_all(&inst).unwrap();
        let (n, skipped) = extract_overrides(&mut z, &spec.override_prefixes, &inst).unwrap();

        assert_eq!(n, 2);
        assert_eq!(skipped, 0, "合法条目不该被跳过");
        assert_eq!(
            std::fs::read_to_string(inst.join("config").join("a.toml")).unwrap(),
            "x=1"
        );
        assert_eq!(
            std::fs::read_to_string(inst.join("options.txt")).unwrap(),
            "fov:90"
        );
        assert!(!inst.join("mods").exists(), "非覆盖层的条目不该被解出来");

        let _ = std::fs::remove_file(&p);
        let _ = std::fs::remove_file(inst.join("options.txt"));
        let _ = std::fs::remove_file(inst.join("config").join("a.toml"));
        let _ = std::fs::remove_dir(inst.join("config"));
        let _ = std::fs::remove_dir(&inst);
    }

    /// 恶意 zip：条目名带 `..` 时**跳过并计数**，既不能写到实例目录之外，也不能整包失败。
    #[test]
    fn hostile_override_entry_is_skipped_and_counted() {
        let tag = format!("ov-evil-{}", std::process::id());
        let p = make_zip(
            &tag,
            &[
                (
                    mrpack::INDEX_NAME,
                    r#"{"dependencies":{"minecraft":"1.21.1"}}"#,
                ),
                ("overrides/../../escaped.txt", "pwned"),
                ("overrides/config/ok.toml", "x=1"),
            ],
        );
        let mut z = zip::ZipArchive::new(File::open(&p).unwrap()).unwrap();

        let inst = std::env::temp_dir().join(format!("mcd-inst-evil-{}", std::process::id()));
        std::fs::create_dir_all(&inst).unwrap();
        let (copied, skipped) =
            extract_overrides(&mut z, &["overrides/".to_string()], &inst).unwrap();

        assert_eq!(copied, 1, "合法条目照常落盘");
        assert_eq!(skipped, 1, "穿越条目必须被跳过并计数");
        let escaped = inst.parent().unwrap().join("escaped.txt");
        assert!(
            !escaped.exists(),
            "不许写到实例目录之外: {}",
            escaped.display()
        );
        assert!(inst.join("config").join("ok.toml").exists());

        let _ = std::fs::remove_file(&p);
        let _ = std::fs::remove_file(inst.join("config").join("ok.toml"));
        let _ = std::fs::remove_dir(inst.join("config"));
        let _ = std::fs::remove_dir(&inst);
    }

    /// zip 条目的另两种越界形态：剥掉前缀后是**绝对路径**（`overrides//abs`）与**带盘符**（`C:/x`）。
    ///
    /// 红队 2026-11-22 指出：此前只有 `overrides/../../escaped.txt` 一条单测，这两种没有任何覆盖。
    #[test]
    fn hostile_override_absolute_and_drive_are_skipped() {
        let tag = format!("ov-abs-{}", std::process::id());
        let p = make_zip(
            &tag,
            &[
                (
                    mrpack::INDEX_NAME,
                    r#"{"dependencies":{"minecraft":"1.21.1"}}"#,
                ),
                ("overrides//abs.txt", "pwned"),
                ("overrides/C:/drive.txt", "pwned"),
                ("overrides/ok/keep.toml", "x=1"),
            ],
        );
        let mut z = zip::ZipArchive::new(File::open(&p).unwrap()).unwrap();

        let inst = std::env::temp_dir().join(format!("mcd-inst-abs-{}", std::process::id()));
        std::fs::create_dir_all(&inst).unwrap();
        let (copied, skipped) =
            extract_overrides(&mut z, &["overrides/".to_string()], &inst).unwrap();

        assert_eq!(copied, 1, "只有 ok/keep.toml 该落盘");
        assert_eq!(skipped, 2, "绝对路径与盘符两条都要跳过并计数");
        assert!(inst.join("ok").join("keep.toml").exists());
        // 剥掉前缀后剩 `/abs.txt`：既不能落到盘根，也不能落到实例目录里叫 `abs.txt`
        assert!(!inst.join("abs.txt").exists(), "绝对路径条目不许落盘");

        let _ = std::fs::remove_file(&p);
        let _ = std::fs::remove_file(inst.join("ok").join("keep.toml"));
        let _ = std::fs::remove_dir(inst.join("ok"));
        let _ = std::fs::remove_dir(&inst);
    }
}
