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
    /// 给人看的：`Forge 47.4.10` / `NeoForge 21.1.251` / `Fabric 0.20.0`；
    /// 未知前缀原样返回；manifest 没写 → `无`
    pub loader_label: String,
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

/// 这个字符串像不像 MC 版本号：形如 `x.y` 或 `x.y.z` 的**纯数字**。
///
/// ⚠️ **这个判据只能挡"明显不是版本号"的**（如 `"26.3 Fabric-v2"` 带空格/字母），
/// 挡不住"包版本号误填进 version 字段"（如 `"26.3"` 形状合法但可能未发布）。
/// 这是放宽 26.x 之后的真实取舍 —— 用户看到警告时应当知道它只是"**形状可疑**"，
/// 不是"版本不对"。
///
/// *为什么放宽*：MC 已改用年份编号。实测本机
/// `versions/26.1.2/26.1.2.json` → `type=release`、`releaseTime=2026-04-09`，
/// 官方 manifest 的 `latest.release` 就是 `"26.1.2"`。
/// 旧判据要求 `1.` 开头，会把**当前正式版**标成可疑 —— 那是错的。
pub fn looks_like_mc_version(v: &str) -> bool {
    let v = v.trim();
    if v.is_empty() {
        return false;
    }
    let parts: Vec<&str> = v.split('.').collect();
    if parts.len() < 2 || parts.len() > 3 {
        return false;
    }
    parts
        .iter()
        .all(|p| !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()))
}

/// `forge-47.4.10` → `Forge 47.4.10`；`neoforge-` 必须**先于** `forge-` 匹配。
/// 不认识的 id **原样返回**（别 bail —— 新加载器出现时不该让整个包装不了）。
pub fn loader_label(raw: &str) -> String {
    let raw = raw.trim();
    if raw.is_empty() {
        return "无".to_string();
    }
    for (prefix, name) in [
        ("neoforge-", "NeoForge "),
        ("forge-", "Forge "),
        ("fabric-", "Fabric "),
    ] {
        if let Some(rest) = raw.strip_prefix(prefix) {
            return format!("{name}{rest}");
        }
    }
    raw.to_string()
}

/// 把 URL 拆成 `(origin, path)`；`origin` 形如 `https://host[:port]`
fn split_origin(u: &str) -> Result<(String, &str)> {
    let (scheme, rest) = u
        .split_once("://")
        .ok_or_else(|| anyhow::anyhow!("URL 缺 scheme: {u}"))?;
    Ok(match rest.find('/') {
        Some(i) => (format!("{scheme}://{}", &rest[..i]), &rest[i..]),
        None => (format!("{scheme}://{rest}"), "/"),
    })
}

/// 按 RFC 3986 的常见情形把 `Location` 解析成绝对 URL（**纯函数**）。
///
/// 实测 forgecdn 给的是绝对地址，但**不能假设**：相对/协议相对/绝对路径都兜住。
/// 不引入 `url` crate 是为了零新增依赖（reqwest 的传递依赖不保证可用）。
pub fn resolve_location(base: &str, loc: &str) -> Result<String> {
    let loc = loc.trim();
    if loc.is_empty() {
        bail!("Location 为空");
    }
    // ① 已是绝对地址
    if loc.starts_with("http://") || loc.starts_with("https://") {
        return Ok(loc.to_string());
    }
    let scheme = base.split_once("://").map(|(s, _)| s).unwrap_or("https");
    let (origin, base_path) = split_origin(base)?;
    // ② 协议相对 `//host/path`
    if let Some(rest) = loc.strip_prefix("//") {
        return Ok(format!("{scheme}://{rest}"));
    }
    // ③ 绝对路径 `/path`
    if loc.starts_with('/') {
        return Ok(format!("{origin}{loc}"));
    }
    // ④ 相对路径：接到 base 的目录后面
    let dir = match base_path.rfind('/') {
        Some(i) => &base_path[..=i],
        None => "/",
    };
    Ok(format!("{origin}{dir}{loc}"))
}

/// 真实地址的解析结果
pub enum RealUrl {
    Ok { url: String, size: Option<u64> },
    /// 链路已死（404）—— 不是致命错误，该条进人工清单
    Dead(String),
}

