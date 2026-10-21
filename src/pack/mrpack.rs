//! Modrinth 整合包（`.mrpack`，或结构相同的 `.zip`）解析。
//!
//! 只做「读归档里的 `modrinth.index.json` 并映射成 `PackSpec`」，
//! 不碰下载、不碰加载器安装 —— 那些是流水线的事。

use anyhow::{bail, Context, Result};
use serde::Deserialize;
use std::collections::HashMap;
use std::io::Read;

use super::spec::{Loader, PackEntry, PackSpec};

pub const INDEX_NAME: &str = "modrinth.index.json";

#[derive(Debug, Deserialize)]
struct Index {
    #[serde(default)]
    name: Option<String>,
    dependencies: HashMap<String, String>,
    #[serde(default)]
    files: Vec<FileEntry>,
}

#[derive(Debug, Deserialize)]
struct FileEntry {
    path: String,
    #[serde(default)]
    downloads: Vec<String>,
    #[serde(default)]
    hashes: HashMap<String, String>,
    #[serde(default)]
    env: Option<HashMap<String, String>>,
    /// Modrinth 规范里的字段名就是 `fileSize`
    #[serde(default, rename = "fileSize")]
    file_size: Option<u64>,
}

/// 该归档是不是 Modrinth 格式
pub fn detect(zip: &mut zip::ZipArchive<std::fs::File>) -> bool {
    zip.by_name(INDEX_NAME).is_ok()
}

pub fn parse_zip(zip: &mut zip::ZipArchive<std::fs::File>) -> Result<PackSpec> {
    let mut text = String::new();
    zip.by_name(INDEX_NAME)
        .context("不是 Modrinth 整合包：缺少 modrinth.index.json")?
        .read_to_string(&mut text)
        .context("读取 modrinth.index.json 失败")?;
    parse_index(&text)
}

/// JSON 文本 → `PackSpec`。抽成纯函数是为了能脱离 zip 单测。
pub fn parse_index(text: &str) -> Result<PackSpec> {
    let idx: Index = serde_json::from_str(text).context("解析 modrinth.index.json 失败")?;

    let mc = idx
        .dependencies
        .get("minecraft")
        .context("整合包未指定 minecraft 版本")?
        .clone();

    let loader = pick_loader(&idx.dependencies)?;

    let mut files = Vec::with_capacity(idx.files.len());
    let mut skipped = 0usize;
    for f in idx.files {
        // 客户端跑不了的（多为仅服务端 mod）直接排除
        if let Some(env) = &f.env {
            if env.get("client").is_some_and(|c| c == "unsupported") {
                skipped += 1;
                continue;
            }
        }
        let Some(url) = f.downloads.into_iter().next() else {
            // 没有下载地址的条目（规范允许）只能跳过
            continue;
        };
        files.push(PackEntry {
            path: f.path.replace('\\', "/"),
            url,
            sha1: f.hashes.get("sha1").cloned(),
            size: f.file_size,
        });
    }

    Ok(PackSpec::new(
        idx.name.unwrap_or_else(|| "(未命名)".to_string()),
        mc,
        loader,
        files,
        Vec::new(),
        skipped,
    ))
}

