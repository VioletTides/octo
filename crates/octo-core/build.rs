//! Sets `OCTO_RELEASE_VERSION` for `octo_core::VERSION`: the release this build is, as the
//! dashboard, the User-Agent and the update check show it (`2026.10.05`).
//!
//! `OCTO_VERSION` in the build's environment wins (a CI or Docker build can stamp a release
//! without editing a file). Otherwise the repository's `VERSION` file holds the release number
//! (docker.yml checks a tag against it, and docker-compose.yml's image tags with it). A tree
//! without one falls back to `InformationalVersion` in `octo/octo.csproj`. `CARGO_PKG_VERSION`
//! cannot carry the tag: semver drops the leading zero of `05`, and the dashboard compares the
//! tag name verbatim.

use std::path::Path;

fn main() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let file = root.join("VERSION");
    let csproj = root.join("octo/octo.csproj");
    println!("cargo:rerun-if-env-changed=OCTO_VERSION");
    println!("cargo:rerun-if-changed={}", file.display());
    println!("cargo:rerun-if-changed={}", csproj.display());

    let version = match std::env::var("OCTO_VERSION") {
        Ok(v) if !v.trim().is_empty() => v.trim().to_string(),
        _ => match std::fs::read_to_string(&file) {
            Ok(text) => text.trim().to_string(),
            Err(_) => match std::fs::read_to_string(&csproj) {
                Ok(text) => informational_version(&text)
                    .unwrap_or_else(|| panic!("{} has no <InformationalVersion>", csproj.display())),
                Err(e) => panic!(
                    "neither {} nor {} can be read ({e}), and OCTO_VERSION is not set",
                    file.display(),
                    csproj.display()
                ),
            },
        },
    };
    if !is_release(&version) {
        panic!("the release version {version:?} is not a dated release like 2026.10.03 or 2026.10.03.2");
    }
    println!("cargo:rustc-env=OCTO_RELEASE_VERSION={version}");
}

/// The text of the first `<InformationalVersion>` element, as docker.yml's release check reads it.
fn informational_version(csproj: &str) -> Option<String> {
    let start = csproj.find("<InformationalVersion>")? + "<InformationalVersion>".len();
    let end = start + csproj[start..].find("</InformationalVersion>")?;
    Some(csproj[start..end].trim().to_string())
}

/// `yyyy.MM.dd` or `yyyy.MM.dd.N`, optionally followed by `+build` metadata (which the
/// User-Agent and the update check strip): the shape of a release tag.
fn is_release(version: &str) -> bool {
    let tag = version.split('+').next().unwrap_or_default();
    let parts: Vec<&str> = tag.split('.').collect();
    let widths_ok = match parts.as_slice() {
        [y, m, d] => y.len() == 4 && m.len() == 2 && d.len() == 2,
        [y, m, d, n] => y.len() == 4 && m.len() == 2 && d.len() == 2 && (1..=4).contains(&n.len()),
        _ => false,
    };
    widths_ok && parts.iter().all(|p| p.bytes().all(|b| b.is_ascii_digit()))
}
