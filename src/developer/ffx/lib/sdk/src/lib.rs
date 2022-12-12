// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::{anyhow, bail, Context, Result};
use sdk_metadata::{FfxTool, FidlLibrary, HostTool, Manifest};
use serde::Deserialize;
use serde_json::Value;
use std::{
    collections::HashMap,
    fs,
    io::{self, BufReader, Read},
    path::{Path, PathBuf},
};
use tracing::warn;

#[derive(Debug, PartialEq, Eq)]
pub enum SdkVersion {
    Version(String),
    InTree,
    Unknown,
}

#[derive(Debug)]
pub struct Sdk {
    path_prefix: PathBuf,
    metas: Vec<Meta>,
    real_paths: Option<HashMap<String, String>>,
    version: SdkVersion,
}

#[derive(Debug)]
pub enum Meta {
    HostTool(HostTool),
    CompanionHostTool(HostTool),
    FfxTool(FfxTool),
    FidlLibrary(FidlLibrary),
    Unknown(Value),
}

// we manually implement deserializing because the semantics of serde's
// tagging functionality don't allow for having a tag and including it
// in the structure, so we make a stop at json to check the type field
// ourselves.
impl Meta {
    fn from_reader(buf: impl Read) -> Result<Self, serde_json::Error> {
        let obj: serde_json::Map<_, _> = serde_json::from_reader(buf)?;
        let meta = match obj.get("type").and_then(Value::as_str) {
            Some("host_tool") => Meta::HostTool(serde_json::from_value(Value::Object(obj))?),
            Some("companion_host_tool") => {
                Meta::CompanionHostTool(serde_json::from_value(Value::Object(obj))?)
            }
            Some("ffx_tool") => Meta::FfxTool(serde_json::from_value(Value::Object(obj))?),
            Some("fidl_library") => Meta::FidlLibrary(serde_json::from_value(Value::Object(obj))?),
            Some(_) | None => Meta::Unknown(Value::Object(obj)),
        };
        Ok(meta)
    }
}

#[derive(Deserialize)]
struct SdkAtoms {
    #[cfg(test)]
    ids: Vec<Value>,
    atoms: Vec<Atom>,
}

#[derive(Deserialize)]
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
    #[cfg(test)]
    ty: String,
}

#[derive(Deserialize)]
struct File {
    destination: String,
    source: String,
}

pub enum SdkRoot {
    InTree { manifest: PathBuf, module: Option<String> },
    Packaged(PathBuf),
}

impl SdkRoot {
    /// Does a full load of the sdk configuration.
    pub fn get_sdk(&self) -> Result<Sdk> {
        tracing::debug!("get_sdk");
        match self {
            Self::InTree { manifest, module } => Sdk::from_build_dir(&manifest, module.as_ref()),
            Self::Packaged(manifest) => Sdk::from_sdk_dir(&manifest),
        }
    }
}

impl Sdk {
    fn from_build_dir(path: &Path, module_manifest: Option<impl AsRef<str>>) -> Result<Self> {
        let path = std::fs::canonicalize(path)
            .context(format!("canonicalizing sdk path prefix {}", path.display()))?;
        let manifest_path = match module_manifest {
            None => path.join("sdk/manifest/core"),
            Some(module) => if cfg!(target_arch = "x86_64") {
                path.join("host_x64")
            } else if cfg!(target_arch = "aarch64") {
                path.join("host_arm64")
            } else {
                bail!("Host architecture not supported")
            }
            .join("sdk/manifest")
            .join(module.as_ref()),
        };

        let file = fs::File::open(&manifest_path)
            .context(format!("opening manifest path: {:?}", manifest_path))?;

        // If we are able to parse the json file into atoms, creates a Sdk object from the atoms.
        Self::from_sdk_atoms(
            path.to_owned(),
            Self::atoms_from_core_manifest(&manifest_path, BufReader::new(file))?,
            Self::open_meta,
            SdkVersion::InTree,
        )
    }

    fn atoms_from_core_manifest<T>(manifest_path: &Path, reader: BufReader<T>) -> Result<SdkAtoms>
    where
        T: io::Read,
    {
        let atoms: serde_json::Result<SdkAtoms> = serde_json::from_reader(reader);

        match atoms {
            Ok(result) => Ok(result),
            Err(e) => Err(anyhow!("Can't read json file {:?}: {:?}", manifest_path, e)),
        }
    }