/// 按 `dependencies` 里的键判定加载器。
///
/// 这里只做**识别**，不做支持性判断 —— 那是 `Loader::check_supported` 的事，
/// 调用方会在下载任何东西之前先问它。
fn pick_loader(deps: &HashMap<String, String>) -> Result<Loader> {
    if let Some(v) = deps.get("fabric-loader") {
        return Ok(Loader::Fabric { loader: v.clone() });
    }
    if let Some(v) = deps.get("forge") {
        return Ok(Loader::Forge { version: v.clone() });
    }
    if let Some(v) = deps.get("neoforge") {
        return Ok(Loader::NeoForge { version: v.clone() });
    }
    if let Some(v) = deps.get("quilt-loader") {
        bail!("暂不支持 Quilt（整合包要求 {v}）。当前只做原版与 Fabric。");
    }
    // 只有 minecraft（或干脆什么都没有）⇒ 纯原版
    Ok(Loader::Vanilla)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn idx(deps: &str, files: &str) -> String {
        format!(r#"{{"dependencies":{deps},"files":{files}}}"#)
    }

    #[test]
    fn fabric_pack_parses() {
        let text = idx(
            r#"{"minecraft":"1.21.1","fabric-loader":"0.16.5"}"#,
            r#"[{"path":"mods/a.jar","downloads":["https://cdn/a.jar"],
                 "hashes":{"sha1":"abc"},"fileSize":1024,
                 "env":{"client":"required","server":"required"}}]"#,
        );
        let s = parse_index(&text).unwrap();
        assert_eq!(s.mc, "1.21.1");
        assert_eq!(
            s.loader,
            Loader::Fabric {
                loader: "0.16.5".into()
            }
        );
        assert_eq!(s.files.len(), 1);
        assert_eq!(s.files[0].path, "mods/a.jar");
        assert_eq!(s.files[0].sha1.as_deref(), Some("abc"));
        assert_eq!(s.files[0].size, Some(1024));
        assert_eq!(s.name, "(未命名)");
        assert_eq!(s.skipped_unsupported, 0);
    }

    #[test]
    fn client_unsupported_is_skipped() {
        let text = idx(
            r#"{"minecraft":"1.21.1","fabric-loader":"0.16.5"}"#,
            r#"[{"path":"mods/server-only.jar","downloads":["https://cdn/s.jar"],
                 "env":{"client":"unsupported","server":"required"}},
                {"path":"mods/ok.jar","downloads":["https://cdn/o.jar"]}]"#,
        );
        let s = parse_index(&text).unwrap();
        assert_eq!(s.files.len(), 1);
        assert_eq!(s.files[0].path, "mods/ok.jar");
        assert_eq!(s.skipped_unsupported, 1);
    }

    #[test]
    fn entry_without_downloads_is_dropped() {
        let text = idx(
            r#"{"minecraft":"1.21.1"}"#,
            r#"[{"path":"mods/nourl.jar","downloads":[]}]"#,
        );
        let s = parse_index(&text).unwrap();
        assert!(s.files.is_empty());
    }

    #[test]
    fn forge_pack_is_recognized_but_not_supported() {
        let text = idx(r#"{"minecraft":"1.20.1","forge":"47.2.0"}"#, "[]");
        let s = parse_index(&text).unwrap();
        assert_eq!(
            s.loader,
            Loader::Forge {
                version: "47.2.0".into()
            }
        );
        // 识别出来 ≠ 能装；这条边界必须清晰
        assert!(s.loader.check_supported().is_err());
    }

    #[test]
    fn neoforge_pack_is_recognized() {
        let text = idx(r#"{"minecraft":"1.21.1","neoforge":"21.1.0"}"#, "[]");
        let s = parse_index(&text).unwrap();
        assert_eq!(s.loader.label(), "NeoForge 21.1.0");
    }

    #[test]
    fn quilt_is_rejected_at_parse_time() {
        let text = idx(r#"{"minecraft":"1.21.1","quilt-loader":"0.27.0"}"#, "[]");
        let e = parse_index(&text).unwrap_err().to_string();
        assert!(e.contains("Quilt"), "{e}");
    }

    #[test]
    fn vanilla_pack_has_no_loader() {
        let text = idx(r#"{"minecraft":"1.21.1"}"#, "[]");
        assert_eq!(parse_index(&text).unwrap().loader, Loader::Vanilla);
    }

    #[test]
    fn missing_minecraft_is_error() {
        let text = idx(r#"{"fabric-loader":"0.16.5"}"#, "[]");
        assert!(parse_index(&text).is_err());
    }

    #[test]
    fn backslashes_in_path_are_normalized() {
        let text = idx(
            r#"{"minecraft":"1.21.1"}"#,
            r#"[{"path":"mods\\sub\\a.jar","downloads":["https://cdn/a.jar"]}]"#,
        );
        assert_eq!(parse_index(&text).unwrap().files[0].path, "mods/sub/a.jar");
    }

    #[test]
    fn malformed_json_is_error() {
        assert!(parse_index("{not json").is_err());
    }
}
