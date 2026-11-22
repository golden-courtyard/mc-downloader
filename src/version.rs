use anyhow::Result;
use serde::Deserialize;
use std::collections::HashMap;

#[derive(Debug, Deserialize)]
pub struct VersionJson {
    pub id: String,
    pub downloads: Downloads,
    #[serde(default)]
    pub libraries: Vec<Library>,
    #[serde(rename = "assetIndex")]
    pub asset_index: Option<AssetIndex>,
}

#[derive(Debug, Deserialize)]
pub struct Downloads {
    pub client: Artifact,
}

#[derive(Debug, Deserialize, Clone)]
pub struct Artifact {
    pub url: String,
    #[serde(default)]
    pub path: Option<String>,
    #[serde(default)]
    pub sha1: Option<String>,
    #[serde(default)]
    pub size: Option<u64>,
}

#[derive(Debug, Deserialize)]
pub struct Library {
    #[serde(default)]
    pub downloads: Option<LibDownloads>,
    #[serde(default)]
    pub rules: Vec<Rule>,
}

#[derive(Debug, Deserialize)]
pub struct LibDownloads {
    #[serde(default)]
    pub artifact: Option<Artifact>,
    #[serde(default)]
    pub classifiers: Option<HashMap<String, Artifact>>,
}

#[derive(Debug, Deserialize)]
pub struct Rule {
    pub action: String,
    #[serde(default)]
    pub os: Option<OsRule>,
}

#[derive(Debug, Deserialize)]
pub struct OsRule {
    #[serde(default)]
    pub name: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct AssetIndex {
    pub url: String,
    pub id: String,
}

#[derive(Debug, Deserialize)]
struct AssetIndexJson {
    objects: HashMap<String, AssetObj>,
}

#[derive(Debug, Deserialize)]
struct AssetObj {
    hash: String,
    #[serde(default)]
    size: Option<u64>,
}

#[derive(Debug, Clone)]
pub struct Job {
    pub url: String,
    /// 相对 .minecraft 的路径
    pub path: String,
    pub sha1: Option<String>,
    /// 文件大小。**三种真值等级，别当成一个数用** —— 见 [`SizeHint`]
    pub size: SizeHint,
    /// 额外镜像（同内容的其它 URL）；空表示只用 `url`
    pub mirrors: Vec<String>,
}

/// `Job.size` 的三种真值等级。
///
/// 审查（C1）指出的问题是：一个 `Option<u64>` 同时被当成三种东西用 ——
/// ① 决定"分片还是单流"（**提示**就够）；② 进度条的总长（估计）；
/// ③ `Content-Range` 的总长比对（**必须权威**，用来抓"远端对象中途被换掉"）。
///
/// 于是 `fetch -s 12M` 这种"用户随手填的提示"会走进第 ③ 种用法：每片总长比对必失败 ⇒
/// 重试换源 ⇒ 单源时立刻耗尽 ⇒ 报错变成"分片 0 无可用源"，指向网络，而根因在用户填的参数。
///
/// 分开表达后，**只有 manifest / API 给的 `Exact` 才参与比对**。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SizeHint {
    /// 来自 manifest / API：完整且权威
    Exact(u64),
    /// 用户给的提示（`--size`、脚本里的 `size`）：**只**用来决定分片还是单流
    Hint(u64),
    /// 未知（服务器没给 `Content-Length`，如 chunked）
    Unknown,
}

impl SizeHint {
    /// 字节数（提示也算），用于"分片还是单流"与进度总长。
    /// manifest / API 给的值 ⇒ Exact；没有 ⇒ Unknown。--size 之类**不该**走这里。
    pub fn from_exact(v: Option<u64>) -> Self {
        v.map_or(Self::Unknown, Self::Exact)
    }

    pub fn bytes(self) -> Option<u64> {
        match self {
            Self::Exact(n) | Self::Hint(n) => Some(n),
            Self::Unknown => None,
        }
    }

    /// 只有权威值才返回 —— 这是 `Content-Range` 校验的输入。
    pub fn exact(self) -> Option<u64> {
        match self {
            Self::Exact(n) => Some(n),
            _ => None,
        }
    }
}

/// 返回解析结果 + 原始 JSON 文本（后者要原样落到 versions/<id>/<id>.json）
pub async fn fetch(client: &reqwest::Client, url: &str) -> Result<(VersionJson, String)> {
    let text = client
        .get(url)
        .send()
        .await?
        .error_for_status()?
        .text()
        .await?;
    let v: VersionJson = serde_json::from_str(&text)?;
    Ok((v, text))
}

