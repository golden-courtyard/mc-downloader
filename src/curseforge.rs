//! CurseForge 整合包支持（DESIGN §14）。
//!
//! 本模块只做三件事：**解析 manifest** / **调 API 换直链** / **分拣可分发与禁分发**。
//! 真正的下载完全交给 `version::Job` + `download::run`（§14.3），不在这里写任何下载逻辑。
//!
//! 与 Modrinth 的差别（决定了本模块的形状）：
//! - CF manifest 只有 `{projectID, fileID}`，**没有 path / 类别 / sha1** —— 那些都得问 API；
//! - 因此解析是纯本地的，**取直链是异步的** ⇒ 调用方（`pack::install`）必须分两段。
//!
//! 两条**不可信**的 manifest 字段（实测，见 §14）：
//! - `minecraft.version` 可能是整合包自己的版本号（`"26.3"`），用 [`looks_like_mc_version`] 判；
//! - `files[].required` 在三个真实包里全是 `true` ⇒ 照实记录，但**不做任何过滤**。

use anyhow::{Context, Result, bail};
use serde::Deserialize;
use std::collections::HashMap;
use std::io::Read;
use std::path::Path;

use crate::pack::spec::PackEntry;

/// CF 格式的标志文件
pub const CF_MANIFEST: &str = "manifest.json";
pub const API_BASE: &str = "https://api.curseforge.com";
/// 一次 `POST /v1/mods` 最多带多少个 id（§14.3）
pub const BATCH_SIZE: usize = 50;
/// 批间间隔：免费 key 有配额（§14.3）
pub const BATCH_SLEEP_MS: u64 = 400;
/// `FileHash.algo` 里 sha1 的编号
const HASH_SHA1: u32 = 1;
/// 环境变量名（唯一通道，§14.1）
pub const ENV_KEY: &str = "CURSEFORGE_API_KEY";
/// mod 落盘目录。CF manifest 不含类别信息 ⇒ 一律落这里（§14 的取舍）
pub const MODS_DIR: &str = "mods";

// ── 解析结果 ────────────────────────────────────────────────────────────────

