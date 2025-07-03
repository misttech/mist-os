// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::{bail, Context, Result};
use assembly_config_schema::Architecture;
use camino::Utf8PathBuf;
use serde::Deserialize;
use std::str::FromStr;

/// An artifact reference.
#[derive(Debug, PartialEq)]
pub enum Artifact {
    /// A artifact that lives on the local host.
    Local(Utf8PathBuf),

    /// An artifact found in a CIPD package.
    CIPD(CIPDPackage),

    /// An artifact known by MOS.
    MOS(MOSIdentifier),
}

/// A reference to an artifact in CIPD.
#[derive(Debug, PartialEq)]
pub struct CIPDPackage {
    pub path: Utf8PathBuf,
    pub tag: String,
}

impl std::fmt::Display for CIPDPackage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "cipd://{}@{}", self.path, self.tag)
    }
}

/// A reference to an artifact known by MOS.
#[derive(Debug, PartialEq)]
pub struct MOSIdentifier {
    pub repo: String,
    pub version: String,
    pub name: String,
}

#[derive(Deserialize)]
struct BuildApiEntry {
    name: String,
    outdir: Utf8PathBuf,
}

#[derive(Deserialize)]
struct PlatformBuildApiEntry {
    path: Utf8PathBuf,
}

impl Artifact {
    /// Construct an artifact from a string.
    pub fn from_product_or_board_string(s: impl AsRef<str>) -> Result<Self> {
        if let Some(artifact) = parse_cipd(&s)? {
            return Ok(artifact);
        }

        if let Some(artifact) = parse_local(&s)? {
            return Ok(artifact);
        }

        bail!("Artifact must be either a CIPD package or a local path: {}", s.as_ref());
    }

    /// Construct an artifact from a local board name.
    pub fn from_local_board_name(
        name: impl AsRef<str>,
        build_dir: Option<&Utf8PathBuf>,
    ) -> Result<Self> {
        let build_dir = build_dir
            .context("identifying boards by their name can only be done in a fuchsia checkout")?;
        let build_api = build_dir.join("boards.json");
        let build_api_file =
            std::fs::File::open(&build_api).with_context(|| format!("Opening: {}", &build_api))?;
        let boards: Vec<BuildApiEntry> = serde_json::from_reader(build_api_file)
            .with_context(|| format!("Parsing: {}", &build_api))?;
        let board = boards
            .iter()
            .find(|b| b.name == name.as_ref())
            .with_context(|| format!("searching for board: {}", name.as_ref()))
            .with_context(|| format!("searching build api: {}", &build_api))?;
        let board_path = build_dir.join(&board.outdir);
        Ok(Artifact::Local(board_path))
    }

    /// Construct an artifact from a local product name.
    pub fn from_local_product_name(
        name: impl AsRef<str>,
        build_dir: Option<&Utf8PathBuf>,
    ) -> Result<Self> {
        // TODO: provide nice error when local artifact is not found that
        // indicates how to build it.
        let build_dir = build_dir
            .context("identifying products by their name can only be done in a fuchsia checkout")?;
        let build_api = build_dir.join("products.json");
        let build_api_file =
            std::fs::File::open(&build_api).with_context(|| format!("Opening: {}", &build_api))?;
        let products: Vec<BuildApiEntry> = serde_json::from_reader(build_api_file)
            .with_context(|| format!("Parsing: {}", &build_api))?;
        let product = products
            .iter()
            .find(|p| p.name == name.as_ref())
            .with_context(|| format!("searching for product: {}", name.as_ref()))
            .with_context(|| format!("searching build api: {}", &build_api))?;
        let product_path = build_dir.join(&product.outdir);
        Ok(Artifact::Local(product_path))
    }

    /// Construct an artifact from an optionally-specified platform.
    /// If None, this returns the default local path in a fuchsia checkout.
    /// Otherwise, it parses it as a CIPD or local path.
    pub fn from_platform(
        platform: Option<String>,
        arch: &Architecture,
        build_dir: Option<&Utf8PathBuf>,
    ) -> Result<Self> {
        if let Some(platform) = platform {
            if let Some(artifact) = parse_cipd(&platform)? {
                return Ok(artifact);
            }

            if let Some(artifact) = parse_local(&platform)? {
                return Ok(artifact);
            }

            // Assume the input is a tag to the default CIPD location.
            let path = Utf8PathBuf::from_str("fuchsia/assembly/platform").unwrap();
            let path = path.join(arch.to_string());
            let tag = version_to_cipd_tag(format!("version:{}", platform));
            Ok(Artifact::CIPD(CIPDPackage { path, tag }))
        } else {
            let build_dir = build_dir
                .context("could not find environment variable BUILD_DIR")
                .context("--platform is required outside a fuchsia checkout")?;
            let build_api = build_dir.join("platform_artifacts.json");
            let build_api_file = std::fs::File::open(&build_api)
                .with_context(|| format!("Opening: {}", &build_api))?;
            let platform_artifacts: Vec<PlatformBuildApiEntry> =
                serde_json::from_reader(build_api_file)
                    .with_context(|| format!("Parsing: {}", &build_api))?;
            let platform_artifacts = platform_artifacts
                .first()
                .context("searching for platform artifacts")
                .with_context(|| format!("searching build api: {}", &build_api))?;
            let platform_artifacts_path = build_dir.join(&platform_artifacts.path);
            Ok(Artifact::Local(platform_artifacts_path))
        }
    }
}