/// 规则匹配：无 rules => 允许；有 rules => 最后一条匹配项的决定
fn allowed(rules: &[Rule]) -> bool {
    if rules.is_empty() {
        return true;
    }
    let mut ok = false;
    for r in rules {
        let matched = match &r.os {
            None => true,
            Some(os) => os.name.as_deref().map(|n| n == "windows").unwrap_or(true),
        };
        if matched {
            ok = r.action == "allow";
        }
    }
    ok
}

pub async fn collect(
    client: &reqwest::Client,
    v: &VersionJson,
    assets_base: &str,
) -> Result<Vec<Job>> {
    let mut jobs = Vec::new();

    // ① 客户端 jar
    jobs.push(Job {
        url: v.downloads.client.url.clone(),
        path: format!("versions/{}/{}.jar", v.id, v.id),
        sha1: v.downloads.client.sha1.clone(),
        size: SizeHint::from_exact(v.downloads.client.size),
        mirrors: Vec::new(),
    });

    // ② 依赖库（按 rules 过滤非 Windows 项）
    for lib in &v.libraries {
        if !allowed(&lib.rules) {
            continue;
        }
        let Some(d) = &lib.downloads else { continue };

        // 主 jar
        if let Some(a) = &d.artifact
            && let Some(p) = a.path.clone()
        {
            jobs.push(Job {
                url: a.url.clone(),
                path: format!("libraries/{p}"),
                sha1: a.sha1.clone(),
                size: SizeHint::from_exact(a.size),
                mirrors: Vec::new(),
            });
        }

        // ⭐ native 库（Windows）：旧版本（1.7.x / 1.12.x）必需，缺了启动会报 lwjgl.dll
        if let Some(cls) = &d.classifiers {
            for key in [
                "natives-windows",
                "natives-windows-64",
                "natives-windows-arm64",
            ] {
                if let Some(nat) = cls.get(key)
                    && let Some(p) = nat.path.clone()
                {
                    jobs.push(Job {
                        url: nat.url.clone(),
                        path: format!("libraries/{p}"),
                        sha1: nat.sha1.clone(),
                        size: SizeHint::from_exact(nat.size),
                        mirrors: Vec::new(),
                    });
                }
            }
        }
    }

    // ③ 资源索引 + 资源对象
    if let Some(ai) = &v.asset_index {
        jobs.push(Job {
            url: ai.url.clone(),
            path: format!("assets/indexes/{}.json", ai.id),
            sha1: None,
            size: SizeHint::Unknown,
            mirrors: Vec::new(),
        });

        let idx: AssetIndexJson = client
            .get(&ai.url)
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;
        for obj in idx.objects.values() {
            let h = &obj.hash;
            if h.len() < 2 {
                continue;
            }
            let (a, _) = h.split_at(2);
            jobs.push(Job {
                url: format!("{assets_base}/{a}/{h}"),
                path: format!("assets/objects/{a}/{h}"),
                sha1: Some(h.clone()),
                size: SizeHint::from_exact(obj.size),
                mirrors: Vec::new(),
            });
        }
    }

    Ok(jobs)
}

#[cfg(test)]
mod size_hint_tests {
    use super::SizeHint;

    #[test]
    fn bytes_accepts_hint_but_exact_does_not() {
        assert_eq!(SizeHint::Exact(7).bytes(), Some(7));
        assert_eq!(SizeHint::Hint(7).bytes(), Some(7));
        assert_eq!(SizeHint::Unknown.bytes(), None);
    }

    /// 这条是 C1 的核心：**只有** `Exact` 会进 `Content-Range` 比对。
    /// 若哪天有人把 `exact()` 改成"有字节就返回"，`--size` 填错就又会变成必然失败。
    #[test]
    fn exact_is_the_only_authoritative_variant() {
        assert_eq!(SizeHint::Exact(7).exact(), Some(7));
        assert_eq!(SizeHint::Hint(7).exact(), None);
        assert_eq!(SizeHint::Unknown.exact(), None);
    }

    #[test]
    fn from_exact_marks_manifest_values_authoritative() {
        assert_eq!(SizeHint::from_exact(Some(9)), SizeHint::Exact(9));
        assert_eq!(SizeHint::from_exact(None), SizeHint::Unknown);
    }
}
