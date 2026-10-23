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
    /// 下载原版；可选装加载器（`loader = "fabric"` 或 `"fabric:0.20.0"`）
    #[serde(rename = "mc")]
    Mc {
        version: String,
        dir: String,
        /// `fabric` / `fabric:<版本>`；缺省 = 只装原版
        #[serde(default)]
        loader: Option<String>,
    },
    /// 下载单个 URL
    #[serde(rename = "generic")]
    Generic {
        /// 单个 URL。与 `urls_file` **二选一**（都给会报错）。
        #[serde(default)]
        url: Option<String>,
        /// 列表文件：一行一个 URL（对齐 aria2 的 `-i`）。与 `url` 二选一。
        #[serde(default)]
        urls_file: Option<String>,
        dir: String,
        /// **仅 `url` 模式可用**：`urls_file` 下给了会报错（多文件用一个名字没有意义，
        /// 允许就等于静默不生效）
        #[serde(default)]
        name: Option<String>,
        /// **仅 `url` 模式可用**（同上）。省略则 HEAD 探测。
        #[serde(default)]
        size: Option<u64>,
        /// **仅 `url` 模式可用**（同上）：多文件无法表达"谁是同内容的镜像"。
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
            // generic 的 url / urls_file 二选一；urls_file 模式禁用单文件字段
            if let TaskSpec::Generic {
                url,
                urls_file,
                name,
                size,
                mirrors,
                ..
            } = t
            {
                match (url.is_some(), urls_file.is_some()) {
                    (true, true) => anyhow::bail!(
                        "第 {} 个任务同时给了 `url` 和 `urls_file`。这两个是二选一的，\
                         请删掉一个（同时给没法判断你要下几个文件）",
                        i + 1
                    ),
                    (false, false) => anyhow::bail!(
                        "第 {} 个任务既没有 `url` 也没有 `urls_file`。\
                         单个文件用 `url = \"...\"`，一批用 `urls_file = \"list.txt\"`",
                        i + 1
                    ),
                    _ => {}
                }
                if urls_file.is_some() {
                    let mut bad: Vec<&str> = Vec::new();
                    if name.is_some() {
                        bad.push("name");
                    }
                    if size.is_some() {
                        bad.push("size");
                    }
                    if !mirrors.is_empty() {
                        bad.push("mirrors");
                    }
                    if !bad.is_empty() {
                        anyhow::bail!(
                            "第 {} 个任务是 `urls_file` 模式，不能再给 `{}`。\
                             这些字段是“针对单个文件”的：一批文件共用一个名字/大小没有意义，\
                             mirrors 也无法表达“哪些 URL 是同一文件的镜像”。\
                             需要的话请拆成多个 `url` 任务",
                            i + 1,
                            bad.join("` / `")
                        );
                    }
                }
            }
        }
        Ok(())
    }

    /// 代理：空串视为直连
    pub fn proxy_filtered(&self) -> Option<String> {
        // 空串 / 全空白 = 显式直连。用 Option 的 filter+cloned 而不是 match：
        // 「取出、判条件、复制」本来就是这三个组合子的语义。
        // 这里刻意留 `trim()` 而非 `trim_ascii()`——代理串是用户手输的，
        // 万一带了全角空格也应当被吃掉，宽容一些。
        self.network
            .proxy
            .as_ref()
            .filter(|p| !p.trim().is_empty())
            .cloned()
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