/// manifest 里的一条引用（**只有 id，其它一律要问 API**）
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CfRef {
    pub project_id: u32,
    pub file_id: u32,
    /// 照实记录，**不用于过滤**（真实包里全是 true）
    pub required: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CfManifest {
    pub name: String,
    /// 原样保留，可能是整合包版本号而不是 MC 版本
    pub mc: String,
    /// 原样保留，形如 `forge-47.4.10` / `neoforge-21.1.251` / `fabric-0.20.0`
    pub loader_raw: String,
    pub refs: Vec<CfRef>,
}

// ── serde 侧（只在模块内部用）──────────────────────────────────────────────

#[derive(Debug, Deserialize)]
struct RawManifest {
    minecraft: RawMc,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    files: Vec<RawFile>,
}

#[derive(Debug, Deserialize)]
struct RawMc {
    version: String,
    #[serde(default, rename = "modLoaders")]
    mod_loaders: Vec<RawLoader>,
}

#[derive(Debug, Deserialize)]
struct RawLoader {
    id: String,
    #[serde(default)]
    primary: bool,
}

#[derive(Debug, Deserialize)]
struct RawFile {
    #[serde(rename = "projectID")]
    project_id: u32,
    #[serde(rename = "fileID")]
    file_id: u32,
    #[serde(default)]
    required: bool,
}

// ── 纯函数（全部可单测，不碰网络/磁盘）─────────────────────────────────────

/// 这个字符串像不像 MC 版本号。
///
/// 判据故意窄：`1.` 开头 + 至少两段纯数字。
/// *理由*：实测 `IWS 26.3 Fabric-v2.zip` 的 `minecraft.version` 是 `"26.3"`（整合包自己的版本号）。
/// 窄判据的代价是"未来 MC 2.0 会被判成不像" —— 那时改这一行即可，
/// 而**误判成"像"会去下一个不存在的版本**，代价更大。
pub fn looks_like_mc_version(v: &str) -> bool {
    if !v.starts_with("1.") {
        return false;
    }
    let mut it = v.split('.');
    matches!(
        (it.next(), it.next()),
        (Some(a), Some(b)) if a.parse::<u32>().is_ok() && b.parse::<u32>().is_ok()
    )
}

/// 从 manifest 文本解析（**纯函数**，便于用字符串做单测）。
pub fn parse_manifest_str(text: &str) -> Result<CfManifest> {
    let raw: RawManifest =
        serde_json::from_str(text).with_context(|| format!("{CF_MANIFEST} 不是合法 JSON"))?;

    // primary 优先；没有 primary 就用第一个（真实包都有 primary，但别假设）
    let loader_raw = raw
        .minecraft
        .mod_loaders
        .iter()
        .find(|l| l.primary)
        .or_else(|| raw.minecraft.mod_loaders.first())
        .map(|l| l.id.clone())
        .unwrap_or_else(|| "(manifest 未指定)".to_string());

    if raw.files.is_empty() {
        bail!("{CF_MANIFEST} 里没有任何 files 条目");
    }

    let refs = raw
        .files
        .into_iter()
        .map(|f| CfRef {
            project_id: f.project_id,
            file_id: f.file_id,
            required: f.required,
        })
        .collect();

    Ok(CfManifest {
        name: raw.name.unwrap_or_else(|| "(未命名)".to_string()),
        mc: raw.minecraft.version,
        loader_raw,
        refs,
    })
}

/// 从归档里读 manifest（薄封装，真正的解析在 [`parse_manifest_str`]）
pub fn parse_manifest(zip: &mut zip::ZipArchive<std::fs::File>) -> Result<CfManifest> {
    let mut text = String::new();
    zip.by_name(CF_MANIFEST)
        .with_context(|| format!("归档里没有 {CF_MANIFEST}"))?
        .read_to_string(&mut text)
        .with_context(|| format!("读 {CF_MANIFEST} 失败"))?;
    parse_manifest_str(&text)
}

/// 从环境变量取 key。**缺失或空串都必须硬报错** —— 空串拿去调 API 只会得到莫名的 403。
pub fn api_key_from_env() -> Result<String> {
    match std::env::var(ENV_KEY) {
        Ok(k) if !k.trim().is_empty() => Ok(k.trim().to_string()),
        _ => bail!(
            "错误：CurseForge 整合包需要 {ENV_KEY} 环境变量\n\
             \x20     获取：https://console.curseforge.com/\n\
             \x20     设置：setx {ENV_KEY} \"<key>\"   然后重开终端"
        ),
    }
}

/// 禁分发条目的人工下载页
pub fn page_url(slug: &str, file_id: u32) -> String {
    format!("https://www.curseforge.com/minecraft/mc-mods/{slug}/files/{file_id}")
}

/// API 解析后的一个文件
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CfResolved {
    pub name: String,
    pub slug: String,
    pub file_name: String,
    /// `None` ⇒ 拿不到直链（禁分发或查不到）
    pub url: Option<String>,
    pub sha1: Option<String>,
    pub size: Option<u64>,
    pub project_id: u32,
    pub file_id: u32,
    /// 不可下载时的原因（写进 manual.txt）；可下载时为 `None`
    pub manual_reason: Option<String>,
}

impl CfResolved {
    pub fn is_distributable(&self) -> bool {
        self.url.is_some()
    }
}

/// 分拣：可下载的变成 [`PackEntry`]（路径 `mods/<fileName>`），其余进人工清单。
pub fn split(resolved: Vec<CfResolved>) -> (Vec<PackEntry>, Vec<CfResolved>) {
    let mut entries = Vec::new();
    let mut manual = Vec::new();
    for r in resolved {
        match r.url.clone() {
            Some(url) => entries.push(PackEntry {
                path: format!("{MODS_DIR}/{}", r.file_name),
                url,
                sha1: r.sha1.clone(),
                size: r.size,
            }),
            None => manual.push(r),
        }
    }
    (entries, manual)
}

/// 把禁分发清单写到 `<dir>/manual.txt`；返回文件路径。清单为空时不写文件（返回 None）。
pub fn write_manual(dir: &Path, items: &[CfResolved]) -> Result<Option<std::path::PathBuf>> {
    if items.is_empty() {
        return Ok(None);
    }
    std::fs::create_dir_all(dir).with_context(|| format!("建目录失败: {}", dir.display()))?;
    let mut s = String::new();
    s.push_str(&format!(
        "以下 {} 个 mod 的作者禁止第三方分发，需手动下载：\n",
        items.len()
    ));
    for (i, m) in items.iter().enumerate() {
        let why = m.manual_reason.as_deref().unwrap_or("无法获取直链");
        s.push_str(&format!(
            "  {}. {:<24} {}   [{}]\n",
            i + 1,
            m.name,
            page_url(&m.slug, m.file_id),
            why
        ));
    }
    s.push_str("\n放到位后重跑本命令即可（已下载的会跳过）。\n");
    s.push_str("（若某文件本属资源包/光影，请自行移到 resourcepacks/ 或 shaderpacks/。）\n");

    let path = dir.join("manual.txt");
    std::fs::write(&path, s).with_context(|| format!("写 {} 失败", path.display()))?;
    Ok(Some(path))
}

/// 遇到这个 HTTP 状态码该怎么办（**纯函数，重试策略可单测**）
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RetryAction {
    /// 退避一次再试
    Retry,
    /// 重试无意义，直接报错
    GiveUp,
}

/// 依据 §六.25「失败重试一次，还失败就换思路」：**只有 429 与 5xx 值得重试**。
/// 403 是 key 无效，重试一万次也一样；4xx 其它同理。
pub fn retry_decision(status: u16) -> RetryAction {
    match status {
        429 => RetryAction::Retry,
        500..=599 => RetryAction::Retry,
        _ => RetryAction::GiveUp,
    }
}

/// 429 的退避毫秒数：有 `Retry-After` 就用它，但夹在 [200, 10_000] 之间，
/// 避免服务端给一个荒唐值把命令卡死。
pub fn backoff_ms(retry_after_secs: Option<u64>) -> u64 {
    match retry_after_secs {
        Some(s) => (s * 1000).clamp(200, 10_000),
        None => 2_000,
    }
}

// ── 网络层 ──────────────────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
struct ModsResp {
    data: Vec<CfMod>,
}

#[derive(Debug, Deserialize)]
struct CfMod {
    id: u32,
    name: String,
    slug: String,
    /// `false` ⇒ 作者禁分发；`null`（字段缺失）⇒ **未知**，仍去问 file 接口
    #[serde(default, rename = "allowModDistribution")]
    allow_mod_distribution: Option<bool>,
}

#[derive(Debug, Deserialize)]
struct FileResp {
    data: CfFile,
}

#[derive(Debug, Deserialize)]
struct CfFile {
    #[serde(rename = "fileName")]
    file_name: String,
    #[serde(default, rename = "downloadUrl")]
    download_url: Option<String>,
    #[serde(default, rename = "fileLength")]
    file_length: Option<u64>,
    #[serde(default)]
    hashes: Vec<CfHash>,
}

#[derive(Debug, Deserialize)]
struct CfHash {
    algo: u32,
    value: String,
}

impl CfFile {
    fn sha1(&self) -> Option<String> {
        self.hashes
            .iter()
            .find(|h| h.algo == HASH_SHA1)
            .map(|h| h.value.clone())
    }
}

/// 一次 API 调用的结果
enum ApiOutcome {
    Ok(serde_json::Value),
    /// 404：**不是错误**（mod/文件被作者删了），对该条进人工清单
    NotFound,
}

fn api_error(what: &str, status: u16, body: &str) -> anyhow::Error {
    let hint = if status == 403 {
        " —— key 无效或无权限，检查 CURSEFORGE_API_KEY"
    } else {
        ""
    };
    let snippet: String = body.chars().take(200).collect();
    anyhow::anyhow!("CurseForge {what} 返回 {status}{hint}；响应片段: {snippet}")
}

/// 发一次请求；429/5xx **重试一次**，其余直接报错（§六.25）。
async fn call(
    client: &reqwest::Client,
    what: &str,
    url: &str,
    key: &str,
    body: Option<&serde_json::Value>,
) -> Result<ApiOutcome> {
    let mut attempt = 0u32;
    loop {
        attempt += 1;
        let mut req = client.get(url);
        if let Some(b) = body {
            req = client.post(url).json(b);
        }
        let resp = req
            .header("x-api-key", key)
            .header("accept", "application/json")
            .send()
            .await;

        let resp = match resp {
            Ok(r) => r,
            Err(e) => {
                // 连接错/超时：重试一次，再失败就报
                if attempt < 2 {
                    tokio::time::sleep(std::time::Duration::from_millis(BATCH_SLEEP_MS)).await;
                    continue;
                }
                return Err(anyhow::Error::new(e).context(format!(
                    "CurseForge {what} 网络失败（已重试 1 次）: {url}"
                )));
            }
        };

        let status = resp.status();
        if status.is_success() {
            let v: serde_json::Value = resp
                .json()
                .await
                .with_context(|| format!("CurseForge {what} 返回的不是合法 JSON"))?;
            return Ok(ApiOutcome::Ok(v));
        }
        if status.as_u16() == 404 {
            return Ok(ApiOutcome::NotFound);
        }

        let code = status.as_u16();
        let retry_after = resp
            .headers()
            .get("retry-after")
            .and_then(|v| v.to_str().ok())
            .and_then(|s| s.trim().parse::<u64>().ok());
        let text = resp.text().await.unwrap_or_default();

        if retry_decision(code) == RetryAction::Retry && attempt < 2 {
            let ms = backoff_ms(retry_after);
            eprintln!("[!] CurseForge {what} 返回 {code}，{ms}ms 后重试一次");
            tokio::time::sleep(std::time::Duration::from_millis(ms)).await;
            continue;
        }
        return Err(api_error(what, code, &text));
    }
}

/// 解析全部引用：批量拿 mod 元信息 → 逐条拿直链。
///
/// `cache` 是**单次运行内的内存缓存**（§14.5）：同一个 mod 可能出现在多个包里。
pub async fn resolve_all(
    client: &reqwest::Client,
    key: &str,
    refs: &[CfRef],
    cache: &mut HashMap<(u32, u32), CfResolved>,
) -> Result<Vec<CfResolved>> {
    // ① 批量取 mod 元信息（去重、每批 BATCH_SIZE 个）
    let mut ids: Vec<u32> = refs.iter().map(|r| r.project_id).collect();
    ids.sort_unstable();
    ids.dedup();

    let mut mods: HashMap<u32, CfMod> = HashMap::new();
    let batches: Vec<&[u32]> = ids.chunks(BATCH_SIZE).collect();
    for (i, batch) in batches.iter().enumerate() {
        let body = serde_json::json!({ "modIds": batch, "filterPcOnly": false });
        let what = format!("POST /v1/mods（批 {}/{}）", i + 1, batches.len());
        let url = format!("{API_BASE}/v1/mods");
        match call(client, &what, &url, key, Some(&body)).await? {
            ApiOutcome::Ok(v) => {
                let resp: ModsResp = serde_json::from_value(v)
                    .with_context(|| format!("{what} 的响应结构不认识"))?;
                for m in resp.data {
                    mods.insert(m.id, m);
                }
            }
            ApiOutcome::NotFound => bail!("{what} 返回 404 —— 不是预期的响应"),
        }
        // 批间限速（免费 key 有配额）
        if i + 1 < batches.len() {
            tokio::time::sleep(std::time::Duration::from_millis(BATCH_SLEEP_MS)).await;
        }
    }

    // ② 逐条拿文件信息
    let mut out = Vec::with_capacity(refs.len());
    for r in refs {
        let k = (r.project_id, r.file_id);
        if let Some(hit) = cache.get(&k) {
            out.push(hit.clone());
            continue;
        }

        let meta = mods.get(&r.project_id);
        let name = meta.map(|m| m.name.clone()).unwrap_or_else(|| format!("projectID {}", r.project_id));
        let slug = meta.map(|m| m.slug.clone()).unwrap_or_default();

        // 作者明确禁分发 ⇒ **不调 file 接口**（§14.4）
        if meta.and_then(|m| m.allow_mod_distribution) == Some(false) {
            let resolved = CfResolved {
                name,
                slug,
                file_name: String::new(),
                url: None,
                sha1: None,
                size: None,
                project_id: r.project_id,
                file_id: r.file_id,
                manual_reason: Some("作者禁止第三方分发".into()),
            };
            cache.insert(k, resolved.clone());
            out.push(resolved);
            continue;
        }

        let what = format!("GET /v1/mods/{}/files/{}", r.project_id, r.file_id);
        let url = format!("{API_BASE}/v1/mods/{}/files/{}", r.project_id, r.file_id);
        let resolved = match call(client, &what, &url, key, None).await? {
            ApiOutcome::NotFound => CfResolved {
                name,
                slug,
                file_name: String::new(),
                url: None,
                sha1: None,
                size: None,
                project_id: r.project_id,
                file_id: r.file_id,
                manual_reason: Some("API 查不到该文件（可能已被作者删除）".into()),
            },
            ApiOutcome::Ok(v) => {
                let f: FileResp = serde_json::from_value(v)
                    .with_context(|| format!("{what} 的响应结构不认识"))?;
                let f = f.data;
                let url = f.download_url.clone();
                CfResolved {
                    name,
                    slug,
                    file_name: f.file_name.clone(),
                    url: url.clone(),
                    sha1: f.sha1(),
                    size: f.file_length,
                    project_id: r.project_id,
                    file_id: r.file_id,
                    manual_reason: url
                        .is_none()
                        .then(|| "作者禁止第三方分发（API 未给直链）".to_string()),
                }
            }
        };
        cache.insert(k, resolved.clone());
        out.push(resolved);
    }

    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn manifest(mc: &str, loaders: &str, files: &str) -> String {
        format!(r#"{{"minecraft":{{"version":"{mc}","modLoaders":{loaders}}},"files":{files}}}"#)
    }

    #[test]
    fn real_shape_parses() {
        // 形状取自 challenge of death.zip（实测）
        let text = manifest(
            "1.20.1",
            r#"[{"id":"forge-47.4.10","primary":true}]"#,
            r#"[{"projectID":1066065,"fileID":6752633,"required":true,"isLocked":false},
                {"projectID":1,"fileID":2,"required":true}]"#,
        );
        let m = parse_manifest_str(&text).unwrap();
        assert_eq!(m.mc, "1.20.1");
        assert_eq!(m.loader_raw, "forge-47.4.10");
        assert_eq!(m.name, "(未命名)");
        assert_eq!(m.refs.len(), 2);
        assert_eq!(
            m.refs[0],
            CfRef {
                project_id: 1066065,
                file_id: 6752633,
                required: true
            }
        );
    }

    #[test]
    fn primary_loader_wins() {
        let text = manifest(
            "1.21.1",
            r#"[{"id":"fabric-0.20.0"},{"id":"neoforge-21.1.251","primary":true}]"#,
            r#"[{"projectID":1,"fileID":2}]"#,
        );
        assert_eq!(parse_manifest_str(&text).unwrap().loader_raw, "neoforge-21.1.251");
    }

    #[test]
    fn loader_absent_is_not_a_panic() {
        let text = manifest("1.20.1", "[]", r#"[{"projectID":1,"fileID":2}]"#);
        assert_eq!(
            parse_manifest_str(&text).unwrap().loader_raw,
            "(manifest 未指定)"
        );
    }

    #[test]
    fn empty_files_is_an_error() {
        let text = manifest("1.20.1", "[]", "[]");
        let err = parse_manifest_str(&text).unwrap_err().to_string();
        assert!(err.contains("没有任何 files"), "{err}");
    }

    #[test]
    fn invalid_json_is_an_error() {
        assert!(parse_manifest_str("{not json").is_err());
    }

    #[test]
    fn iws_version_is_flagged() {
        // 真实案例：IWS 26.3 Fabric-v2.zip 的 minecraft.version == "26.3"
        assert!(!looks_like_mc_version("26.3"));
        assert!(looks_like_mc_version("1.20.1"));
        assert!(looks_like_mc_version("1.8.9"));
        assert!(looks_like_mc_version("1.21"));
        assert!(!looks_like_mc_version("1"));
        assert!(!looks_like_mc_version("1.x"));
        assert!(!looks_like_mc_version(""));
    }

    fn resolved(url: Option<&str>, name: &str, file: &str) -> CfResolved {
        CfResolved {
            name: name.into(),
            slug: "some-mod".into(),
            file_name: file.into(),
            url: url.map(Into::into),
            sha1: Some("abc".into()),
            size: Some(7),
            project_id: 1,
            file_id: 22,
            manual_reason: url.is_none().then(|| "作者禁止第三方分发".to_string()),
        }
    }

    #[test]
    fn split_puts_mods_under_mods_dir() {
        let (entries, manual) = split(vec![
            resolved(Some("https://cdn/x.jar"), "X", "x.jar"),
            resolved(None, "Playmate", "p.jar"),
        ]);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].path, "mods/x.jar");
        assert_eq!(entries[0].url, "https://cdn/x.jar");
        assert_eq!(entries[0].sha1.as_deref(), Some("abc"));
        assert_eq!(manual.len(), 1);
        assert_eq!(manual[0].name, "Playmate");
    }

    #[test]
    fn page_url_shape() {
        assert_eq!(
            page_url("playmate", 6752633),
            "https://www.curseforge.com/minecraft/mc-mods/playmate/files/6752633"
        );
    }

    #[test]
    fn retry_only_for_429_and_5xx() {
        assert_eq!(retry_decision(429), RetryAction::Retry);
        assert_eq!(retry_decision(503), RetryAction::Retry);
        assert_eq!(retry_decision(500), RetryAction::Retry);
        assert_eq!(retry_decision(403), RetryAction::GiveUp);
        assert_eq!(retry_decision(404), RetryAction::GiveUp);
        assert_eq!(retry_decision(400), RetryAction::GiveUp);
        assert_eq!(retry_decision(200), RetryAction::GiveUp);
    }

    #[test]
    fn sha1_picked_by_algo_not_position() {
        // hashes 是数组，sha1 的 algo == 1；顺序不保证，必须按 algo 找
        let f: CfFile = serde_json::from_str(
            r#"{"fileName":"a.jar","downloadUrl":"https://cdn/a.jar",
                "fileLength":10,
                "hashes":[{"algo":2,"value":"md5xx"},{"algo":1,"value":"sha1xx"}]}"#,
        )
        .unwrap();
        assert_eq!(f.sha1().as_deref(), Some("sha1xx"));
        assert_eq!(f.file_length, Some(10));
    }

    #[test]
    fn sha1_absent_is_none_not_empty() {
        let f: CfFile =
            serde_json::from_str(r#"{"fileName":"a.jar","hashes":[{"algo":2,"value":"m"}]}"#)
                .unwrap();
        assert_eq!(f.sha1(), None);
        assert_eq!(f.download_url, None);
    }

    #[test]
    fn forbidden_error_hints_at_the_key() {
        let e = api_error("POST /v1/mods", 403, r#"{"error":"bad key"}"#).to_string();
        assert!(e.contains("403"), "{e}");
        assert!(e.contains("CURSEFORGE_API_KEY"), "403 必须指向 key: {e}");
        // 非 403 不该出现 key 提示，否则会把「服务端抖动」误导成「key 坏了」
        let e = api_error("POST /v1/mods", 503, "busy").to_string();
        assert!(!e.contains("CURSEFORGE_API_KEY"), "{e}");
    }

    #[test]
    fn manual_txt_is_actionable() {
        let dir = std::env::temp_dir().join(format!("mcd-cf-manual-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);

        let items = vec![
            resolved(None, "Playmate", ""),
            CfResolved {
                name: "Gone Mod".into(),
                slug: "gone".into(),
                file_name: String::new(),
                url: None,
                sha1: None,
                size: None,
                project_id: 9,
                file_id: 99,
                manual_reason: Some("API 查不到该文件（可能已被作者删除）".into()),
            },
        ];
        let p = write_manual(&dir, &items).unwrap().expect("非空清单应写文件");
        let text = std::fs::read_to_string(&p).unwrap();

        // 头部条数 + 1 基编号 + 可点的页面链接 + 各自的原因 + 重跑提示
        assert!(text.contains("以下 2 个 mod"), "{text}");
        assert!(text.contains("1. Playmate"), "{text}");
        assert!(text.contains("2. Gone Mod"), "{text}");
        assert!(text.contains("files/22"), "第一条要带页面链接: {text}");
        assert!(text.contains("files/99"), "{text}");
        assert!(text.contains("作者禁止第三方分发"), "{text}");
        assert!(text.contains("API 查不到"), "两种原因要能区分: {text}");
        assert!(text.contains("重跑本命令"), "{text}");
        // 资源包/光影的错放提示（因为 CF manifest 无类别信息）
        assert!(text.contains("resourcepacks"), "{text}");

        // 空清单：不产文件
        assert!(write_manual(&dir, &[]).unwrap().is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn backoff_is_clamped() {
        assert_eq!(backoff_ms(None), 2_000);
        assert_eq!(backoff_ms(Some(1)), 1_000);
        assert_eq!(backoff_ms(Some(0)), 200); // 下限，别退避成 0
        assert_eq!(backoff_ms(Some(999)), 10_000); // 上限，别被服务端卡死
    }
}
