//! 用户级配置：**"这台机器上默认怎么干"**。
//!
//! 与下载脚本的分工（别混）：
//!
//! | | 用户配置（本模块） | 下载脚本（`config.rs`） |
//! |---|---|---|
//! | 回答 | 这台机器上默认怎么干 | 这一次干什么 |
//! | 内容 | proxy / retry / concurrency / piece_conns / adaptive / timeout / max_speed / motw | **任务清单**（`[[task]]`）+ 本次覆盖 |
//! | 作用域 | 所有子命令 | 只有 `run` |
//!
//! **配置里不许出现任务** —— 那是脚本的事。这条边界由本文件的结构保证（没有 `tasks` 字段）。
//!
//! 查找顺序（高 → 低）：
//! 1. `--config <path>`
//! 2. 环境变量 `MC_DOWNLOADER_CONFIG`
//! 3. 平台默认位置：Windows `%APPDATA%\mc-downloader\config.toml`；其它 `$XDG_CONFIG_HOME/mc-downloader/` 或 `~/.config/…`
//!
//! **刻意不读当前目录**：`proxy` 是能改变流量走向的键，若自动读 cwd，那么"在任意目录放一个
//! `mc-downloader.toml`"就能静默改代理 —— 与我们刚修完的路径安全问题同族。

use anyhow::{Context, Result};
use serde::Deserialize;
use std::path::{Path, PathBuf};

use crate::download::DownloadOpts;

/// 环境变量：显式指定配置文件路径。
pub const ENV_VAR: &str = "MC_DOWNLOADER_CONFIG";

/// 配置文件的内容。**全部 `Option`** —— 没写 ≠ 写了默认值（否则无法区分"用户要我设成这个"
/// 与"我没配"，优先级就塌了）。
#[derive(Debug, Default, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Settings {
    /// 代理；空字符串 = 显式直连
    pub proxy: Option<String>,
    /// 单文件失败后的重试次数
    pub retries: Option<u32>,
    /// 同时下载的文件数
    pub concurrency: Option<usize>,
    /// 单文件分片并发
    pub piece_conns: Option<usize>,
    /// 自适应连接数（AIMD）
    pub adaptive: Option<bool>,
    /// 建连与单次读超时（秒）
    pub timeout_secs: Option<u64>,
    /// 全局限速，如 `5M`
    pub max_speed: Option<String>,
    /// 是否给可执行文件打 MotW
    pub motw: Option<bool>,
    /// 下载源。**本工具不内置任何源**（issue #15）
    #[serde(default)]
    pub source: SourceSettings,
}

/// `[source]`：下载源。**每一项都必须由用户提供**。
///
/// 2026-10-20 PCL2 在 v2.99.0.0 移除 MC 下载功能（「为遵守相关法律法规」），原因不是被举报，
/// 而是「第三方工具内置下载功能客观上存在绕过官方正版验证的可能」。本工具与它相邻，
/// 所以采取了 community 已验证可行的形态：**把"知道去哪下"从工具移到用户**。
///
/// 三条硬要求（issue #15）：① 默认配置里没有任何源地址；② 代码里没有"没配就用这个"的兜底；
/// ③ 不做自动发现（不探测、不尝试常见地址）。**第 ② 条最容易被漏** ——
/// 任何一处 `unwrap_or("https://…")` 都会让整个决策失效。
#[derive(Debug, Default, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SourceSettings {
    /// 版本清单 URL（**完整地址**，指向 `version_manifest_v2.json` 那一层）
    pub metadata: Option<String>,
    /// assets 基址（不带结尾 `/`）
    pub assets: Option<String>,
    /// Fabric loader 元数据基址
    pub fabric: Option<String>,
    /// Forge：`promotions_slim.json` 所在基址
    pub forge_meta: Option<String>,
    /// Forge：maven 基址（installer 与 libs）
    pub forge_maven: Option<String>,
    /// NeoForge：maven 基址
    pub neoforge: Option<String>,
}

/// 解析后的源集合：**全部必填**。
///
/// 缺失时的报错必须是**明确的指引**，不是"下载失败"（issue #15 §2）：
/// 告诉用户去哪儿填，并提醒「只想下单个文件用 fetch，不需要源」。
#[derive(Debug, Clone)]
pub struct Sources {
    pub metadata: String,
    pub assets: String,
    pub fabric: String,
    pub forge_meta: String,
    pub forge_maven: String,
    pub neoforge: String,
}