/// CF 的 `downloadUrl` 指向 `edge.forgecdn.net`，而**该域对带 Range 的请求直接 404**
/// （且不返回 302）；真文件在 302 目标 `mediafilez.forgecdn.net` 上，那个域支持 Range（实测 206）。
/// ⇒ 在这里先 HEAD 一次（**禁止跟随重定向**）拿 `Location`，后续分片直接用真地址。
///
/// `client` 必须是**不跟随重定向**的那个（调用方建一次复用，别每个 URL 建一个）。
pub async fn resolve_real_url(client: &reqwest::Client, url: &str) -> Result<RealUrl> {
    let r = client.head(url).send().await.with_context(|| {
        format!("HEAD {url} 失败（拿不到真实地址，分片可能 404）")
    })?;
    let status = r.status().as_u16();
    let clen = r
        .headers()
        .get(reqwest::header::CONTENT_LENGTH)
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.trim().parse::<u64>().ok());

    match status {
        301 | 302 | 303 | 307 | 308 => {
            let loc = r
                .headers()
                .get(reqwest::header::LOCATION)
                .and_then(|v| v.to_str().ok())
                .ok_or_else(|| anyhow::anyhow!("{url} 返回 {status} 但没有 Location 头"))?;
            let abs = resolve_location(url, loc)
                .with_context(|| format!("解析 Location 失败: {loc}"))?;
            Ok(RealUrl::Ok { url: abs, size: clen })
        }
        200 => Ok(RealUrl::Ok {
            url: url.to_string(),
            size: clen,
        }),
        404 => Ok(RealUrl::Dead(format!(
            "文件地址 404（作者可能已删除/替换该文件）: {url}"
        ))),
        other => {
            // HEAD 本身不被支持（或临时故障）：**不 bail**，回退原地址。
            // 后果：< 8MB 的单流可能成；≥ 8MB 的分片会 404 —— 但比整个包失败好。
            eprintln!("[!] HEAD {url} 返回 {other}，回退原地址（大文件的分片可能失败）");
            Ok(RealUrl::Ok {
                url: url.to_string(),
                size: None,
            })
        }
    }
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
        .unwrap_or_default(); // 空 ⇒ loader_label 会映射成「无」

    // 空 files 不报错：那是"这个包没有要下的 mod"，不是解析失败
    // （主理人 2026-10-22 裁决：refs 空即可，交给上层打印"可自动 0 个"）

    let refs = raw
        .files
        .into_iter()
        .map(|f| CfRef {
            project_id: f.project_id,
            file_id: f.file_id,
            required: f.required,
        })
        .collect();

    let label = loader_label(&loader_raw);
    Ok(CfManifest {
        name: raw.name.unwrap_or_else(|| "(未命名)".to_string()),
        mc: raw.minecraft.version,
        loader_raw,
        loader_label: label,
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
             \n\
             申请（免费，几分钟）：\n\
             \x20 1. 登录 https://console.curseforge.com/ （没有账号先注册）\n\
             \x20 2. 创建 API Key，用途填「个人使用 / 下载整合包」\n\
             \x20 3. 复制以 $2a$10$ 开头的字符串\n\
             \n\
             设置：\n\
             \x20 Windows:     setx {ENV_KEY} \"<key>\"     然后重开终端\n\
             \x20 Linux/macOS: export {ENV_KEY}='<key>'\n\
             \n\
             验证（返回 200 就是成功；403 说明 key 无效）：\n\
             \x20 curl -s -o /dev/null -w \"%{{http_code}}\\n\" \\\n\
             \x20   -H \"x-api-key: ${{{ENV_KEY}}}\" \\\n\
             \x20   https://api.curseforge.com/v1/mods/1066065\n\
             \n\
             不配 key 也能用：Modrinth 整合包（.mrpack）不需要 key"
        ),
    }
}

