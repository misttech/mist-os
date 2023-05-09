// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::{anyhow, Context, Result};
use errors::{ffx_bail, ffx_error};
use sdk_metadata::{CpuArchitecture, ElementType, FfxTool, HostTool, Manifest, Part};
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use std::{
    collections::HashMap,
    fs,
    io::BufReader,
    path::{Path, PathBuf},
};
use tracing::warn;

pub const SDK_MANIFEST_PATH: &str = "meta/manifest.json";
pub const SDK_BUILD_MANIFEST_PATH: &str = "sdk/manifest/core";

/// Current "F" milestone for Fuchsia (e.g. F38).
const MILESTONE: &'static str = include_str!("../../../../../../integration/MILESTONE");

#[derive(Debug, PartialEq, Eq)]
pub enum SdkVersion {
    Version(String),
    InTree,
    Unknown,
}

#[derive(Debug)]
pub struct Sdk {
    path_prefix: PathBuf,
    parts: Vec<Part>,
    real_paths: Option<HashMap<String, String>>,
    version: SdkVersion,
}

#[derive(Debug)]
pub struct FfxToolFiles {
    /// How "specific" this definition is, in terms of how many of the
    /// relevant paths came from arch specific definitions:
    /// - 0: Platform independent, no arch specific paths.
    /// - 1: One of the paths came from an arch specific section.
    /// - 2: Both the paths came from arch specific sections.
    /// This allows for easy sorting of tool files by how specific
    /// they are.
    pub specificity_score: usize,
    /// The actual executable binary to run
    pub executable: PathBuf,
    /// The path to the FHO metadata file
    pub metadata: PathBuf,
}

#[derive(Clone, Debug)]
pub enum SdkRoot {
    Modular { manifest: PathBuf, module: String },
    Full(PathBuf),
}

/// A serde-serializable representation of ffx' sdk configuration.
#[derive(Default, Clone, Debug, Serialize, Deserialize)]
pub struct FfxSdkConfig {
    pub root: Option<PathBuf>,
    pub module: Option<String>,
}

#[derive(Deserialize)]
struct SdkAtoms {
    #[cfg(test)]
    ids: Vec<serde_json::Value>,
    atoms: Vec<Atom>,
}

#[derive(Deserialize, Debug)]
struct Atom {
    #[cfg(test)]
    category: String,
    #[cfg(test)]
    deps: Vec<String>,
    files: Vec<File>,
    #[serde(rename = "gn-label")]
    #[cfg(test)]
    gn_label: String,
    #[cfg(test)]
    id: String,
    meta: String,
    #[serde(rename = "type")]
    kind: ElementType,
}

#[derive(Deserialize, Debug)]
struct File {
    destination: String,
    source: String,
}

impl SdkRoot {
    /// Returns true if the given path appears to be an sdk root.
    pub fn is_sdk_root(path: &Path) -> bool {
        path.join(SDK_MANIFEST_PATH).exists() || path.join(SDK_BUILD_MANIFEST_PATH).exists()
    }

    /// Does a full load of the sdk configuration.
    pub fn get_sdk(self) -> Result<Sdk> {
        tracing::debug!("get_sdk");
        match self {
            Self::Modular { manifest, module } => {
                // Modular only ever makes sense as part of a build directory
                // sdk, so there's no need to figure out what kind it is.
                Sdk::from_build_dir(&manifest, Some(&module)).with_context(|| {
                    anyhow!(
                        "Loading sdk manifest at `{}` with module `{module}`",
                        manifest.display()
                    )
                })
            }
            Self::Full(manifest) if manifest.join(SDK_MANIFEST_PATH).exists() => {
                // If the packaged sdk manifest exists, use that.
                Sdk::from_sdk_dir(&manifest)
                    .with_context(|| anyhow!("Loading sdk manifest at `{}`", manifest.display()))
            }
            Self::Full(manifest) if manifest.join(SDK_BUILD_MANIFEST_PATH).exists() => {
                // Otherwise assume this is a build manifest, but with no module.
                Sdk::from_build_dir(&manifest, None)
                    .with_context(|| anyhow!("Loading sdk manifest at `{}`", manifest.display()))
            }
            Self::Full(manifest) => Err(ffx_error!(
                "Failed to load the SDK.\n\
                    Expected '{manifest}' to contain a manifest at either:\n\
                    - '{SDK_MANIFEST_PATH}'\n\
                    - '{SDK_BUILD_MANIFEST_PATH}'.\n\
                    Check your SDK configuration (`ffx config get sdk.root`) and verify that \
                    an SDK has been downloaded or built in that location.",
                manifest = manifest.display()
            )
            .into()),
        }
    }

