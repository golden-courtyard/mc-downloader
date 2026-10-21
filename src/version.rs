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
    /// 文件大小（未知时为 None；有值且 > 8MB 才考虑分片）
    pub size: Option<u64>,
    /// 额外镜像（同内容的其它 URL）；空表示只用 `url`
    pub mirrors: Vec<String>,
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

pub async fn collect(client: &reqwest::Client, v: &VersionJson) -> Result<Vec<Job>> {
    let mut jobs = Vec::new();

    // ① 客户端 jar
    jobs.push(Job {
        url: v.downloads.client.url.clone(),
        path: format!("versions/{}/{}.jar", v.id, v.id),
        sha1: v.downloads.client.sha1.clone(),
        size: v.downloads.client.size,
        mirrors: Vec::new(),
    });

    // ② 依赖库（按 rules 过滤非 Windows 项）
    for lib in &v.libraries {
        if !allowed(&lib.rules) {
            continue;
        }
        let Some(d) = &lib.downloads else { continue };

        // 主 jar
        if let Some(a) = &d.artifact {
            if let Some(p) = a.path.clone() {
                jobs.push(Job {
                    url: a.url.clone(),
                    path: format!("libraries/{p}"),
                    sha1: a.sha1.clone(),
                    size: a.size,
                    mirrors: Vec::new(),
                });
            }
        }

        // ⭐ native 库（Windows）：旧版本（1.7.x / 1.12.x）必需，缺了启动会报 lwjgl.dll
        if let Some(cls) = &d.classifiers {
            for key in ["natives-windows", "natives-windows-64", "natives-windows-arm64"] {
                if let Some(nat) = cls.get(key) {
                    if let Some(p) = nat.path.clone() {
                        jobs.push(Job {
                            url: nat.url.clone(),
                            path: format!("libraries/{p}"),
                            sha1: nat.sha1.clone(),
                            size: nat.size,
                            mirrors: Vec::new(),
                        });
                    }
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
            size: None,
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
                url: format!("https://<你的源>/{a}/{h}"),
                path: format!("assets/objects/{a}/{h}"),
                sha1: Some(h.clone()),
                size: obj.size,
                mirrors: Vec::new(),
            });
        }
    }

    Ok(jobs)
}