    pub fn from_sdk_dir(path_prefix: &Path) -> Result<Self> {
        tracing::debug!("from_sdk_dir {:?}", path_prefix);
        let path_prefix = std::fs::canonicalize(path_prefix)
            .context(format!("canonicalizing sdk path prefix {}", path_prefix.display()))?;
        let manifest_path = path_prefix.join("meta/manifest.json");
        let mut version = SdkVersion::Unknown;

        Self::metas_from_sdk_manifest(
            BufReader::new(
                fs::File::open(manifest_path.clone())
                    .context(format!("opening sdk manifest path: {:?}", manifest_path))?,
            ),
            &mut version,
            |meta| {
                let meta_path = path_prefix.join(meta);
                fs::File::open(meta_path.clone())
                    .context(format!("opening sdk path: {:?}", meta_path))
                    .ok()
                    .map(BufReader::new)
            },
        )
        .map(|metas| Sdk { path_prefix, metas, real_paths: None, version })
    }

    fn metas_from_sdk_manifest<M, T>(
        manifest: BufReader<T>,
        version: &mut SdkVersion,
        get_meta: M,
    ) -> Result<Vec<Meta>>
    where
        M: Fn(&str) -> Option<BufReader<T>>,
        T: io::Read,
    {
        tracing::debug!("metas_from_sdk_manifest {:?}", version);
        let manifest: Manifest =
            serde_json::from_reader(manifest).context("Reading manifest file")?;
        // TODO: Check the schema version and log a warning if it's not what we expect.

        *version = SdkVersion::Version(manifest.id.clone());

        let metas = manifest
            .parts
            .into_iter()
            .filter_map(|x| get_meta(&x.meta))
            .map(|reader| Meta::from_reader(reader).context("Parsing metadata file"))
            .collect::<Result<Vec<_>>>()?;
        Ok(metas)
    }

    /// Attempts to get a host tool from the SDK manifest. If it fails, this method falls
    /// back to attempting to derive the path to the host tool binary by simply checking
    /// for its existence in `ffx`'s directory.
    /// If you don't want the fallback behavior, use [get_host_tool_from_manifest] instead.
    /// TODO(fxb/84342): Remove when the linked bug is fixed.
    pub fn get_host_tool(&self, name: &str) -> Result<PathBuf> {
        match self.get_host_tool_from_manifest(name) {
            Ok(path) => Ok(path),
            Err(error) => {
                tracing::warn!(
                    "failed to get host tool {} from manifest. Trying local SDK dir: {}",
                    name,
                    error
                );
                let mut ffx_path = std::env::current_exe()
                    .context(format!("getting current ffx exe path for host tool {}", name))?;
                ffx_path = std::fs::canonicalize(ffx_path.clone())
                    .context(format!("canonicalizing ffx path {:?}", ffx_path))?;

                let tool_path = ffx_path
                    .parent()
                    .context(format!("ffx path missing parent {:?}", ffx_path))?
                    .join(name);

                if tool_path.exists() {
                    Ok(tool_path)
                } else {
                    bail!("Host tool '{}' not found after checking in `ffx` directory.", name);
                }
            }
        }
    }

    pub fn get_host_tool_from_manifest(&self, name: &str) -> Result<PathBuf> {
        match self.get_host_tool_relative_path(name) {
            Ok(path) => {
                let result = self.path_prefix.join(path);
                Ok(result)
            }
            Err(error) => Err(error),
        }
    }

    fn get_host_tools(&self) -> impl Iterator<Item = &HostTool> {
        self.metas.iter().filter_map(|meta| match meta {
            Meta::HostTool(tool) | Meta::CompanionHostTool(tool) => Some(tool),
            _ => None,
        })
    }

    fn get_host_tool_relative_path(&self, name: &str) -> Result<PathBuf> {
        let found_tool = self
            .get_host_tools()
            .filter(|tool| tool.name == name)
            .map(|tool| match &tool.files.as_deref() {
                Some([tool_path]) => Ok(tool_path.as_str()),
                Some([tool_path, ..]) => {
                    warn!("Tool '{}' provides multiple files in manifest", name);
                    Ok(tool_path.as_str())
                }
                Some([]) | None => {
                    Err(anyhow!("No executable provided for tool '{}' (file list was empty)", name))
                }
            })
            .collect::<Result<Vec<_>>>()?
            .into_iter()
            .min_by_key(|x| x.len()) // Shortest path is the one with no arch specifier, i.e. the default arch, i.e. the current arch (we hope.)
            .ok_or(anyhow!("Tool '{}' not found in SDK dir", name))?;
        self.get_real_path(found_tool)
    }