    pub fn to_config(&self) -> FfxSdkConfig {
        match self.clone() {
            Self::Modular { manifest, module } => {
                FfxSdkConfig { root: Some(manifest), module: Some(module) }
            }
            Self::Full(manifest) => FfxSdkConfig { root: Some(manifest), module: None },
        }
    }
}

impl Sdk {
    fn from_build_dir(path: &Path, module_manifest: Option<&str>) -> Result<Self> {
        let path = std::fs::canonicalize(path).with_context(|| {
            ffx_error!("SDK path `{}` was invalid and couldn't be canonicalized", path.display())
        })?;
        let manifest_path = match module_manifest {
            None => path.join(SDK_BUILD_MANIFEST_PATH),
            Some(module) => if cfg!(target_arch = "x86_64") {
                path.join("host_x64")
            } else if cfg!(target_arch = "aarch64") {
                path.join("host_arm64")
            } else {
                ffx_bail!("Host architecture {} not supported by the SDK", std::env::consts::ARCH)
            }
            .join("sdk/manifest")
            .join(module),
        };

        let file = Self::open_manifest(&manifest_path)?;
        let atoms = Self::parse_manifest(&manifest_path, file)?;

        // If we are able to parse the json file into atoms, creates a Sdk object from the atoms.
        Self::from_sdk_atoms(&path, atoms, SdkVersion::InTree)
            .with_context(|| anyhow!("Parsing atoms from SDK manifest at `{}`", path.display()))
    }

    pub fn from_sdk_dir(path_prefix: &Path) -> Result<Self> {
        tracing::debug!("from_sdk_dir {:?}", path_prefix);
        let path_prefix = std::fs::canonicalize(path_prefix).with_context(|| {
            ffx_error!(
                "SDK path `{}` was invalid and couldn't be canonicalized",
                path_prefix.display()
            )
        })?;
        let manifest_path = path_prefix.join(SDK_MANIFEST_PATH);

        let manifest_file = Self::open_manifest(&manifest_path)?;
        let manifest: Manifest = Self::parse_manifest(&manifest_path, manifest_file)?;

        Ok(Sdk {
            path_prefix,
            parts: manifest.parts,
            real_paths: None,
            version: SdkVersion::Version(manifest.id.clone()),
        })
    }

    fn open_manifest(path: &Path) -> Result<fs::File> {
        fs::File::open(path)
            .with_context(|| ffx_error!("Failed to open SDK manifest path at `{}`", path.display()))
    }

    fn parse_manifest<T: DeserializeOwned>(
        manifest_path: &Path,
        manifest_file: fs::File,
    ) -> Result<T> {
        serde_json::from_reader(BufReader::new(manifest_file)).with_context(|| {
            ffx_error!("Failed to parse SDK manifest file at `{}`", manifest_path.display())
        })
    }