/// 极简 URL 编码（只用于搜索词：空格 → `+`，其余非 unreserved 字符转 %XX）
fn urlencode(s: &str) -> String {
    let mut o = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => o.push(b as char),
            b' ' => o.push('+'),
            _ => o.push_str(&format!("%{b:02X}")),
        }
    }
    o
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
            "  {}. {}\n     CurseForge: {}   [{}]\n     可试 Modrinth: https://modrinth.com/mods?q={}\n",
            i + 1,
            m.name,
            page_url(&m.slug, m.file_id),
            why,
            urlencode(&m.name)
        ));
    }
    s.push_str("\n放到位后重跑本命令即可（已下载的会跳过）。\n");
    s.push_str("（若某文件本属资源包/光影，请自行移到 resourcepacks/ 或 shaderpacks/。）\n");
    s.push_str(
        "⚠️ Modrinth 上未必有同一个 mod；同名也可能是不同项目 —— 请核对作者与 MC 版本再下载。\n",
    );

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
    /// CurseForge 的文件状态：1=Approved, 2=ApprovedWithRestrictions,
    /// 3=Rejected, 4=Processing, 5=ChangesRequired, 6=Deleted, 7=Archived
    /// 缺省为 None（老响应可能没有这个字段）
    #[serde(default, rename = "fileStatus")]
    file_status: Option<u32>,
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
    //    `noredir` 只用于解 302（见 resolve_real_url 的注释），建一次复用
    let noredir = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .context("建 no-redirect 客户端失败")?;
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
                let sha1 = f.sha1();
                let api_size = f.file_length;
                // 拿到 downloadUrl 后**先解 302**：edge 域对 Range 404，真文件在 302 目标上
                match f.download_url.clone() {
                    None => CfResolved {
                        name,
                        slug,
                        file_name: f.file_name.clone(),
                        url: None,
                        sha1,
                        size: api_size,
                        project_id: r.project_id,
                        file_id: r.file_id,
                        // ⚠️ downloadUrl 为空有**两种完全不同的原因**，混成一句会误导用户：
                        //   · fileStatus=4（Processing）：文件还在上传/审核，**等一下重跑就行**
                        //   · 禁分发：**永远下不了**，只能手动
                        // 实测 IWS 那个包里有 2 个 fileStatus=4 的，原来被报成"作者禁止分发"。
                        manual_reason: Some(match f.file_status {
                            Some(4) => "文件正在处理中（Processing）—— 稍后重跑即可".to_string(),
                            Some(2) => "作者限制分发（ApprovedWithRestrictions）".to_string(),
                            Some(3) => "文件被拒绝（Rejected）".to_string(),
                            Some(5) => "需要修改后才能发布（ChangesRequired）".to_string(),
                            Some(6) => "文件已被作者删除（Deleted）".to_string(),
                            Some(7) => "文件已归档（Archived）".to_string(),
                            Some(s) => format!("API 未给直链（fileStatus={s}）"),
                            None => "作者禁止第三方分发（API 未给直链）".to_string(),
                        }),
                    },
                    Some(raw) => match resolve_real_url(&noredir, &raw).await? {
                        RealUrl::Dead(why) => CfResolved {
                            name,
                            slug,
                            file_name: f.file_name.clone(),
                            url: None,
                            sha1,
                            size: api_size,
                            project_id: r.project_id,
                            file_id: r.file_id,
                            manual_reason: Some(why),
                        },
                        RealUrl::Ok { url, size } => CfResolved {
                            name,
                            slug,
                            file_name: f.file_name.clone(),
                            url: Some(url),
                            sha1,
                            // HEAD 给的 content-length 比 API 的 fileLength 更贴近实际
                            size: size.or(api_size),
                            project_id: r.project_id,
                            file_id: r.file_id,
                            manual_reason: None,
                        },
                    },
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
        assert_eq!(m.loader_label, "Forge 47.4.10");
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
        let m = parse_manifest_str(&text).unwrap();
        assert_eq!(m.loader_raw, "neoforge-21.1.251");
        assert_eq!(m.loader_label, "NeoForge 21.1.251");
    }

    #[test]
    fn loader_absent_is_not_a_panic() {
        let text = manifest("1.20.1", "[]", r#"[{"projectID":1,"fileID":2}]"#);
        let m = parse_manifest_str(&text).unwrap();
        assert_eq!(m.loader_raw, "", "manifest 没写就留空，别编一个占位串");
        assert_eq!(m.loader_label, "无");
    }

    /// 空 files **不是错误**（主理人裁决）：那是"这个包没有要下的 mod"
    #[test]
    fn empty_files_is_ok() {
        let text = manifest("1.20.1", "[]", "[]");
        let m = parse_manifest_str(&text).unwrap();
        assert!(m.refs.is_empty());
        assert_eq!(m.loader_label, "无");
    }

    #[test]
    fn loader_labels_are_normalized() {
        assert_eq!(loader_label("forge-47.4.10"), "Forge 47.4.10");
        assert_eq!(loader_label("neoforge-21.1.251"), "NeoForge 21.1.251");
        assert_eq!(loader_label("fabric-0.20.0"), "Fabric 0.20.0");
        assert_eq!(loader_label(""), "无");
        // 未知前缀：**原样返回**，别 bail（新加载器出现时不该整包装不了）
        assert_eq!(loader_label("custom-1.2.3"), "custom-1.2.3");
        // neoforge 不能被 forge 抢走（前缀顺序陷阱）
        assert!(loader_label("neoforge-1").starts_with("NeoForge"));
    }

    #[test]
    fn invalid_json_is_an_error() {
        assert!(parse_manifest_str("{not json").is_err());
    }

    /// 年份编号已是现实：26.1.2 是当前正式版（本机 versions/26.1.2 实测）
    #[test]
    fn year_based_versions_are_accepted() {
        assert!(looks_like_mc_version("26.1.2"));
        assert!(looks_like_mc_version("26.3"));
        assert!(looks_like_mc_version("1.20.1"));
        assert!(looks_like_mc_version("1.8.9"));
        assert!(looks_like_mc_version("1.21"));
        // 带空格/字母的仍挡住 —— 这是本判据**唯一**还能做的事
        assert!(!looks_like_mc_version("26.3 Fabric-v2"));
        assert!(!looks_like_mc_version("1.x"));
        assert!(!looks_like_mc_version("1"));
        assert!(!looks_like_mc_version("1.2.3.4"));
        assert!(!looks_like_mc_version(""));
        assert!(!looks_like_mc_version("  "));
    }

    /// 取舍必须写死在测试里：放宽之后，"包版本误填"这种再也测不出来
    #[test]
    fn pack_version_misplaced_is_no_longer_detectable() {
        // IWS 那个包写的是 "26.3"（可能是包版本号），但形状与合法 MC 版本无法区分
        assert!(looks_like_mc_version("26.3"));
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
    fn location_absolute_passthrough() {
        // 实测 forgecdn 给的就是绝对地址（最常见路径）
        assert_eq!(
            resolve_location(
                "https://edge.forgecdn.net/files/6752/633/x.jar",
                "https://mediafilez.forgecdn.net/files/6752/633/x.jar"
            )
            .unwrap(),
            "https://mediafilez.forgecdn.net/files/6752/633/x.jar"
        );
    }

    #[test]
    fn location_other_shapes_are_covered() {
        let base = "https://edge.forgecdn.net/files/6752/633/x.jar";
        // 协议相对
        assert_eq!(
            resolve_location(base, "//mediafilez.forgecdn.net/a/b.jar").unwrap(),
            "https://mediafilez.forgecdn.net/a/b.jar"
        );
        // 绝对路径
        assert_eq!(
            resolve_location(base, "/files/1/2/y.jar").unwrap(),
            "https://edge.forgecdn.net/files/1/2/y.jar"
        );
        // 相对路径：接到 base 的**目录**后面（不是替换文件名所在段）
        assert_eq!(
            resolve_location(base, "z.jar").unwrap(),
            "https://edge.forgecdn.net/files/6752/633/z.jar"
        );
        // base 没有路径时，目录默认 "/"
        assert_eq!(
            resolve_location("https://host", "z.jar").unwrap(),
            "https://host/z.jar"
        );
    }

    #[test]
    fn location_empty_is_an_error() {
        let e = resolve_location("https://edge/x.jar", "   ").unwrap_err().to_string();
        assert!(e.contains("Location 为空"), "{e}");
        // base 缺 scheme 也要报错，别静默拼出个怪 URL
        assert!(resolve_location("edge/x.jar", "y.jar").is_err());
    }

    #[test]
    fn backoff_is_clamped() {
        assert_eq!(backoff_ms(None), 2_000);
        assert_eq!(backoff_ms(Some(1)), 1_000);
        assert_eq!(backoff_ms(Some(0)), 200); // 下限，别退避成 0
        assert_eq!(backoff_ms(Some(999)), 10_000); // 上限，别被服务端卡死
    }
}
