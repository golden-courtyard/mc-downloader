//! 整合包的**中间表示**。
//!
//! 所有格式（.mrpack / 未来的 CurseForge）都先解析成 `PackSpec`，
//! 下游流水线只认这个结构。加一种格式 = 加一个 parser，加一种加载器 = 加一个 installer，
//! 两者互不干扰。

use anyhow::{bail, Result};

/// 一个要被下载到实例目录的文件
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PackEntry {
    /// 相对实例根目录的落地路径（用 `/` 分隔，落盘时再转平台分隔符）
    pub path: String,
    pub url: String,
    pub sha1: Option<String>,
    pub size: Option<u64>,
}

/// 模组加载器
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Loader {
    /// 纯原版，不装加载器
    Vanilla,
    Fabric { loader: String },
    Forge { version: String },
    NeoForge { version: String },
}

impl Loader {
    /// 提前判定能不能装。
    ///
    /// **必须在下载原版之前调用** —— 否则要下完几个 GB 才告诉用户「不支持」。
    pub fn check_supported(&self) -> Result<()> {
        match self {
            Self::Vanilla | Self::Fabric { .. } => Ok(()),
            Self::Forge { version } => bail!(
                "暂不支持 Forge（整合包要求 {version}）。\
                 当前只做原版与 Fabric —— Forge/NeoForge 需要跑官方 installer，尚未实现。"
            ),
            Self::NeoForge { version } => bail!(
                "暂不支持 NeoForge（整合包要求 {version}）。\
                 当前只做原版与 Fabric —— Forge/NeoForge 需要跑官方 installer，尚未实现。"
            ),
        }
    }

    pub fn label(&self) -> String {
        match self {
            Self::Vanilla => "原版".to_string(),
            Self::Fabric { loader } => format!("Fabric {loader}"),
            Self::Forge { version } => format!("Forge {version}"),
            Self::NeoForge { version } => format!("NeoForge {version}"),
        }
    }
}

/// 解析后的整合包
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PackSpec {
    pub name: String,
    /// 目标 MC 版本
    pub mc: String,
    pub loader: Loader,
    /// 要下载的文件（已剔除 `client = unsupported` 的条目）
    pub files: Vec<PackEntry>,
    /// 需要原样铺到实例目录的归档内前缀
    pub override_prefixes: Vec<String>,
    /// 因 `env.client = unsupported` 被跳过的条目数（只为日志）
    pub skipped_unsupported: usize,
}

impl PackSpec {
    /// 规范化后的构造：保证 `override_prefixes` 非空、版本号已 trim
    pub fn new(
        name: impl Into<String>,
        mc: impl Into<String>,
        loader: Loader,
        files: Vec<PackEntry>,
        override_prefixes: Vec<String>,
        skipped_unsupported: usize,
    ) -> Self {
        let mut prefixes = override_prefixes;
        if prefixes.is_empty() {
            prefixes = vec!["overrides/".to_string(), "client-overrides/".to_string()];
        }
        Self {
            name: name.into(),
            mc: mc.into().trim().to_string(),
            loader,
            files,
            override_prefixes: prefixes,
            skipped_unsupported,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fabric_is_supported() {
        let l = Loader::Fabric {
            loader: "0.16.5".into(),
        };
        assert!(l.check_supported().is_ok());
        assert_eq!(l.label(), "Fabric 0.16.5");
    }

    #[test]
    fn forge_is_rejected_with_actionable_message() {
        let e = Loader::Forge {
            version: "47.2.0".into(),
        }
        .check_supported()
        .unwrap_err()
        .to_string();
        assert!(e.contains("Forge"), "报错要点名：{e}");
        assert!(e.contains("47.2.0"), "报错要带上要求版本：{e}");
        assert!(e.contains("尚未实现"), "报错要说明原因：{e}");
    }

    #[test]
    fn neoforge_is_rejected() {
        let l = Loader::NeoForge {
            version: "21.1.0".into(),
        };
        assert!(l.check_supported().is_err());
    }

    #[test]
    fn spec_trims_mc_and_fills_default_prefixes() {
        let s = PackSpec::new("测试包", " 1.21.1 ", Loader::Vanilla, vec![], vec![], 0);
        assert_eq!(s.mc, "1.21.1");
        assert_eq!(
            s.override_prefixes,
            vec!["overrides/".to_string(), "client-overrides/".to_string()]
        );
    }

    #[test]
    fn spec_keeps_custom_prefixes() {
        let s = PackSpec::new(
            "x",
            "1.21",
            Loader::Vanilla,
            vec![],
            vec!["overrides/client/".into()],
            2,
        );
        assert_eq!(s.override_prefixes, vec!["overrides/client/".to_string()]);
        assert_eq!(s.skipped_unsupported, 2);
    }
}
