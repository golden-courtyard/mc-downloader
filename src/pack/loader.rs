//! 加载器安装器：把 `Loader` 变成磁盘上可启动的 `versions/<id>/`。
//!
//! 加一种加载器 = 在这里加一个分支（外加它自己的安装实现），
//! 不影响解析层，也不影响文件铺设。

use anyhow::Result;
use std::path::Path;

use super::spec::Loader;
use crate::download::DownloadOpts;

/// 装好加载器，返回可用的实例 id（对应 `versions/<id>/`）。
pub async fn install(
    loader: &Loader,
    mc: &str,
    root: &Path,
    opts: &DownloadOpts,
) -> Result<String> {
    // 单一判定口：支持性只在这里说「不行」，下面的 match 只负责「怎么装」
    loader.check_supported()?;

    match loader {
        Loader::Vanilla => Ok(mc.to_string()),
        Loader::Fabric { loader } => crate::fabric::install(mc, loader, root, opts).await,
        // 走到这里说明 check_supported 与 match 两处判断脱节了 —— 响亮地报出来
        other => anyhow::bail!(
            "内部错误：{} 通过了支持性检查却没有对应实现（check_supported 与 install 不一致）",
            other.label()
        ),
    }
}
