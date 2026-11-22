use anyhow::{Result, bail};
use serde::Deserialize;

#[derive(Debug, Deserialize)]
pub struct Manifest {
    pub versions: Vec<Ver>,
}

#[derive(Debug, Deserialize, Clone)]
pub struct Ver {
    pub id: String,
    pub url: String,
    #[serde(rename = "type")]
    pub kind: String,
}

/// `url` 是用户配的版本清单地址（`[source].metadata`）。**没有默认值**（issue #15）。
pub async fn fetch(client: &reqwest::Client, url: &str) -> Result<Manifest> {
    Ok(client
        .get(url)
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?)
}

pub fn find<'a>(m: &'a Manifest, id: &str) -> Result<&'a Ver> {
    if let Some(v) = m.versions.iter().find(|v| v.id == id) {
        return Ok(v);
    }
    let hint: Vec<&str> = m
        .versions
        .iter()
        .filter(|v| v.kind == "release")
        .take(8)
        .map(|v| v.id.as_str())
        .collect();
    bail!("找不到版本 {}。最近的正式版: {}", id, hint.join(", "))
}