    fn metadata_for<'a, M: DeserializeOwned>(
        &'a self,
        kinds: &'a [ElementType],
    ) -> impl Iterator<Item = M> + 'a {
        self.parts
            .iter()
            .filter_map(|part| {
                if kinds.contains(&part.kind) {
                    Some(self.path_prefix.join(&part.meta))
                } else {
                    None
                }
            })
            .filter_map(|path| match fs::File::open(path.clone()) {
                Ok(file) => Some((path, file)),
                Err(err) => {
                    warn!("Failed to open sdk metadata path: {} (error: {err})", path.display());
                    None
                }
            })
            .filter_map(|(path, file)| match serde_json::from_reader(file) {
                Ok(meta) => Some(meta),
                Err(err) => {
                    warn!("Failed to parse sdk metadata file: {} (error: {err})", path.display());
                    None
                }
            })
    }

    fn get_all_ffx_tools(&self) -> impl Iterator<Item = FfxTool> + '_ {
        self.metadata_for(&[ElementType::FfxTool])
    }

    pub fn get_ffx_tools(&self) -> impl Iterator<Item = FfxToolFiles> + '_ {
        self.get_all_ffx_tools().flat_map(|tool| {
            FfxToolFiles::from_metadata(self, tool, CpuArchitecture::current()).ok().flatten()
        })
    }

    pub fn get_ffx_tool(&self, name: &str) -> Option<FfxToolFiles> {
        self.get_all_ffx_tools()
            .filter(|tool| tool.name == name)
            .filter_map(|tool| {
                FfxToolFiles::from_metadata(self, tool, CpuArchitecture::current()).ok().flatten()
            })
            .max_by_key(|tool| tool.specificity_score)
    }

    pub fn get_host_tool(&self, name: &str) -> Result<PathBuf> {
        self.get_host_tool_relative_path(name).map(|path| self.path_prefix.join(path))
    }

    fn get_host_tools(&self) -> impl Iterator<Item = HostTool> + '_ {
        self.metadata_for(&[ElementType::HostTool, ElementType::CompanionHostTool])
    }

    fn get_host_tool_relative_path(&self, name: &str) -> Result<PathBuf> {
        let found_tool = self
            .get_host_tools()
            .filter(|tool| tool.name == name)
            .map(|tool| match &tool.files.as_deref() {
                Some([tool_path]) => Ok(tool_path.to_owned()),
                Some([tool_path, ..]) => {
                    warn!("Tool '{}' provides multiple files in manifest", name);
                    Ok(tool_path.to_owned())
                }
                Some([]) | None => {
                    Err(anyhow!("No executable provided for tool '{}' (file list was empty)", name))
                }
            })
            .collect::<Result<Vec<_>>>()?
            .into_iter()
            .min_by_key(|x| x.len()) // Shortest path is the one with no arch specifier, i.e. the default arch, i.e. the current arch (we hope.)
            .ok_or_else(|| ffx_error!("Tool '{}' not found in SDK dir", name))?;
        self.get_real_path(found_tool)
    }

    fn get_real_path(&self, path: impl AsRef<str>) -> Result<PathBuf> {
        match &self.real_paths {
            Some(map) => map.get(path.as_ref()).map(PathBuf::from).ok_or_else(|| {
                anyhow!("SDK File '{}' has no source in the build directory", path.as_ref())
            }),
            _ => Ok(PathBuf::from(path.as_ref())),
        }
    }

    pub fn get_path_prefix(&self) -> &Path {
        &self.path_prefix
    }

    pub fn get_version(&self) -> &SdkVersion {
        &self.version
    }

    /// For tests only
    #[doc(hidden)]
    pub fn get_empty_sdk_with_version(version: SdkVersion) -> Sdk {
        Sdk { path_prefix: PathBuf::new(), parts: Vec::new(), real_paths: None, version }
    }

    /// Allocates a new Sdk using the given atoms.
    ///
    /// All the meta files specified in the atoms are loaded.
    /// The creation succeed only if all the meta files have been loaded successfully.
    fn from_sdk_atoms(path_prefix: &Path, atoms: SdkAtoms, version: SdkVersion) -> Result<Self> {
        let mut metas = Vec::new();
        let mut real_paths = HashMap::new();

        for atom in atoms.atoms.iter() {
            for file in atom.files.iter() {
                real_paths.insert(file.destination.clone(), file.source.clone());
            }

            if atom.meta.len() > 0 {
                let meta = real_paths.get(&atom.meta).ok_or_else(|| {
                    anyhow!("Atom did not specify source for its metadata: {atom:?}")
                })?;

                metas.push(Part { meta: meta.clone(), kind: atom.kind.clone() });
            } else {
                tracing::debug!("Atom did not contain a meta file, skipping it: {atom:?}");
            }
        }

        Ok(Sdk {
            path_prefix: path_prefix.to_owned(),
            parts: metas,
            real_paths: Some(real_paths),
            version,
        })
    }
}