fn parse_cipd(s: impl AsRef<str>) -> Result<Option<Artifact>> {
    let s = s.as_ref();
    if let Some(cipd_path_and_version) = s.strip_prefix("cipd://") {
        let (path, version) = cipd_path_and_version
            .split_once("@")
            .with_context(|| format!("Artifact is missing a version: {}", &s))?;
        let path = Utf8PathBuf::from_str(path)
            .with_context(|| format!("Artifact path is not utf8: {}", &s))?;
        let tag = version_to_cipd_tag(version);
        Ok(Some(Artifact::CIPD(CIPDPackage { path, tag })))
    } else {
        Ok(None)
    }
}

fn version_to_cipd_tag(version: impl AsRef<str>) -> String {
    if version.as_ref() == "latest" {
        return "latest".into();
    }
    // TODO: verify version format.
    version.as_ref().to_string()
}

fn parse_local(s: impl AsRef<str>) -> Result<Option<Artifact>> {
    let s = s.as_ref();
    if s.contains("/") || std::fs::exists(s)? {
        // TODO: validate the path exists.
        let path =
            Utf8PathBuf::from_str(s).with_context(|| format!("Local path is not utf8: {}", &s))?;
        Ok(Some(Artifact::Local(path)))
    } else {
        Ok(None)
    }
}

#[cfg(test)]
mod tests {
    use super::{Artifact, CIPDPackage};
    use assembly_config_schema::Architecture;
    use camino::Utf8PathBuf;
    use std::fs::File;
    use tempfile::tempdir;

    #[test]
    fn test_cipd_platform() {
        assert_eq!(
            Artifact::CIPD(CIPDPackage {
                path: "path/to/platform".into(),
                tag: "version:1.2.3.4".into()
            }),
            Artifact::from_platform(
                Some("cipd://path/to/platform@version:1.2.3.4".into()),
                &Architecture::X64,
                None
            )
            .unwrap(),
        );
    }

    #[test]
    fn test_default_cipd_platform() {
        assert_eq!(
            Artifact::CIPD(CIPDPackage {
                path: "fuchsia/assembly/platform/x64".into(),
                tag: "version:tag".into()
            }),
            Artifact::from_platform(Some("tag".into()), &Architecture::X64, None).unwrap(),
        );

        assert_eq!(
            Artifact::CIPD(CIPDPackage {
                path: "fuchsia/assembly/platform/arm64".into(),
                tag: "version:tag".into()
            }),
            Artifact::from_platform(Some("tag".into()), &Architecture::ARM64, None).unwrap(),
        );
    }

    #[test]
    fn test_local_platform() {
        assert_eq!(
            Artifact::Local("path/to/platform".into()),
            Artifact::from_platform(Some("path/to/platform".into()), &Architecture::X64, None)
                .unwrap(),
        );
    }

    #[test]
    fn test_default_local_platform() {
        let tmp_dir = tempdir().unwrap();
        let tmp_path = Utf8PathBuf::from_path_buf(tmp_dir.path().to_path_buf()).unwrap();

        let platform_artifacts_json = tmp_path.join("platform_artifacts.json");
        let platform_artifacts_file = File::create(&platform_artifacts_json).unwrap();
        let platform_artifacts = serde_json::json!([
            {
                "path": "path/to/local/platform",
            }
        ]);
        serde_json::to_writer(&platform_artifacts_file, &platform_artifacts).unwrap();

        assert_eq!(
            Artifact::Local(tmp_path.join("path/to/local/platform")),
            Artifact::from_platform(None, &Architecture::X64, Some(&tmp_path)).unwrap(),
        );
    }

    #[test]
    fn test_cipd_artifact() {
        assert_eq!(
            Artifact::CIPD(CIPDPackage {
                path: "path/to/artifact".into(),
                tag: "version:1.2.3.4".into()
            }),
            Artifact::from_product_or_board_string(
                "cipd://path/to/artifact@version:1.2.3.4".to_string(),
            )
            .unwrap(),
        )
    }

    #[test]
    fn test_local_product() {
        let tmp_dir = tempdir().unwrap();
        let tmp_path = Utf8PathBuf::from_path_buf(tmp_dir.path().to_path_buf()).unwrap();

        let products_json = tmp_path.join("products.json");
        let products_file = File::create(&products_json).unwrap();
        let products = serde_json::json!([
            {
                "name": "product_a",
                "outdir": "path/to/local/product",
            }
        ]);
        serde_json::to_writer(&products_file, &products).unwrap();

        assert_eq!(
            Artifact::Local(tmp_path.join("path/to/local/product")),
            Artifact::from_local_product_name("product_a", Some(&tmp_path)).unwrap()
        );
    }

    #[test]
    fn test_local_board() {
        let tmp_dir = tempdir().unwrap();
        let tmp_path = Utf8PathBuf::from_path_buf(tmp_dir.path().to_path_buf()).unwrap();

        let boards_json = tmp_path.join("boards.json");
        let boards_file = File::create(&boards_json).unwrap();
        let boards = serde_json::json!([
            {
                "name": "board_a",
                "outdir": "path/to/local/board",
            }
        ]);
        serde_json::to_writer(&boards_file, &boards).unwrap();

        assert_eq!(
            Artifact::Local(tmp_path.join("path/to/local/board")),
            Artifact::from_local_board_name("board_a", Some(&tmp_path)).unwrap()
        );
    }
}
