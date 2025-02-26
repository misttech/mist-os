// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::{anyhow, Context, Result};
use errors::{ffx_bail, ffx_error};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::io::BufReader;
use std::path::{Path, PathBuf};
use std::process::Command;
use tracing::warn;

use metadata::{CpuArchitecture, ElementType, FfxTool, HostTool, Manifest, Part};
pub use sdk_metadata as metadata;

const SDK_MANIFEST_PATH: &str = "meta/manifest.json";
// TODO(https://fxbug.dev/397989792): Remove this hard coded path.
const SDK_BUILD_MANIFEST_PATH: &str = "sdk/manifest/core";

/// Current "F" milestone for Fuchsia (e.g. F38).
const MILESTONE: &'static str = include_str!("../../../../../../integration/MILESTONE");

const SDK_NOT_FOUND_HELP: &str = "\
SDK directory could not be found. Please set with
`ffx sdk set root <PATH_TO_SDK_DIR>`\n
If you are developing in the fuchsia tree, ensure \
that you are running the `ffx` command (in $FUCHSIA_DIR/.jiri_root) or `fx ffx`, not a built binary.
Running the binary directly is not supported in the fuchsia tree.\n\n";

#[derive(Debug, PartialEq, Eq)]
pub enum SdkVersion {
    Version(String),
    InTree,
    Unknown,
}

#[derive(Debug)]
pub struct Sdk {
    path_prefix: PathBuf,
    module: Option<String>,
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

/// The SDKRoot is the path that is the root directory for the relative paths contained in
/// the SDK manifest. The SDK manifest defines the contents of the SDK.
/// There are two common use cases for the SdkRoot.
///
/// The first is the "out-of-tree" use case, this
/// is where the IDK and optionally additional files, are downloaded as part of a source code project.
/// The IDK includes a manifest file that defines the contents of the IDK.  The manifest file, and
/// the root directory define a specific SdkRoot.
///
/// The other use case is in the Fuchsia.git source code project (aka in-tree). In this case the IDK
/// atom collection is used to locate the host and companion tools. This is done to avoid building
///  the complete IDK and results in dramatically reduced build times for common developer workflows.
///  When using SdkRoot in-tree, the root should be the $root_build_dir.
///
/// TODO(https://fxbug.dev/397989792) tracks removing this hard coded default path for in-tree IDK usage.
///
#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum SdkRoot {
    /// Modular SDKRoot is not used widely - it was an attempt to make a partial SDK, specifically
    /// just the host tools needed to run build actions. In this case, the module is a SDK manifest
    /// that is located at ${sdk.root}/host-${cpu}/sdk/manifest/${module}.
    Modular { root: PathBuf, module: String },

    /// Full SDK root is actually referring to the root directory of an IDK.  This means it
    /// has the contents of is normally found in meta/manifest.json in the IDK. The paths in the
    ///  manifest are relative to the root directory.

    /// The manifest is optional where None indicates use one of the well known manifests. These are
    ///
    ///  `meta/manifest.json`, which represents the out of tree IDK structure.
    ///
    /// `sdk/manifest/core`, which represents the in-tree IDK that has not been made
    /// into a final IDK manifest. This is actually an IDK collection,
    ///  which contains the atoms for a single build. These atoms contain the mapping from a file
    /// that is an output of the build to the destination in the IDK root directory. The
    /// advantage being the contents of IDK can be read by ffx, and access the files wherever they are
    /// located in the build directory vs. having to copy these files into the final IDK structure.
    /// As a result, when using a "Full SDK" in-tree, the root should always be the $root_build_dir.
    ///
    ///  If the manifest is specified, it must be a relative path to the manifest,
    /// based on the root directory.
    Full { root: PathBuf, manifest: Option<String> },
}

/// A serde-serializable representation of ffx' sdk configuration.
/// Used by Isolate tests.
#[derive(Default, Clone, Debug, Serialize, Deserialize)]
pub struct FfxSdkConfig {
    pub root: Option<PathBuf>,
    pub manifest: Option<String>,
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
    #[serde(default)]
    stable: bool,
}

#[derive(Deserialize, Debug)]
struct File {
    destination: String,
    source: String,
}