impl Sources {
    /// 从配置解析。`cfg_path` 只用于错误文案（告诉用户该改哪个文件）。
    pub fn resolve(cfg: &SourceSettings, cfg_path: Option<&Path>) -> Result<Self> {
        let need = |key: &str, v: &Option<String>| -> Result<String> {
            let Some(v) = v.as_deref().map(str::trim).filter(|s| !s.is_empty()) else {
                return Err(anyhow::anyhow!(missing_source_msg(key, cfg_path)));
            };
            if !(v.starts_with("https://") || v.starts_with("http://")) {
                anyhow::bail!(
                    "[source].{key} 必须以 http:// 或 https:// 开头：`{v}`\n\
                     \x20 这里只接受一个地址，不接受 `none` / `off` 这类取值。"
                );
            }
            Ok(v.trim_end_matches('/').to_string())
        };
        Ok(Self {
            metadata: need("metadata", &cfg.metadata)?,
            assets: need("assets", &cfg.assets)?,
            fabric: need("fabric", &cfg.fabric)?,
            forge_meta: need("forge_meta", &cfg.forge_meta)?,
            forge_maven: need("forge_maven", &cfg.forge_maven)?,
            neoforge: need("neoforge", &cfg.neoforge)?,
        })
    }

    /// 允许的主机集合（给 Forge installer 的 host 白名单用）。
    ///
    /// 为什么从配置推：installer profile 里的下载 URL 是**远端给的**，不能无条件信任；
    /// 而"信哪些 host"应当与"我配了哪些源"同源 —— 否则用户换镜像后 installer 会被自己拒掉。
    pub fn host_of(url: &str) -> Option<String> {
        let rest = url.split_once("://")?.1;
        let host = rest.split(['/', '?', '#']).next()?;
        let host = host.rsplit_once('@').map_or(host, |(_, h)| h); // 去掉 userinfo
        Some(host.split(':').next()?.to_ascii_lowercase())
    }

    /// 配置里所有源的 host（去重）。
    pub fn hosts(&self) -> Vec<String> {
        let mut v: Vec<String> = [
            &self.metadata,
            &self.assets,
            &self.fabric,
            &self.forge_meta,
            &self.forge_maven,
            &self.neoforge,
        ]
        .into_iter()
        .filter_map(|u| Self::host_of(u))
        .collect();
        v.sort();
        v.dedup();
        v
    }
}

/// 缺源时的统一文案。**不要**在这里塞任何具体地址（那条判据见 issue #15）。
fn missing_source_msg(key: &str, cfg_path: Option<&Path>) -> String {
    let where_ = match cfg_path {
        Some(p) => format!("请先在 {} 的 [source] 段填写 `{key}` 地址。", p.display()),
        None => format!(
            "还没有配置文件。请先建一个（`--config <path>` 指定，或放到平台配置目录），\
             在 [source] 段填写 `{key}` 地址。"
        ),
    };
    format!(
        "未配置下载源（`[source].{key}`）。\n\
         \x20 本工具不内置任何源地址 —— 这是刻意的（issue #15）。\n\
         \x20 {where_}\n\
         \x20 只想下单个文件？`mc-downloader fetch <url> -d <dir>` 不需要任何源。"
    )
}
/// 读到的配置 + 它来自哪个文件（`None` = 没找到任何配置文件，全走默认）。
#[derive(Debug, Default)]
pub struct Loaded {
    pub settings: Settings,
    /// 这个配置是从哪个文件读来的（给错误文案用）
    pub path: Option<PathBuf>,
}

/// 平台默认路径（不保证存在）。
pub fn default_path() -> Option<PathBuf> {
    if cfg!(windows) {
        let appdata = std::env::var_os("APPDATA")?;
        Some(
            PathBuf::from(appdata)
                .join("mc-downloader")
                .join("config.toml"),
        )
    } else {
        let base = match std::env::var_os("XDG_CONFIG_HOME") {
            Some(x) if !x.is_empty() => PathBuf::from(x),
            _ => PathBuf::from(std::env::var_os("HOME")?).join(".config"),
        };
        Some(base.join("mc-downloader").join("config.toml"))
    }
}

