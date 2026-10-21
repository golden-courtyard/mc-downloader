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
    /// 任务清单，按声明顺序执行
    #[serde(default, rename = "task")]
    pub tasks: Vec<TaskSpec>,
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

/// 一个下载任务（`[[task]]` 数组元素，按 `kind` 区分）
#[derive(Debug, Deserialize, Clone)]
#[serde(tag = "kind", deny_unknown_fields)]
pub enum TaskSpec {
    /// 下载原版
    #[serde(rename = "mc")]
    Mc { version: String, dir: String },
    /// 下载单个 URL
    #[serde(rename = "generic")]
    Generic {
        url: String,
        dir: String,
        name: Option<String>,
        size: Option<u64>,
        #[serde(default)]
        mirrors: Vec<String>,
    },
    /// 装 Modrinth 整合包
    #[serde(rename = "pack")]
    Pack { file: String, dir: String },
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
        if self.tasks.is_empty() {
            anyhow::bail!("脚本里至少要有一个 [[task]]");
        }
        if let Some(c) = self.download.concurrency
            && (c == 0 || c > 1024)
        {
            anyhow::bail!("download.concurrency 应在 1..=1024，收到 {c}");
        }
        if let Some(c) = self.download.piece_conns
            && (c == 0 || c > 256)
        {
            anyhow::bail!("download.piece_conns 应在 1..=256，收到 {c}");
        }
        for (i, t) in self.tasks.iter().enumerate() {
            if let TaskSpec::Generic { size: Some(0), .. } = t {
                anyhow::bail!(
                    "第 {} 个任务是 generic 且 size = 0，会导致不分片；请给真实大小或省略",
                    i + 1
                );
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
        let src = r#"
[[task]]
kind = "mc"
version = "26.3"
dir = "D:/mc"
"#;
        let cfg: Config = toml::from_str(src).unwrap();
        assert_eq!(cfg.tasks.len(), 1);
        assert!(cfg.validate().is_ok());
        assert!(cfg.proxy_filtered().is_none());
    }

    #[test]
    fn parse_multi_task() {
        let src = r#"
[network]
proxy = "http://127.0.0.1:7897"
retry = 5

[download]
concurrency = 8
piece_conns = 6
adaptive = true

[[task]]
kind = "mc"
version = "26.3"
dir = "D:/mc"

[[task]]
kind = "generic"
url = "https://a/x.iso"
dir = "D:/dl"
name = "x.iso"
size = 100
mirrors = ["https://b/x.iso", "https://c/x.iso"]

[[task]]
kind = "pack"
file = "E:/FO.mrpack"
dir = "D:/mc/FO"
"#;
        let cfg: Config = toml::from_str(src).unwrap();
        assert_eq!(cfg.tasks.len(), 3);
        assert_eq!(cfg.network.retry, Some(5));
        assert_eq!(cfg.download.concurrency, Some(8));
        assert!(cfg.validate().is_ok());
        match &cfg.tasks[1] {
            TaskSpec::Generic { mirrors, .. } => assert_eq!(mirrors.len(), 2),
            _ => panic!("第 2 个任务应是 generic"),
        }
    }

    #[test]
    fn empty_proxy_means_direct() {
        let cfg: Config = toml::from_str(
            "[network]\nproxy = \"\"\n[[task]]\nkind=\"mc\"\nversion=\"1\"\ndir=\"d\"\n",
        )
        .unwrap();
        assert!(cfg.proxy_filtered().is_none());
    }

    #[test]
    fn unknown_field_is_error() {
        let src = "[network]\nproxxy = \"x\"\n";
        let r: std::result::Result<Config, _> = toml::from_str(src);
        assert!(r.is_err(), "拼错字段名必须报错");
    }

    #[test]
    fn unknown_task_kind_is_error() {
        let src = "[[task]]\nkind = \"ftp\"\n";
        let r: std::result::Result<Config, _> = toml::from_str(src);
        assert!(r.is_err(), "未知 kind 必须报错");
    }

    #[test]
    fn empty_tasks_rejected() {
        let cfg: Config = toml::from_str("").unwrap();
        assert!(cfg.validate().is_err(), "没有 task 应报错");
    }

    #[test]
    fn generic_size_zero_rejected() {
        let src = r#"
[[task]]
kind = "generic"
url = "https://a/x"
dir = "d"
size = 0
"#;
        let cfg: Config = toml::from_str(src).unwrap();
        assert!(cfg.validate().is_err());
    }

    #[test]
    fn zero_concurrency_rejected() {
        let src = "[download]\nconcurrency = 0\n[[task]]\nkind=\"mc\"\nversion=\"1\"\ndir=\"d\"\n";
        let cfg: Config = toml::from_str(src).unwrap();
        assert!(cfg.validate().is_err());
    }

    #[test]
    fn mc_requires_fields() {
        // 缺 version 应是类型错误
        let r: std::result::Result<Config, _> =
            toml::from_str("[[task]]\nkind = \"mc\"\ndir = \"d\"\n");
        assert!(r.is_err());
    }
}