impl SdkRoot {
    /// Gets the basic information about the sdk as configured, without diving deeper into the sdk's own configuration.
    pub fn from_paths(start_path: Option<&Path>, module: Option<String>) -> Result<Self> {
        // All gets in this function should declare that they don't want the build directory searched, because
        // if there is a build directory it *is* generally the sdk.
        let sdk_root = match start_path {
            Some(root) => root.to_owned(),
            _ => {
                let exe_path = find_exe_path()?;

                match Self::find_sdk_root(&Path::new(&exe_path)) {
                    Ok(Some(root)) => root,
                    Ok(None) => {
                        errors::ffx_bail!(
                            "{}Could not find an SDK manifest in any parent of ffx's directory.",
                            SDK_NOT_FOUND_HELP,
                        );
                    }
                    Err(e) => {
                        errors::ffx_bail!("{}Error was: {:?}", SDK_NOT_FOUND_HELP, e);
                    }
                }
            }
        };

        match module {
            Some(module) => {
                tracing::debug!("Found modular Fuchsia SDK at {sdk_root:?} with module {module}");
                Ok(SdkRoot::Modular { root: sdk_root, module })
            }
            _ => {
                tracing::debug!("Found full Fuchsia SDK at {sdk_root:?}");
                Ok(SdkRoot::Full { root: sdk_root, manifest: None })
            }
        }
    }

    fn find_sdk_root(start_path: &Path) -> Result<Option<PathBuf>> {
        let cwd = std::env::current_dir()
            .context("Could not resolve working directory while searching for the Fuchsia SDK")?;
        let mut path = cwd.join(start_path);
        tracing::debug!("Attempting to find the sdk root from {path:?}");

        loop {
            path = if let Some(parent) = path.parent() {
                parent.to_path_buf()
            } else {
                return Ok(None);
            };

            if SdkRoot::is_sdk_root(&path) {
                tracing::debug!("Found sdk root through recursive search in {path:?}");
                return Ok(Some(path));
            }
        }
    }

    /// Returns true if the given path appears to be an sdk root.
    fn is_sdk_root(path: &Path) -> bool {
        // TODO(https://fxbug.dev/397989792) remove the hard coded paths to the manifest for in-tree SDKs.
        path.join(SDK_MANIFEST_PATH).exists() || path.join(SDK_BUILD_MANIFEST_PATH).exists()
    }

    /// Returns manifest path if it exists.
    pub fn manifest_path(&self) -> Option<PathBuf> {
        match self {
            Self::Full { root, manifest: Some(manifest) } if root.join(manifest).exists() => {
                Some(root.join(manifest))
            }
            Self::Full { root, manifest: None } if root.join(SDK_MANIFEST_PATH).exists() => {
                Some(root.join(SDK_MANIFEST_PATH))
            }
            Self::Full { root, manifest: None } if root.join(SDK_BUILD_MANIFEST_PATH).exists() => {
                Some(root.join(SDK_BUILD_MANIFEST_PATH))
            }
            Self::Full { .. } => None,
            Self::Modular { root, module } => {
                if let Ok(module_path) = module_manifest_path(root, module) {
                    if module_path.exists() {
                        Some(module_path)
                    } else {
                        None
                    }
                } else {
                    None
                }
            }
        }
    }

    /// Does a full load of the sdk configuration.
    pub fn get_sdk(self) -> Result<Sdk> {
        tracing::debug!("get_sdk from {self:?}");
        match self {
            Self::Modular { root, module } => {
                // Modular only ever makes sense as part of a build directory
                // sdk, so there's no need to figure out what kind it is.
                Sdk::from_build_dir(&root, Some(&module)).with_context(|| {
                    anyhow!("Loading modular sdk at `{}` with module `{module}`", root.display())
                })
            }
            Self::Full { root, manifest: Some(manifest_file) } => {
                // If manifest file is specified, use it as an IDK manifest.
                Sdk::from_sdk_dir(&root, &manifest_file).with_context(|| {
                    anyhow!("Loading sdk manifest at `{}/{manifest_file}`", root.display())
                })
            }
            Self::Full { root, manifest: None } if root.join(SDK_MANIFEST_PATH).exists() => {
                // If the manifest is not specified, but the SDK_MANIFEST exists, read it as the
                // IDK manifest.
                Sdk::from_sdk_dir(&root, SDK_MANIFEST_PATH)
                    .with_context(|| anyhow!("Loading sdk manifest at `{}`", root.display()))
            }
            // TODO(https://fxbug.dev/397989792): Remove the in-tree hard coded path to the manifest.
            Self::Full { root, manifest: None } if root.join(SDK_BUILD_MANIFEST_PATH).exists() => {
                // If the manifest is not specified, but the atom collection manifest exists,
                // use that as an atom collection.
                Sdk::from_build_dir(&root, None)
                    .with_context(|| anyhow!("Loading sdk atom collection at `{}`", root.display()))
            }
            Self::Full { root, manifest: _ } => {
                // Otherwise assume this is a root that has no expected manifest or atom collection.
                // So make a partial SDK which will work with tools that do not access the SDK
                // information.
                Sdk::from_build_dir(&root, None).with_context(|| {
                    anyhow!("Loading unknown sdk manifest at `{}`", root.display())
                })
            }
        }
    }