/// 配置文件的**候选路径**（按查找顺序），给 `--print-config` 用来说明"它在哪找过"。
///
/// 与 [`locate`] 的区别：`locate` 返回"实际读到的那个"（可能一个都没有）；
/// 这里返回**全部候选**，不管存不存在 —— 用户问的正是"该往哪写"。
/// 不含 `--config`（那是运行时给的，`main` 自己知道）。
pub fn search_paths() -> Vec<PathBuf> {
    let mut v = Vec::new();
    if let Some(p) = std::env::var_os(ENV_VAR)
        && !p.is_empty()
    {
        v.push(PathBuf::from(p));
    }
    if let Some(p) = default_path() {
        v.push(p);
    }
    v
}

/// 决定要读哪个文件：`--config` > 环境变量 > 平台默认（**只有存在时才用**）。
///
/// 返回 `None` 表示"没有任何配置文件"，调用方据此全走默认。
pub fn locate(explicit: Option<&Path>) -> Option<PathBuf> {
    if let Some(p) = explicit {
        return Some(p.to_path_buf());
    }
    if let Some(p) = std::env::var_os(ENV_VAR)
        && !p.is_empty()
    {
        return Some(PathBuf::from(p));
    }
    default_path().filter(|p| p.exists())
}

/// 读取配置。文件不存在 ⇒ 空配置（不算错）；文件存在但格式坏 ⇒ 报错（不许静默忽略）。
pub fn load(explicit: Option<&Path>) -> Result<Loaded> {
    let Some(path) = locate(explicit) else {
        return Ok(Loaded::default());
    };
    // 显式指定（--config / 环境变量）却不存在 ⇒ 报错；平台默认位置不存在 ⇒ 已在 locate 里过滤掉
    let text = match std::fs::read_to_string(&path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound && explicit.is_none() => {
            return Ok(Loaded::default());
        }
        Err(e) => {
            return Err(e).with_context(|| format!("读配置文件失败: {}", path.display()));
        }
    };
    let settings: Settings = toml::from_str(&text)
        .with_context(|| format!("解析配置文件失败（检查键名）: {}", path.display()))?;
    Ok(Loaded {
        settings,
        path: Some(path),
    })
}

/// 一层来源：标签 + 该层的值。
type Layer<'a> = (&'static str, &'a Settings);

