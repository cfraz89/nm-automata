//! Vendoring of the upstream NetworkManager D-Bus spec.
//!
//! Everything is pulled from freedesktop.org's GitLab at a release tag, so a regeneration is
//! reproducible from `spec/nm-version.txt` alone.

use std::path::Path;

use anyhow::{Context, Result, bail};
use serde::Deserialize;

/// The GitLab project, URL-encoded for the REST API.
const PROJECT: &str = "NetworkManager%2FNetworkManager";
const API: &str = "https://gitlab.freedesktop.org/api/v4/projects";
const RAW: &str = "https://gitlab.freedesktop.org/NetworkManager/NetworkManager/-/raw";

#[derive(Deserialize)]
struct Tag {
    name: String,
}

#[derive(Deserialize)]
struct TreeEntry {
    name: String,
    #[serde(rename = "type")]
    kind: String,
}

/// The newest stable release tag.
///
/// NetworkManager uses odd minor versions for development snapshots, so only even ones are
/// considered.
pub fn latest_stable_release() -> Result<String> {
    let tags: Vec<Tag> = get_json(&format!("{API}/{PROJECT}/repository/tags?per_page=100"))
        .context("listing NetworkManager release tags")?;

    let latest = tags
        .iter()
        .filter_map(|tag| parse_version(&tag.name).map(|v| (v, &tag.name)))
        .filter(|((_, minor, _), _)| minor % 2 == 0)
        .max();

    match latest {
        Some((_, name)) => Ok(name.clone()),
        None => bail!("no stable NetworkManager release tag found"),
    }
}

/// Download the D-Bus introspection XML of `version` into `spec/`.
///
/// The generated inputs — the introspection data and the settings documentation — are not
/// fetched: they do not exist in the git tree and are built by `just build-spec`.
///
/// Returns the version that was vendored.
pub fn vendor_spec(root: &Path, version: Option<&str>) -> Result<String> {
    let version = match version {
        Some(v) => v.to_owned(),
        None => latest_stable_release()?,
    };

    let spec = root.join("spec");
    let introspection = spec.join("introspection");
    // Interfaces are dropped upstream from time to time; start from an empty directory so
    // they disappear here too instead of lingering as stale files.
    if introspection.exists() {
        std::fs::remove_dir_all(&introspection)
            .with_context(|| format!("clearing {}", introspection.display()))?;
    }
    std::fs::create_dir_all(&introspection)
        .with_context(|| format!("creating {}", introspection.display()))?;

    let tree: Vec<TreeEntry> = get_json(&format!(
        "{API}/{PROJECT}/repository/tree?path=introspection&ref={version}&per_page=100"
    ))
    .context("listing the introspection directory")?;

    let mut count = 0;
    for entry in &tree {
        if entry.kind != "blob" || !entry.name.ends_with(".xml") {
            continue;
        }
        let body = get_text(&format!("{RAW}/{version}/introspection/{}", entry.name))
            .with_context(|| format!("downloading {}", entry.name))?;
        std::fs::write(introspection.join(&entry.name), body)?;
        count += 1;
    }
    if count == 0 {
        bail!("no introspection XML found at tag {version}");
    }

    std::fs::write(spec.join("nm-version.txt"), format!("{version}\n"))?;

    Ok(version)
}

/// The version that `spec/` was vendored from.
pub fn vendored_version(root: &Path) -> Result<String> {
    let path = root.join("spec/nm-version.txt");
    let version = std::fs::read_to_string(&path)
        .with_context(|| format!("reading {} — run `just fetch` first", path.display()))?;
    Ok(version.trim().to_owned())
}

fn parse_version(tag: &str) -> Option<(u32, u32, u32)> {
    let mut parts = tag.split('.');
    let major = parts.next()?.parse().ok()?;
    let minor = parts.next()?.parse().ok()?;
    let patch = parts.next()?.parse().ok()?;
    parts.next().is_none().then_some((major, minor, patch))
}

fn get_text(url: &str) -> Result<String> {
    let body = ureq::get(url)
        .call()
        .with_context(|| format!("GET {url}"))?
        .body_mut()
        .read_to_string()?;
    Ok(body)
}

fn get_json<T: serde::de::DeserializeOwned>(url: &str) -> Result<T> {
    let value = ureq::get(url)
        .call()
        .with_context(|| format!("GET {url}"))?
        .body_mut()
        .read_json()?;
    Ok(value)
}