    pub fn to_config(&self) -> FfxSdkConfig {
        match self.clone() {
            Self::Modular { root, module } => {
                FfxSdkConfig { root: Some(root), manifest: None, module: Some(module) }
            }
            Self::Full { root, manifest } => {
                FfxSdkConfig { root: Some(root), manifest, module: None }
            }
        }
    }
}

/// Finds the executable path of the ffx binary being run, attempting to
/// get the path the user believes it to be at, even if it's symlinked from
/// somewhere else, by using `argv[0]` and [`std::env::current_exe`].
///
/// We do this because sometimes ffx is invoked through an SDK that is symlinked
/// into place from a content addressable store, and we want to make a best
/// effort to search for the sdk in the right place.
fn find_exe_path() -> Result<PathBuf> {
    // get the 'real' binary path, which may have symlinks resolved, as well
    // as the command this was run as and the cwd
    let cwd = std::env::current_dir().context("FFX was run from an invalid working directory")?;
    let binary_path = std::env::current_exe()
        .and_then(|p| p.canonicalize())
        .context("FFX Binary doesn't exist in the file system")?;
    let args_path = match std::env::args_os().next() {
        Some(arg) => PathBuf::from(&arg),
        None => {
            tracing::trace!("FFX was run without an argv[0] somehow");
            return Ok(binary_path);
        }
    };

    // canonicalize the path from argv0 to try to figure out where it 'really'
    // is to make sure it's actually the right binary through potential
    // symlinks.
    let canonical_args_path = match args_path.canonicalize() {
        Ok(path) => path,
        Err(e) => {
            tracing::trace!(
                "Could not canonicalize the path ffx was run with, \
                which might mean the working directory has changed or the file \
                doesn't exist anymore: {e:?}"
            );
            return Ok(binary_path);
        }
    };

    // check that it's the same file in the end
    if binary_path == canonical_args_path {
        // but return the path it was actually run through instead of the canonical
        // path, but [`Path::join`]-ed to the cwd to make it more or less
        // absolute.
        Ok(cwd.join(args_path))
    } else {
        tracing::trace!(
            "FFX's argv[0] ({args_path:?}) resolved to {canonical_args_path:?} \
            instead of the binary's path {binary_path:?}, falling back to the \
            binary path."
        );
        Ok(binary_path)
    }
}

fn module_manifest_path(path: &Path, module: &str) -> Result<PathBuf> {
    let arch_path = if cfg!(target_arch = "x86_64") {
        "host_x64"
    } else if cfg!(target_arch = "aarch64") {
        "host_arm64"
    } else {
        ffx_bail!("Host architecture {} not supported by the SDK", std::env::consts::ARCH)
    };
    Ok(path.join(arch_path).join("sdk/manifest").join(module))
}

impl Sdk {
    fn from_build_dir(path: &Path, module_manifest: Option<&str>) -> Result<Self> {
        let path = std::fs::canonicalize(path).with_context(|| {
            ffx_error!("SDK path `{}` was invalid and couldn't be canonicalized", path.display())
        })?;
        let manifest_path = match module_manifest {
            None => path.join(SDK_BUILD_MANIFEST_PATH),
            Some(module) => module_manifest_path(&path, module)?,
        };

        let file = Self::open_manifest(&manifest_path)?;
        let atoms = Self::parse_manifest(&manifest_path, file)?;

        // If we are able to parse the json file into atoms, creates a Sdk object from the atoms.
        Self::from_sdk_atoms(&path, module_manifest, atoms, SdkVersion::InTree)
            .with_context(|| anyhow!("Parsing atoms from SDK manifest at `{}`", path.display()))
    }