/// 按层级挑一个值，并记下它来自哪一层。
fn pick<T: Clone>(
    layers: &[Layer<'_>],
    field: fn(&Settings) -> Option<T>,
    fallback: T,
) -> (T, &'static str) {
    for (label, s) in layers {
        if let Some(v) = field(s) {
            return (v, label);
        }
    }
    (fallback, "default")
}

/// 同上，但**没有 fallback**：`Option` 型字段（proxy / max_speed）用这个，
/// 因为"都没配"要能表达成 `None`，不能拿一个假值顶替。
fn pick_opt<T: Clone>(
    layers: &[Layer<'_>],
    field: fn(&Settings) -> Option<T>,
) -> (Option<T>, &'static str) {
    for (label, s) in layers {
        if let Some(v) = field(s) {
            return (Some(v), label);
        }
    }
    (None, "default")
}

/// 合并结果：可直接构造 [`DownloadOpts`]，外加每个键的来源（给 `--print-config` 用）。
pub struct Merged {
    pub opts: DownloadOpts,
    /// `(键, 生效值, 来源)` —— 顺序固定，便于测试与阅读
    pub rows: Vec<(&'static str, String, &'static str)>,
}

/// 合并：`layers` 从高到低（CLI → 脚本 → 用户配置），最后落到 `dflt`。
pub fn merge(layers: &[Layer<'_>], dflt: &DownloadOpts) -> Merged {
    let (retries, s_retry) = pick(layers, |s| s.retries, dflt.retries);
    let (concurrency, s_conc) = pick(layers, |s| s.concurrency, dflt.concurrency);
    let (piece_conns, s_piece) = pick(layers, |s| s.piece_conns, dflt.piece_conns);
    let (adaptive, s_adapt) = pick(layers, |s| s.adaptive, dflt.adaptive);
    let (timeout_secs, s_timeout) = pick(layers, |s| s.timeout_secs, dflt.timeout_secs);
    let (motw, s_motw) = pick(layers, |s| s.motw, dflt.motw);
    let (proxy_layers, s_proxy) = pick_opt(layers, |s| s.proxy.clone());
    let (speed_layers, s_speed) = pick_opt(layers, |s| s.max_speed.clone());

    let proxy_raw = proxy_layers.or(dflt.proxy.clone());
    // 空字符串 = **显式直连**（与"没配"在行为上一样，但展示上要能区分，否则用户看不出自己配过）
    let proxy = proxy_raw.clone().filter(|p| !p.trim().is_empty());
    let max_speed = speed_layers.and_then(|s| crate::progress::parse_speed(&s));

    let show = |o: &Option<String>| match o {
        Some(v) if v.trim().is_empty() => "（直连）".to_string(),
        Some(v) => v.clone(),
        None => "（未配）".to_string(),
    };
    let rows = vec![
        ("proxy", show(&proxy_raw), s_proxy),
        ("retries", retries.to_string(), s_retry),
        ("concurrency", concurrency.to_string(), s_conc),
        ("piece_conns", piece_conns.to_string(), s_piece),
        ("adaptive", adaptive.to_string(), s_adapt),
        ("timeout_secs", timeout_secs.to_string(), s_timeout),
        (
            "max_speed",
            match max_speed {
                Some(n) => crate::download::human_rate(n) + "/s",
                None => "（不限）".to_string(),
            },
            s_speed,
        ),
        ("motw", motw.to_string(), s_motw),
    ];

    Merged {
        opts: DownloadOpts {
            proxy,
            retries,
            concurrency,
            piece_conns,
            adaptive,
            timeout_secs,
            motw,
            max_speed,
            ..DownloadOpts::default()
        },
        rows,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn user(proxy: Option<&str>, piece: Option<usize>) -> Settings {
        Settings {
            proxy: proxy.map(str::to_string),
            piece_conns: piece,
            ..Default::default()
        }
    }

    #[test]
    fn cli_beats_user_config_beats_default() {
        let cli = Settings {
            piece_conns: Some(8),
            ..Default::default()
        };
        let cfg = user(None, Some(16));
        let m = merge(&[("cli", &cli), ("config", &cfg)], &DownloadOpts::default());
        assert_eq!(m.opts.piece_conns, 8, "CLI 最高");
        let src = m.rows.iter().find(|r| r.0 == "piece_conns").unwrap();
        assert_eq!(src.2, "cli");

        let m = merge(
            &[("cli", &Settings::default()), ("config", &cfg)],
            &DownloadOpts::default(),
        );
        assert_eq!(m.opts.piece_conns, 16, "CLI 没给就用配置");
        assert_eq!(
            m.rows.iter().find(|r| r.0 == "piece_conns").unwrap().2,
            "config"
        );

        let m = merge(&[("cli", &Settings::default())], &DownloadOpts::default());
        assert_eq!(m.opts.piece_conns, crate::download::DEFAULT_PIECE_CONNS);
        assert_eq!(
            m.rows.iter().find(|r| r.0 == "piece_conns").unwrap().2,
            "default"
        );
    }

    #[test]
    fn empty_proxy_means_direct_connect() {
        let cfg = user(Some("   "), None);
        let m = merge(&[("config", &cfg)], &DownloadOpts::default());
        assert!(m.opts.proxy.is_none(), "空字符串 = 显式直连");
        assert_eq!(m.rows[0].1, "（直连）");
    }

    #[test]
    fn unknown_field_is_rejected_not_ignored() {
        let e = toml::from_str::<Settings>("piec_conns = 4\n").unwrap_err();
        assert!(e.to_string().contains("piec_conns"), "要指出拼错的键: {e}");
    }

    #[test]
    fn config_has_no_task_field() {
        // 配置里出现 [[task]] 必须报错 —— 任务属于下载脚本
        let e = toml::from_str::<Settings>("[[task]]\nkind = \"mc\"\n").unwrap_err();
        assert!(e.to_string().contains("task"), "{e}");
    }

    #[test]
    fn never_reads_cwd() {
        // 本进程的 cwd 里就算有 mc-downloader.toml 也不该被当成配置来源
        let cwd = std::env::current_dir().unwrap();
        if let Some(found) = locate(None) {
            assert!(
                !found.starts_with(&cwd) || found == default_path().unwrap_or_default(),
                "不许把 cwd 下的文件当配置: {}",
                found.display()
            );
        }
    }
}