/// Even though an sdk_version for in-tree is an oxymoron, a value can be
/// generated.
///
/// Returns the current "F" milestone (e.g. F38) and a fixed date.major.minor
/// value of ".99991231.0.1". (e.g. "38.99991231.0.1" altogether).
///
/// The value was chosen because:
/// - it will never conflict with a real sdk build
/// - it will be newest for an sdk build of the same F
/// - it's just weird enough to recognizable and searchable
/// - the major.minor values align with fuchsia.dev guidelines
pub fn in_tree_sdk_version() -> String {
    format!("{}.99991231.0.1", MILESTONE.trim())
}

impl FfxToolFiles {
    fn from_metadata(sdk: &Sdk, tool: FfxTool, arch: CpuArchitecture) -> Result<Option<Self>> {
        let Some(executable) = tool.executable(arch) else {
            return Ok(None);
        };
        let Some(metadata) = tool.executable_metadata(arch) else {
            return Ok(None);
        };

        // Increment the score by zero or one for each of the executable and
        // metadata files, depending on if they're architecture specific or not,
        // for a total score of 0-2 (least specific to most specific).
        let specificity_score = executable.arch.map_or(0, |_| 1) + metadata.arch.map_or(0, |_| 1);
        let executable = sdk.path_prefix.join(&sdk.get_real_path(executable.file)?);
        let metadata = sdk.path_prefix.join(&sdk.get_real_path(metadata.file)?);
        Ok(Some(Self { executable, metadata, specificity_score }))
    }
}

////////////////////////////////////////////////////////////////////////////////
// tests

#[cfg(test)]
mod test {
    use super::*;
    use regex::Regex;
    use sdk_metadata::ElementType;
    use std::io::Write;
    use tempfile::TempDir;

    /// Writes the file to $root, with the path $path, from the source tree prefix $prefix
    /// (relative to this source file)
    macro_rules! put_file {
        ($root:expr, $prefix:literal, $name:literal) => {{
            fs::create_dir_all($root.path().join($name).parent().unwrap()).unwrap();
            fs::File::create($root.path().join($name))
                .unwrap()
                .write_all(include_bytes!(concat!($prefix, "/", $name)))
                .unwrap();
        }};
    }