    pub fn from_sdk_dir(path_prefix: &Path, manifest_file: &str) -> Result<Self> {
        let path_prefix = std::fs::canonicalize(path_prefix).with_context(|| {
            ffx_error!(
                "SDK path `{}` was invalid and couldn't be canonicalized",
                path_prefix.display()
            )
        })?;
        let manifest_path = path_prefix.join(manifest_file);

        let manifest_file = Self::open_manifest(&manifest_path)?;
        let manifest: Manifest = Self::parse_manifest(&manifest_path, manifest_file)?;

        Ok(Sdk {
            path_prefix,
            module: None,
            parts: manifest.parts,
            real_paths: None,
            version: SdkVersion::Version(manifest.id),
        })
    }

    pub fn new() -> Self {
        Sdk {
            path_prefix: PathBuf::new(),
            module: None,
            parts: vec![],
            real_paths: None,
            version: SdkVersion::Unknown,
        }
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

    /// Returns the path to the tool with the given name based on the SDK contents.
    /// A preferred alternative to this method is ffx_config::get_host_tool() which
    /// also considers configured overrides for the tools.
    pub fn get_host_tool(&self, name: &str) -> Result<PathBuf> {
        self.get_host_tool_relative_path(name).map(|path| self.path_prefix.join(path))
    }

    /// Get the metadata for all host tools
    pub fn get_all_host_tools_metadata(&self) -> impl Iterator<Item = HostTool> + '_ {
        self.metadata_for(&[ElementType::HostTool, ElementType::CompanionHostTool])
    }

    fn get_host_tool_relative_path(&self, name: &str) -> Result<PathBuf> {
        let found_tool = self
            .get_all_host_tools_metadata()
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

    /// Returns a command invocation builder for the given host tool, if it
    /// exists in the sdk.
    pub fn get_host_tool_command(&self, name: &str) -> Result<Command> {
        let host_tool = self.get_host_tool(name)?;
        let mut command = Command::new(host_tool);
        command.env("FUCHSIA_SDK_PATH", &self.path_prefix);
        if let Some(module) = self.module.as_deref() {
            command.env("FUCHSIA_SDK_ENV", module);
        }
        Ok(command)
    }

    pub fn get_path_prefix(&self) -> &Path {
        &self.path_prefix
    }

    pub fn get_version(&self) -> &SdkVersion {
        &self.version
    }

    pub fn get_version_string(&self) -> Option<String> {
        match &self.version {
            SdkVersion::Version(version) => Some(version.to_string()),
            SdkVersion::InTree => Some(in_tree_sdk_version()),
            SdkVersion::Unknown => None,
        }
    }

    /// For tests only
    #[doc(hidden)]
    pub fn get_empty_sdk_with_version(version: SdkVersion) -> Sdk {
        Sdk {
            path_prefix: PathBuf::new(),
            module: None,
            parts: Vec::new(),
            real_paths: None,
            version,
        }
    }

    /// Allocates a new Sdk using the given atoms.
    ///
    /// All the meta files specified in the atoms are loaded.
    /// The creation succeed only if all the meta files have been loaded successfully.
    fn from_sdk_atoms(
        path_prefix: &Path,
        module: Option<&str>,
        atoms: SdkAtoms,
        version: SdkVersion,
    ) -> Result<Self> {
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

                metas.push(Part {
                    meta: meta.clone(),
                    kind: atom.kind.clone(),
                    stable: atom.stable,
                });
            } else {
                tracing::debug!("Atom did not contain a meta file, skipping it: {atom:?}");
            }
        }

