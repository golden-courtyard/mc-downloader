//! 把 Minecraft 相关文件放到正确位置（下载源由用户在 `[source]` 配置）。
//!
//! 拆成 lib + bin 两个 target：`main.rs` 只做 CLI 解析与分派，
//! 逻辑全部住在这里，**为的是可测**——bin crate 的模块无法写集成测试。

pub mod config;
pub mod curseforge;
pub mod download;
pub mod fabric;
pub mod forge;
pub mod httpname;
pub mod manifest;
pub mod motw;
pub mod neoforge;
pub mod pack;
pub mod pathsafe;
pub mod piece;
pub mod progress;
pub mod scheduler;
pub mod settings;
pub mod source;
pub mod url_list;
pub mod vanilla;
pub mod version;