    fn core_test_data_root() -> TempDir {
        let r = tempfile::tempdir().unwrap();
        put_file!(
            r,
            "../test_data/core-sdk-root",
            "host_arm64/gen/tools/symbol-index/symbol_index_sdk.meta.json"
        );
        put_file!(r, "../test_data/core-sdk-root", "sdk/manifest/core");
        put_file!(
            r,
            "../test_data/core-sdk-root",
            "host_x64/gen/src/developer/ffx/plugins/assembly/sdk.meta.json"
        );
        put_file!(
            r,
            "../test_data/core-sdk-root",
            "host_x64/gen/src/developer/debug/zxdb/zxdb_sdk.meta.json"
        );
        put_file!(
            r,
            "../test_data/core-sdk-root",
            "host_x64/gen/tools/symbol-index/symbol_index_sdk_legacy.meta.json"
        );
        put_file!(
            r,
            "../test_data/core-sdk-root",
            "host_x64/gen/tools/symbol-index/symbol_index_sdk.meta.json"
        );
        r
    }
    fn sdk_test_data_root() -> TempDir {
        let r = tempfile::tempdir().unwrap();
        put_file!(r, "../test_data/release-sdk-root", "fidl/fuchsia.data/meta.json");
        put_file!(r, "../test_data/release-sdk-root", "tools/ffx_tools/ffx-assembly-meta.json");
        put_file!(r, "../test_data/release-sdk-root", "meta/manifest.json");
        put_file!(r, "../test_data/release-sdk-root", "tools/zxdb-meta.json");
        r
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_core_manifest() {
        let root = core_test_data_root();
        let manifest_path = root.path();
        let atoms: SdkAtoms = serde_json::from_reader(BufReader::new(
            fs::File::open(manifest_path.join(SDK_BUILD_MANIFEST_PATH)).unwrap(),
        ))
        .unwrap();

        assert!(atoms.ids.is_empty());

        let atoms = atoms.atoms;
        assert_eq!(4, atoms.len());
        assert_eq!("partner", atoms[0].category);
        assert!(atoms[0].deps.is_empty());
        assert_eq!(
            "//src/developer/debug/zxdb:zxdb_sdk(//build/toolchain:host_x64)",
            atoms[0].gn_label
        );
        assert_eq!("sdk://tools/x64/zxdb", atoms[0].id);
        assert_eq!(ElementType::HostTool, atoms[0].kind);
        assert_eq!(2, atoms[0].files.len());
        assert_eq!("host_x64/zxdb", atoms[0].files[0].source);
        assert_eq!("tools/x64/zxdb", atoms[0].files[0].destination);

        assert_eq!("partner", atoms[3].category);
        assert!(atoms[3].deps.is_empty());
        assert_eq!(
            "//src/developer/ffx/plugins/assembly:sdk(//build/toolchain:host_x64)",
            atoms[3].gn_label
        );
        assert_eq!("sdk://tools/ffx_tools/ffx-assembly", atoms[3].id);
        assert_eq!(ElementType::FfxTool, atoms[3].kind);
        assert_eq!(4, atoms[3].files.len());
        assert_eq!("host_x64/ffx-assembly", atoms[3].files[0].source);
        assert_eq!("tools/x64/ffx_tools/ffx-assembly", atoms[3].files[0].destination);
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_core_manifest_to_sdk() {
        let root = core_test_data_root();
        let manifest_path = root.path();
        let atoms = serde_json::from_reader(BufReader::new(
            fs::File::open(manifest_path.join(SDK_BUILD_MANIFEST_PATH)).unwrap(),
        ))
        .unwrap();

        let sdk = Sdk::from_sdk_atoms(manifest_path, atoms, SdkVersion::Unknown).unwrap();

        let mut parts = sdk.parts.iter();
        assert!(matches!(parts.next().unwrap(), Part { kind: ElementType::HostTool, .. }));
        assert!(matches!(parts.next().unwrap(), Part { kind: ElementType::HostTool, .. }));
        assert!(matches!(parts.next().unwrap(), Part { kind: ElementType::HostTool, .. }));
        assert!(matches!(parts.next().unwrap(), Part { kind: ElementType::FfxTool, .. }));
        assert!(parts.next().is_none());
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_core_manifest_host_tool() {
        let root = core_test_data_root();
        let manifest_path = root.path();
        let atoms = serde_json::from_reader(BufReader::new(
            fs::File::open(manifest_path.join(SDK_BUILD_MANIFEST_PATH)).unwrap(),
        ))
        .unwrap();

        let sdk = Sdk::from_sdk_atoms(manifest_path, atoms, SdkVersion::Unknown).unwrap();
        let zxdb = sdk.get_host_tool("zxdb").unwrap();

        assert_eq!(manifest_path.join("host_x64/zxdb"), zxdb);
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_core_manifest_host_tool_multi_arch() {
        let root = core_test_data_root();
        let manifest_path = root.path();
        let atoms = serde_json::from_reader(BufReader::new(
            fs::File::open(manifest_path.join(SDK_BUILD_MANIFEST_PATH)).unwrap(),
        ))
        .unwrap();

        let sdk = Sdk::from_sdk_atoms(manifest_path, atoms, SdkVersion::InTree).unwrap();
        let symbol_index = sdk.get_host_tool("symbol-index").unwrap();

        assert_eq!(manifest_path.join("host_x64/symbol-index"), symbol_index);
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_core_manifest_ffx_tool() {
        let root = core_test_data_root();
        let manifest_path = root.path();
        let atoms = serde_json::from_reader(BufReader::new(
            fs::File::open(manifest_path.join(SDK_BUILD_MANIFEST_PATH)).unwrap(),
        ))
        .unwrap();

        let sdk = Sdk::from_sdk_atoms(manifest_path, atoms, SdkVersion::Unknown).unwrap();
        let ffx_assembly = sdk.get_ffx_tool("ffx-assembly").unwrap();

        assert_eq!(manifest_path.join("host_x64/ffx-assembly"), ffx_assembly.executable);
        assert_eq!(manifest_path.join("host_x64/ffx-assembly.json"), ffx_assembly.metadata);
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_sdk_manifest() {
        let root = sdk_test_data_root();
        let sdk_root = root.path();
        let manifest: Manifest = serde_json::from_reader(BufReader::new(
            fs::File::open(sdk_root.join(SDK_MANIFEST_PATH)).unwrap(),
        ))
        .unwrap();

        assert_eq!("0.20201005.4.1", manifest.id);

        let mut parts = manifest.parts.iter();
        assert!(matches!(parts.next().unwrap(), Part { kind: ElementType::FidlLibrary, .. }));
        assert!(matches!(parts.next().unwrap(), Part { kind: ElementType::HostTool, .. }));
        assert!(matches!(parts.next().unwrap(), Part { kind: ElementType::FfxTool, .. }));
        assert!(parts.next().is_none());
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_sdk_manifest_host_tool() {
        let root = sdk_test_data_root();
        let sdk_root = root.path();
        let manifest: Manifest = serde_json::from_reader(BufReader::new(
            fs::File::open(sdk_root.join(SDK_MANIFEST_PATH)).unwrap(),
        ))
        .unwrap();

        let sdk = Sdk {
            path_prefix: sdk_root.to_owned(),
            parts: manifest.parts,
            real_paths: None,
            version: SdkVersion::Version(manifest.id.to_owned()),
        };
        let zxdb = sdk.get_host_tool("zxdb").unwrap();

        assert_eq!(sdk_root.join("tools/zxdb"), zxdb);
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_sdk_manifest_ffx_tool() {
        let root = sdk_test_data_root();
        let sdk_root = root.path();
        let manifest: Manifest = serde_json::from_reader(BufReader::new(
            fs::File::open(sdk_root.join(SDK_MANIFEST_PATH)).unwrap(),
        ))
        .unwrap();

        let sdk = Sdk {
            path_prefix: sdk_root.to_owned(),
            parts: manifest.parts,
            real_paths: None,
            version: SdkVersion::Version(manifest.id.to_owned()),
        };
        let ffx_assembly = sdk.get_ffx_tool("ffx-assembly").unwrap();

        assert_eq!(sdk_root.join("tools/x64/ffx_tools/ffx-assembly"), ffx_assembly.executable);
        assert_eq!(sdk_root.join("tools/ffx_tools/ffx-assembly.json"), ffx_assembly.metadata);
    }

    #[test]
    fn test_in_tree_sdk_version() {
        let version = in_tree_sdk_version();
        let re = Regex::new(r"^\d+.99991231.0.1$").expect("creating regex");
        assert!(re.is_match(&version));
    }
}
