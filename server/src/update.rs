//! Updates from the project's GitHub releases. Each release carries the
//! capture server on its own (`misterkvms-capture-<version>-win-x64.exe`),
//! with the SHA-256 GitHub records for every asset. The service downloads
//! it into its Program Files folder (only administrators can write there,
//! so nobody can swap the file before it runs as LocalSystem), checks the
//! hash, and reinstalls itself from it.

use std::io::Read;
use std::path::Path;

use serde::Serialize;

const REPO: &str = "danifunker/MiSTerKVMS";

/// This build's version: the release's (CI sets MISTERKVMS_VERSION), else
/// Cargo.toml's.
pub fn version() -> &'static str {
    option_env!("MISTERKVMS_VERSION").filter(|v| !v.is_empty()).unwrap_or(env!("CARGO_PKG_VERSION"))
}

fn parse(v: &str) -> Option<(u32, u32, u32)> {
    let mut parts = v.trim().trim_start_matches('v').split('.').map(|p| p.parse::<u32>().ok());
    Some((parts.next()??, parts.next().flatten().unwrap_or(0), parts.next().flatten().unwrap_or(0)))
}

/// Is version `a` newer than `b`?
pub fn newer(a: &str, b: &str) -> bool {
    matches!((parse(a), parse(b)), (Some(a), Some(b)) if a > b)
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Latest {
    pub current: String,
    pub latest: String,
    pub newer: bool,
    /// The release page, for reading what changed.
    pub page: String,
    /// The capture server for this OS, if the release has it.
    #[serde(skip)]
    pub asset: Option<(String, String)>,
    /// Whether this copy can update itself from the page.
    pub can_apply: bool,
}

fn asset_name(version: &str) -> String {
    format!("misterkvms-capture-{version}-win-x64.exe")
}

fn agent() -> ureq::Agent {
    ureq::Agent::config_builder().timeout_global(Some(std::time::Duration::from_secs(120))).build().into()
}

/// Ask GitHub for the latest release.
pub fn latest() -> Result<Latest, String> {
    let url = format!("https://api.github.com/repos/{REPO}/releases/latest");
    let release: serde_json::Value = agent()
        .get(&url)
        .header("User-Agent", &format!("misterkvms-capture/{}", version()))
        .header("Accept", "application/vnd.github+json")
        .call()
        .map_err(|e| format!("can't reach GitHub: {e}"))?
        .body_mut()
        .read_json()
        .map_err(|e| format!("GitHub's answer: {e}"))?;
    let tag = release["tag_name"].as_str().ok_or("GitHub's answer has no version")?;
    let latest = tag.trim_start_matches('v').to_string();
    let wanted = asset_name(&latest);
    let asset = release["assets"].as_array().and_then(|assets| {
        assets.iter().find(|a| a["name"].as_str() == Some(wanted.as_str())).and_then(|a| {
            let url = a["browser_download_url"].as_str()?.to_string();
            let digest = a["digest"].as_str()?.strip_prefix("sha256:")?.to_lowercase();
            Some((url, digest))
        })
    });
    let newer = newer(&latest, version());
    Ok(Latest {
        current: version().to_string(),
        newer,
        page: release["html_url"].as_str().unwrap_or_default().to_string(),
        can_apply: cfg!(windows) && crate::service::running_as_service() && asset.is_some(),
        asset,
        latest,
    })
}

/// Download the latest capture server and reinstall the service from it.
/// Only from the service: the helper it starts stops this process.
pub fn apply(dir: &Path) -> Result<String, String> {
    let latest = latest()?;
    if !latest.newer {
        return Err(format!("already up to date ({})", latest.current));
    }
    if !crate::service::running_as_service() {
        return Err("updates from the page work when running as a service; download the new version from the release page".into());
    }
    let (url, sha256) = latest.asset.clone().ok_or("this release has no capture server download for Windows (with a checksum)")?;
    let target = crate::service::update_download_path().ok_or("updates from the page are Windows-only for now")?;
    crate::log(&format!("update: downloading {} from {url}", latest.latest));
    let mut body = Vec::new();
    agent()
        .get(&url)
        .header("User-Agent", &format!("misterkvms-capture/{}", version()))
        .call()
        .map_err(|e| format!("download failed: {e}"))?
        .body_mut()
        .as_reader()
        .take(64 << 20)
        .read_to_end(&mut body)
        .map_err(|e| format!("download failed: {e}"))?;
    let got = sha256_hex(&body);
    if got != sha256 {
        return Err(format!("the download doesn't match GitHub's checksum (got {got}, expected {sha256})"));
    }
    std::fs::write(&target, &body).map_err(|e| format!("{}: {e}", target.display()))?;
    crate::log(&format!("update: {} downloaded and checked; reinstalling", latest.latest));
    crate::service::install_from(&target, dir)?;
    Ok(latest.latest)
}

fn sha256_hex(data: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    Sha256::digest(data).iter().map(|b| format!("{b:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions_compare_numerically() {
        assert!(newer("0.9.10", "0.9.9"));
        assert!(newer("v1.0.0", "0.9.42"));
        assert!(!newer("0.9.3", "0.9.3"));
        assert!(!newer("0.9.2", "0.9.3"));
        assert!(!newer("junk", "0.9.3"));
    }

    #[test]
    fn sha256_known_value() {
        assert_eq!(sha256_hex(b"abc"), "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad");
    }
}
