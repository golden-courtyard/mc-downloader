use anyhow::{bail, Result};
use serde::Deserialize;

const URL: &str = "https://<你的源>/mc/game/version_manifest_v2.json";

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

pub async fn fetch() -> Result<Manifest> {
    Ok(reqwest::get(URL).await?.json().await?)
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