        Ok(Sdk {
            path_prefix: path_prefix.to_owned(),
            module: module.map(str::to_owned),
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
    use std::io::Write;
    use tempfile::{tempdir, TempDir};

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
            "host_x64/sdk/manifest/host_tools_used_by_ffx_action_during_build"
        );
        put_file!(
            r,
            "../test_data/core-sdk-root",
            "host_arm64/sdk/manifest/host_tools_used_by_ffx_action_during_build"
        );
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

    #[test]
    fn test_manifest_exists() {
        let core_root = core_test_data_root();
        let release_root = sdk_test_data_root();
        assert!(SdkRoot::Full { root: core_root.path().to_owned(), manifest: None }
            .manifest_path()
            .is_some());
        assert!(SdkRoot::Modular {
            root: core_root.path().to_owned(),
            module: "host_tools_used_by_ffx_action_during_build".to_owned()
        }
        .manifest_path()
        .is_some());
        assert!(SdkRoot::Full {
            root: release_root.path().to_owned(),
            manifest: Some(SDK_MANIFEST_PATH.into())
        }
        .manifest_path()
        .is_some());
    }

    #[fuchsia::test]
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

    #[fuchsia::test]
    async fn test_core_manifest_to_sdk() {
        let root = core_test_data_root();
        let manifest_path = root.path();
        let atoms = serde_json::from_reader(BufReader::new(
            fs::File::open(manifest_path.join(SDK_BUILD_MANIFEST_PATH)).unwrap(),
        ))
        .unwrap();

        let sdk = Sdk::from_sdk_atoms(manifest_path, None, atoms, SdkVersion::Unknown).unwrap();

        let mut parts = sdk.parts.iter();
        assert!(matches!(parts.next().unwrap(), Part { kind: ElementType::HostTool, .. }));
        assert!(matches!(parts.next().unwrap(), Part { kind: ElementType::HostTool, .. }));
        assert!(matches!(parts.next().unwrap(), Part { kind: ElementType::HostTool, .. }));
        assert!(matches!(parts.next().unwrap(), Part { kind: ElementType::FfxTool, .. }));
        assert!(parts.next().is_none());
    }

    #[fuchsia::test]
    async fn test_core_manifest_host_tool() {
        let root = core_test_data_root();
        let manifest_path = root.path();
        let atoms = serde_json::from_reader(BufReader::new(
            fs::File::open(manifest_path.join(SDK_BUILD_MANIFEST_PATH)).unwrap(),
        ))
        .unwrap();

        let sdk = Sdk::from_sdk_atoms(manifest_path, None, atoms, SdkVersion::Unknown).unwrap();
        let zxdb = sdk.get_host_tool("zxdb").unwrap();

        assert_eq!(manifest_path.join("host_x64/zxdb"), zxdb);

        let zxdb_cmd = sdk.get_host_tool_command("zxdb").unwrap();
        assert_eq!(zxdb_cmd.get_program(), manifest_path.join("host_x64/zxdb"));
    }

    #[fuchsia::test]
    async fn test_core_manifest_host_tool_multi_arch() {
        let root = core_test_data_root();
        let manifest_path = root.path();
        let atoms = serde_json::from_reader(BufReader::new(
            fs::File::open(manifest_path.join(SDK_BUILD_MANIFEST_PATH)).unwrap(),
        ))
        .unwrap();

        let sdk = Sdk::from_sdk_atoms(manifest_path, None, atoms, SdkVersion::InTree).unwrap();
        let symbol_index = sdk.get_host_tool("symbol-index").unwrap();

        assert_eq!(manifest_path.join("host_x64/symbol-index"), symbol_index);

        let symbol_index_cmd = sdk.get_host_tool_command("symbol-index").unwrap();
        assert_eq!(symbol_index_cmd.get_program(), manifest_path.join("host_x64/symbol-index"));
    }

    #[fuchsia::test]
    async fn test_core_manifest_ffx_tool() {
        let root = core_test_data_root();
        let manifest_path = root.path();
        let atoms = serde_json::from_reader(BufReader::new(
            fs::File::open(manifest_path.join(SDK_BUILD_MANIFEST_PATH)).unwrap(),
        ))
        .unwrap();

        let sdk = Sdk::from_sdk_atoms(manifest_path, None, atoms, SdkVersion::Unknown).unwrap();
        let ffx_assembly = sdk.get_ffx_tool("ffx-assembly").unwrap();

        // get_ffx_tool selects with the current architecture, so the executable path will be
        // architecture-dependent.
        let arch = CpuArchitecture::current();
        let host_dir = match arch {
            CpuArchitecture::X64 => "host_x64",
            CpuArchitecture::Arm64 => "host_arm64",
            CpuArchitecture::Riscv64 => "host_riscv64",
            _ => panic!("Unsupported architecture {}", arch),
        };
        assert_eq!(manifest_path.join(host_dir).join("ffx-assembly"), ffx_assembly.executable);
        // On the other hand, the metadata comes from a fixed set of input test data, which says
        // the source of tools/ffx_tools/ffx-assembly.json is host_x64/ffx-assembly.json
        assert_eq!(manifest_path.join("host_x64/ffx-assembly.json"), ffx_assembly.metadata);
    }

    #[fuchsia::test]
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

    #[fuchsia::test]
    async fn test_sdk_manifest_host_tool() {
        let root = sdk_test_data_root();
        let sdk_root = root.path();
        let manifest: Manifest = serde_json::from_reader(BufReader::new(
            fs::File::open(sdk_root.join(SDK_MANIFEST_PATH)).unwrap(),
        ))
        .unwrap();

        let sdk = Sdk {
            path_prefix: sdk_root.to_owned(),
            module: None,
            parts: manifest.parts,
            real_paths: None,
            version: SdkVersion::Version(manifest.id.to_owned()),
        };
        let zxdb = sdk.get_host_tool("zxdb").unwrap();

        assert_eq!(sdk_root.join("tools/zxdb"), zxdb);

        let zxdb_cmd = sdk.get_host_tool_command("zxdb").unwrap();
        assert_eq!(zxdb_cmd.get_program(), sdk_root.join("tools/zxdb"));
    }

    #[fuchsia::test]
    async fn test_sdk_manifest_ffx_tool() {
        let root = sdk_test_data_root();
        let sdk_root = root.path();
        let manifest: Manifest = serde_json::from_reader(BufReader::new(
            fs::File::open(sdk_root.join(SDK_MANIFEST_PATH)).unwrap(),
        ))
        .unwrap();

        let sdk = Sdk {
            path_prefix: sdk_root.to_owned(),
            module: None,
            parts: manifest.parts,
            real_paths: None,
            version: SdkVersion::Version(manifest.id.to_owned()),
        };
        let ffx_assembly = sdk.get_ffx_tool("ffx-assembly").unwrap();

        // get_ffx_tool selects with the current architecture, so the executable path will be
        // architecture-dependent.
        let current_arch = CpuArchitecture::current();
        let arch = match current_arch {
            CpuArchitecture::Arm64 => "arm64",
            CpuArchitecture::X64 => "x64",
            CpuArchitecture::Riscv64 => "riscv64",
            _ => panic!("Unsupported host tool architecture {}", current_arch),
        };
        assert_eq!(
            sdk_root.join("tools").join(arch).join("ffx_tools/ffx-assembly"),
            ffx_assembly.executable
        );
        assert_eq!(sdk_root.join("tools/ffx_tools/ffx-assembly.json"), ffx_assembly.metadata);
    }

    #[test]
    fn test_in_tree_sdk_version() {
        let version = in_tree_sdk_version();
        let re = Regex::new(r"^\d+.99991231.0.1$").expect("creating regex");
        assert!(re.is_match(&version));
    }

    #[fuchsia::test]
    fn test_find_sdk_root_finds_root() {
        let temp = tempdir().unwrap();
        let temp_path = std::fs::canonicalize(temp.path()).expect("canonical temp path");

        let start_path = temp_path.join("test1").join("test2");
        std::fs::create_dir_all(start_path.clone()).unwrap();

        let meta_path = temp_path.join("meta");
        std::fs::create_dir(meta_path.clone()).unwrap();

        std::fs::write(meta_path.join("manifest.json"), "").unwrap();

        assert_eq!(SdkRoot::find_sdk_root(&start_path).unwrap().unwrap(), temp_path);
    }

    #[fuchsia::test]
    fn test_find_sdk_root_no_manifest() {
        let temp = tempdir().unwrap();

        let start_path = temp.path().to_path_buf().join("test1").join("test2");
        std::fs::create_dir_all(start_path.clone()).unwrap();

        let meta_path = temp.path().to_path_buf().join("meta");
        std::fs::create_dir(meta_path).unwrap();

        assert!(SdkRoot::find_sdk_root(&start_path).unwrap().is_none());
    }
}
