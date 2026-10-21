//! 配置文件（TOML）。
//!
//! 优先级：**CLI 显式给 > 配置文件 > 内置默认**。
//! 未知字段直接报错（`deny_unknown_fields`）—— 手误打错 key 却静默不生效，
//! 是配置文件最难排查的一类问题。

use anyhow::{Context, Result};
use serde::Deserialize;
use std::path::Path;

#[derive(Debug, Deserialize, Default, Clone)]
#[serde(deny_unknown_fields)]
pub struct Config {
    #[serde(default)]
    pub network: Network,
    #[serde(default)]
    pub download: Download,
    #[serde(default)]
    pub task: Task,
}

#[derive(Debug, Deserialize, Default, Clone)]
#[serde(deny_unknown_fields)]
pub struct Network {
    /// 空字符串表示显式直连
    pub proxy: Option<String>,
    pub retry: Option<u32>,
    pub timeout_secs: Option<u64>,
}

#[derive(Debug, Deserialize, Default, Clone)]
#[serde(deny_unknown_fields)]
pub struct Download {
    /// 同时下载的文件数
    pub concurrency: Option<usize>,
    /// 单文件分片并发
    pub piece_conns: Option<usize>,
    /// AIMD 自适应
    pub adaptive: Option<bool>,
}

#[derive(Debug, Deserialize, Default, Clone)]
#[serde(deny_unknown_fields)]
pub struct Task {
    /// "mc" | "generic"
    pub mode: Option<String>,
    #[serde(default)]
    pub mc: McTask,
    #[serde(default)]
    pub generic: GenericTask,
}

#[derive(Debug, Deserialize, Default, Clone)]
#[serde(deny_unknown_fields)]
pub struct McTask {
    pub version: Option<String>,
    pub dir: Option<String>,
}

#[derive(Debug, Deserialize, Default, Clone)]
#[serde(deny_unknown_fields)]
pub struct GenericTask {
    pub url: Option<String>,
    pub dir: Option<String>,
    pub name: Option<String>,
    pub size: Option<u64>,
    /// 额外镜像（内容须与 url 相同）
    pub mirrors: Option<Vec<String>>,
}

impl Config {
    pub fn load(path: &Path) -> Result<Self> {
        let text = std::fs::read_to_string(path)
            .with_context(|| format!("读取配置失败: {}", path.display()))?;
        let cfg: Config = toml::from_str(&text)
            .with_context(|| format!("解析配置失败（检查字段名与类型）: {}", path.display()))?;
        cfg.validate()?;
        Ok(cfg)
    }

    /// 语义校验：不是类型错误，而是"值本身不成立"
    pub fn validate(&self) -> Result<()> {
        if let Some(mode) = &self.task.mode {
            let m = mode.to_ascii_lowercase();
            if m != "mc" && m != "generic" {
                anyhow::bail!("task.mode 只能是 \"mc\" 或 \"generic\"，收到 {mode:?}");
            }
            if m == "mc" && self.task.mc.version.is_none() {
                anyhow::bail!("task.mode = \"mc\" 时必须提供 task.mc.version");
            }
            if m == "generic" && self.task.generic.url.is_none() {
                anyhow::bail!("task.mode = \"generic\" 时必须提供 task.generic.url");
            }
        }
        if let Some(c) = self.download.concurrency {
            if c == 0 || c > 1024 {
                anyhow::bail!("download.concurrency 应在 1..=1024，收到 {c}");
            }
        }
        if let Some(c) = self.download.piece_conns {
            if c == 0 || c > 256 {
                anyhow::bail!("download.piece_conns 应在 1..=256，收到 {c}");
            }
        }
        Ok(())
    }

    /// 代理：空串视为直连
    pub fn proxy_filtered(&self) -> Option<String> {
        match &self.network.proxy {
            Some(p) if !p.trim().is_empty() => Some(p.clone()),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_minimal() {
        let cfg: Config = toml::from_str("").unwrap();
        assert!(cfg.task.mode.is_none());
        assert!(cfg.proxy_filtered().is_none());
    }

    #[test]
    fn parse_full() {
        let src = r#"
[network]
proxy = "http://127.0.0.1:7897"
retry = 5

[download]
concurrency = 8
piece_conns = 6
adaptive = true

[task]
mode = "mc"

[task.mc]
version = "26.3"
dir = "D:/mc"
"#;
        let cfg: Config = toml::from_str(src).unwrap();
        assert_eq!(cfg.network.retry, Some(5));
        assert_eq!(cfg.download.concurrency, Some(8));
        assert_eq!(cfg.download.adaptive, Some(true));
        assert_eq!(cfg.task.mode.as_deref(), Some("mc"));
        assert_eq!(cfg.task.mc.version.as_deref(), Some("26.3"));
        assert_eq!(cfg.proxy_filtered().as_deref(), Some("http://127.0.0.1:7897"));
    }

    #[test]
    fn empty_proxy_means_direct() {
        let cfg: Config = toml::from_str("[network]\nproxy = \"\"\n").unwrap();
        assert!(cfg.proxy_filtered().is_none(), "空串应视为直连");
    }

    #[test]
    fn unknown_field_is_error() {
        let r: std::result::Result<Config, _> = toml::from_str("[network]\nproxxy = \"x\"\n");
        assert!(r.is_err(), "拼错字段名必须报错");
    }

    #[test]
    fn validate_rejects_bad_mode() {
        let cfg: Config = toml::from_str("[task]\nmode = \"ftp\"\n").unwrap();
        assert!(cfg.validate().is_err());
    }

    #[test]
    fn validate_requires_version_for_mc() {
        let cfg: Config = toml::from_str("[task]\nmode = \"mc\"\n").unwrap();
        assert!(cfg.validate().is_err());
    }

    #[test]
    fn validate_requires_url_for_generic() {
        let cfg: Config = toml::from_str("[task]\nmode = \"generic\"\n").unwrap();
        assert!(cfg.validate().is_err());
    }

    #[test]
    fn validate_rejects_zero_concurrency() {
        let cfg: Config = toml::from_str("[download]\nconcurrency = 0\n").unwrap();
        assert!(cfg.validate().is_err());
    }

    #[test]
    fn generic_mirrors_parse() {
        let src = r#"
[task]
mode = "generic"
[task.generic]
url = "https://a/x.iso"
size = 100
mirrors = ["https://b/x.iso", "https://c/x.iso"]
"#;
        let cfg: Config = toml::from_str(src).unwrap();
        assert_eq!(cfg.task.generic.mirrors.as_ref().unwrap().len(), 2);
        assert!(cfg.validate().is_ok());
    }
}