    fn get_real_path(&self, path: impl AsRef<str>) -> Result<PathBuf> {
        match &self.real_paths {
            Some(map) => map.get(path.as_ref()).map(PathBuf::from).ok_or(anyhow!(
                "SDK File '{}' has no source in the build directory",
                path.as_ref()
            )),
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
    pub fn get_empty_sdk_with_version(version: SdkVersion) -> Self {
        Sdk { path_prefix: PathBuf::new(), metas: Vec::new(), real_paths: None, version }
    }

    /// Opens a meta file with the given path. Returns a buffered reader.
    fn open_meta(file_path: &Path) -> Result<BufReader<fs::File>> {
        let file = fs::File::open(file_path);
        match file {
            Ok(file) => Ok(BufReader::new(file)),
            Err(error) => return Err(anyhow!("Can't open {:?}: {:?}", file_path, error)),
        }
    }

    /// Allocates a new Sdk using the given atoms.
    ///
    /// All the meta files specified in the atoms are loaded.
    /// The creation succeed only if all the meta files have been loaded successfully.
    fn from_sdk_atoms<T, U>(
        path_prefix: PathBuf,
        atoms: SdkAtoms,
        get_meta: T,
        version: SdkVersion,
    ) -> Result<Self>
    where
        T: Fn(&Path) -> Result<BufReader<U>>,
        U: io::Read,
    {
        let mut metas = Vec::new();
        let mut real_paths = HashMap::new();

        for atom in atoms.atoms.iter() {
            for file in atom.files.iter() {
                real_paths.insert(file.destination.clone(), file.source.clone());
            }

            let meta_file_name = real_paths
                .get(&atom.meta)
                .ok_or(anyhow!("Atom did not specify source for its metadata."))?;
            let full_meta_path = path_prefix.join(meta_file_name);
            let reader = get_meta(&full_meta_path);
            let json_metas = Meta::from_reader(reader?);

            metas.push(
                json_metas.with_context(|| {
                    format!("Can't read json file {}", full_meta_path.display())
                })?,
            );
        }

        Ok(Sdk { path_prefix, metas, real_paths: Some(real_paths), version })
    }
}

////////////////////////////////////////////////////////////////////////////////
// tests

#[cfg(test)]
mod test {
    use sdk_metadata::ElementType;

    use super::*;

    const CORE_MANIFEST: &[u8] = br#"{
      "atoms": [
        {
          "category": "partner",
          "deps": [],
          "files": [
            {
              "destination": "tools/x64/zxdb",
              "source": "host_x64/zxdb"
            },
            {
              "destination": "tools/x64/zxdb-meta.json",
              "source": "host_x64/gen/src/developer/debug/zxdb/zxdb_sdk.meta.json"
            }
          ],
          "gn-label": "//src/developer/debug/zxdb:zxdb_sdk(//build/toolchain:host_x64)",
          "id": "sdk://tools/x64/zxdb",
          "meta": "tools/x64/zxdb-meta.json",
          "type": "host_tool"
        },
        {
          "category": "partner",
          "deps": [],
          "files": [
            {
              "destination": "tools/arm64/symbol-index",
              "source": "host_arm64/symbol-index"
            },
            {
              "destination": "tools/arm64/symbol-index-meta.json",
              "source": "host_arm64/gen/tools/symbol-index/symbol_index_sdk.meta.json"
            }
          ],
          "gn-label": "//tools/symbol-index:symbol_index_sdk(//build/toolchain:host_arm64)",
          "id": "sdk://tools/arm64/symbol-index",
          "meta": "tools/arm64/symbol-index-meta.json",
          "type": "host_tool"
        },
        {
          "category": "partner",
          "deps": [],
          "files": [
            {
              "destination": "tools/x64/symbol-index",
              "source": "host_x64/symbol-index"
            },
            {
              "destination": "tools/x64/symbol-index-meta.json",
              "source": "host_x64/gen/tools/symbol-index/symbol_index_sdk.meta.json"
            }
          ],
          "gn-label": "//tools/symbol-index:symbol_index_sdk(//build/toolchain:host_x64)",
          "id": "sdk://tools/x64/symbol-index",
          "meta": "tools/x64/symbol-index-meta.json",
          "type": "host_tool"
        },
        {
            "category": "partner",
            "deps": [],
            "files": [
              {
                "destination": "ffx_tools/x64/ffx-assembly",
                "source": "host_x64/ffx-assembly"
              },
              {
                "destination": "ffx_tools/x64/ffx-assembly.json",
                "source": "host_x64/ffx-assembly.json"
              },
              {
                "destination": "ffx_tools/x64/ffx-assembly-meta.json",
                "source": "host_x64/gen/src/developer/ffx/plugins/assembly/sdk.meta.json"
              }
            ],
            "gn-label": "//src/developer/ffx/plugins/assembly:sdk(//build/toolchain:host_x64)",
            "id": "sdk://ffx_tools/x64/ffx-assembly",
            "meta": "ffx_tools/x64/ffx-assembly-meta.json",
            "plasa": [],
            "type": "ffx_tool"
          }
      ],
      "ids": []
    }"#;

    fn get_core_manifest_meta(file_path: &Path) -> Result<BufReader<&'static [u8]>> {
        match file_path.to_str() {
            Some("/fuchsia/out/default/host_x64/gen/src/developer/debug/zxdb/zxdb_sdk.meta.json") => Ok(
                BufReader::new(br#"{
                    "files": [
                        "tools/x64/zxdb"
                    ],
                    "name": "zxdb",
                    "root": "tools",
                    "type": "host_tool"
                }"#)),
            Some("/fuchsia/out/default/host_x64/gen/tools/symbol-index/symbol_index_sdk.meta.json") => Ok(
                BufReader::new(br#"{
                    "files": [
                        "tools/x64/symbol-index"
                    ],
                    "name": "symbol-index",
                    "root": "tools",
                    "type": "host_tool"
                }"#)),
            Some("/fuchsia/out/default/host_x64/gen/tools/symbol-index/symbol_index_sdk_legacy.meta.json") => Ok(
                BufReader::new(br#"{
                    "files": [
                        "tools/x64/symbol-index"
                    ],
                    "name": "symbol-index",
                    "root": "tools",
                    "type": "host_tool"
                }"#)),
            Some("/fuchsia/out/default/host_arm64/gen/tools/symbol-index/symbol_index_sdk.meta.json") => Ok(
                BufReader::new(br#"{
                    "files": [
                        "tools/arm64/symbol-index"
                    ],
                    "name": "symbol-index",
                    "root": "tools",
                    "type": "host_tool"
                }"#)),
            Some("/fuchsia/out/default/host_x64/gen/src/developer/ffx/plugins/assembly/sdk.meta.json") => Ok(BufReader::new(br#"{
                "executable": "ffx_tools/x64/ffx-assembly",
                "executable_metadata": "ffx_tools/x64/ffx-assembly.json",
                "files": [
                  "ffx_tools/x64/ffx-assembly",
                  "ffx_tools/x64/ffx-assembly.json"
                ],
                "name": "ffx-assembly",
                "root": "ffx_tools/x64",
                "type": "ffx_tool"
              }"#)),
            _ => Err(anyhow!("No such manifest: {}", file_path.display()))
        }
    }

    const SDK_MANIFEST: &[u8] = br#"{
        "arch": {
            "host": "x86_64-linux-gnu",
            "target": [
                "arm64",
                "x64"
            ]
        },
        "id": "0.20201005.4.1",
        "parts": [
            {
              "meta": "fidl/fuchsia.data/meta.json",
              "type": "fidl_library"
            },
            {
              "meta": "tools/zxdb-meta.json",
              "type": "host_tool"
            }
        ],
        "root": "..",
        "schema_version": "1"
    }"#;

    fn get_sdk_manifest_meta(name: &str) -> Option<BufReader<&'static [u8]>> {
        match name {
            "fidl/fuchsia.data/meta.json" => Some(BufReader::new(
                br#"{
                "deps": [],
                "name": "fuchsia.data",
                "root": "fidl/fuchsia.data",
                "sources": [
                    "fidl/fuchsia.data/data.fidl"
                ],
                "type": "fidl_library"
                }"#,
            )),
            "tools/zxdb-meta.json" => Some(BufReader::new(
                br#"{
                "files": [
                    "tools/zxdb"
                ],
                "name": "zxdb",
                "root": "tools",
                "target_files": {},
                "type": "host_tool"
                }"#,
            )),
            _ => None,
        }
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_core_manifest() {
        let manifest_path: PathBuf = PathBuf::from("/fuchsia/out/default");
        let atoms =
            Sdk::atoms_from_core_manifest(&manifest_path, BufReader::new(CORE_MANIFEST)).unwrap();

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
        assert_eq!("host_tool", atoms[0].ty);
        assert_eq!(2, atoms[0].files.len());
        assert_eq!("host_x64/zxdb", atoms[0].files[0].source);
        assert_eq!("tools/x64/zxdb", atoms[0].files[0].destination);

        assert_eq!("partner", atoms[3].category);
        assert!(atoms[3].deps.is_empty());
        assert_eq!(
            "//src/developer/ffx/plugins/assembly:sdk(//build/toolchain:host_x64)",
            atoms[3].gn_label
        );
        assert_eq!("sdk://ffx_tools/x64/ffx-assembly", atoms[3].id);
        assert_eq!("ffx_tool", atoms[3].ty);
        assert_eq!(3, atoms[3].files.len());
        assert_eq!("host_x64/ffx-assembly", atoms[3].files[0].source);
        assert_eq!("ffx_tools/x64/ffx-assembly", atoms[3].files[0].destination);
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_core_manifest_to_sdk() {
        let manifest_path: PathBuf = PathBuf::from("/fuchsia/out/default");
        let atoms =
            Sdk::atoms_from_core_manifest(&manifest_path, BufReader::new(CORE_MANIFEST)).unwrap();

        let sdk = Sdk::from_sdk_atoms(
            manifest_path.to_owned(),
            atoms,
            get_core_manifest_meta,
            SdkVersion::Unknown,
        )
        .unwrap();

        assert_eq!(4, sdk.metas.len());
        println!("{:#?}", sdk.metas);
        assert!(matches!(
            sdk.metas[0],
            Meta::HostTool(HostTool { kind: ElementType::HostTool, .. })
        ));
        assert!(matches!(
            sdk.metas[1],
            Meta::HostTool(HostTool { kind: ElementType::HostTool, .. })
        ));
        assert!(matches!(
            sdk.metas[2],
            Meta::HostTool(HostTool { kind: ElementType::HostTool, .. })
        ));
        assert!(matches!(sdk.metas[3], Meta::FfxTool(FfxTool { kind: ElementType::FfxTool, .. })));
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_core_manifest_host_tool() {
        let manifest_path: PathBuf = PathBuf::from("/fuchsia/out/default");
        let atoms =
            Sdk::atoms_from_core_manifest(&manifest_path, BufReader::new(CORE_MANIFEST)).unwrap();

        let sdk = Sdk::from_sdk_atoms(
            manifest_path.to_owned(),
            atoms,
            get_core_manifest_meta,
            SdkVersion::Unknown,
        )
        .unwrap();
        let zxdb = sdk.get_host_tool("zxdb").unwrap();

        assert_eq!(PathBuf::from("/fuchsia/out/default/host_x64/zxdb"), zxdb);
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_core_manifest_host_tool_multi_arch() {
        let manifest_path: PathBuf = PathBuf::from("/fuchsia/out/default");
        let atoms =
            Sdk::atoms_from_core_manifest(&manifest_path, BufReader::new(CORE_MANIFEST)).unwrap();

        let sdk = Sdk::from_sdk_atoms(
            manifest_path.to_owned(),
            atoms,
            get_core_manifest_meta,
            SdkVersion::Unknown,
        )
        .unwrap();
        let symbol_index = sdk.get_host_tool("symbol-index").unwrap();

        assert_eq!(PathBuf::from("/fuchsia/out/default/host_x64/symbol-index"), symbol_index);
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_sdk_manifest() {
        let mut version = SdkVersion::Unknown;
        let metas = Sdk::metas_from_sdk_manifest(
            BufReader::new(SDK_MANIFEST),
            &mut version,
            get_sdk_manifest_meta,
        )
        .unwrap();

        assert_eq!(SdkVersion::Version("0.20201005.4.1".to_owned()), version);

        assert_eq!(2, metas.len());
        assert!(matches!(
            metas[0],
            Meta::FidlLibrary(FidlLibrary { kind: ElementType::FidlLibrary, .. })
        ));
        assert!(matches!(metas[1], Meta::HostTool(HostTool { kind: ElementType::HostTool, .. })));
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_sdk_manifest_host_tool() {
        let metas = Sdk::metas_from_sdk_manifest(
            BufReader::new(SDK_MANIFEST),
            &mut SdkVersion::Unknown,
            get_sdk_manifest_meta,
        )
        .unwrap();

        let sdk = Sdk {
            path_prefix: "/foo/bar".into(),
            metas,
            real_paths: None,
            version: SdkVersion::Unknown,
        };
        let zxdb = sdk.get_host_tool("zxdb").unwrap();

        assert_eq!(PathBuf::from("/foo/bar/tools/zxdb"), zxdb);
    }
}
