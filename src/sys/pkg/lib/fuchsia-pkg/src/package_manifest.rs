// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    crate::{
        BlobEntry, MetaContents, MetaPackage, MetaPackageError, MetaSubpackages, Package,
        PackageManifestError, PackageName, PackagePath, PackageVariant,
    },
    anyhow::{Context, Result},
    camino::Utf8Path,
    fuchsia_archive::{self, Utf8Reader},
    fuchsia_hash::Hash,
    fuchsia_merkle::from_slice,
    fuchsia_url::{RepositoryUrl, UnpinnedAbsolutePackageUrl},
    serde::{Deserialize, Serialize},
    std::{
        collections::{BTreeMap, HashMap, HashSet},
        fs::{self, create_dir_all, File},
        io,
        io::{BufReader, Read, Seek, SeekFrom, Write},
        path::Path,
        str,
    },
    tempfile_ext::NamedTempFileExt as _,
    utf8_path::{path_relative_from_file, resolve_path_from_file},
};

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(transparent)]
pub struct PackageManifest(VersionedPackageManifest);

impl PackageManifest {
    /// Blob path used in package manifests to indicate the `meta.far`.
    pub const META_FAR_BLOB_PATH: &'static str = "meta/";

    pub fn blobs(&self) -> &[BlobInfo] {
        match &self.0 {
            VersionedPackageManifest::Version1(manifest) => &manifest.blobs,
        }
    }

    pub fn subpackages(&self) -> &[SubpackageInfo] {
        match &self.0 {
            VersionedPackageManifest::Version1(manifest) => &manifest.subpackages,
        }
    }

    pub fn into_blobs(self) -> Vec<BlobInfo> {
        match self.0 {
            VersionedPackageManifest::Version1(manifest) => manifest.blobs,
        }
    }

    /// Returns the current packages blobs and subpackages.
    pub fn into_blobs_and_subpackages(self) -> (Vec<BlobInfo>, Vec<SubpackageInfo>) {
        match self.0 {
            VersionedPackageManifest::Version1(manifest) => (manifest.blobs, manifest.subpackages),
        }
    }

    pub fn name(&self) -> &PackageName {
        match &self.0 {
            VersionedPackageManifest::Version1(manifest) => &manifest.package.name,
        }
    }

    /// Write a package archive into the `out` file. The source files are relative to the `root_dir`
    /// directory.
    pub async fn archive(
        self,
        root_dir: impl AsRef<Path>,
        out: impl Write,
    ) -> Result<(), PackageManifestError> {
        let root_dir = root_dir.as_ref();

        let mut contents: BTreeMap<_, (_, Box<dyn Read>)> = BTreeMap::new();
        let (meta_far_blob_info, all_blobs) = Self::package_and_subpackage_blobs(self)?;

        let source_path = root_dir.join(&meta_far_blob_info.source_path);
        let mut meta_far_blob = File::open(&source_path).map_err(|err| {
            PackageManifestError::IoErrorWithPath { cause: err, path: source_path }
        })?;
        meta_far_blob.seek(SeekFrom::Start(0))?;
        contents.insert(
            "meta.far".to_string(),
            (meta_far_blob.metadata()?.len(), Box::new(meta_far_blob)),
        );

        for (_merkle_key, blob_info) in all_blobs.iter() {
            let source_path = root_dir.join(&blob_info.source_path);

            let blob_file = File::open(&source_path).map_err(|err| {
                PackageManifestError::IoErrorWithPath { cause: err, path: source_path }
            })?;
            contents.insert(
                blob_info.merkle.to_string(),
                (blob_file.metadata()?.len(), Box::new(blob_file)),
            );
        }

        fuchsia_archive::write(out, contents)?;
        Ok(())
    }

    pub fn package_path(&self) -> PackagePath {
        match &self.0 {
            VersionedPackageManifest::Version1(manifest) => PackagePath::from_name_and_variant(
                manifest.package.name.to_owned(),
                manifest.package.version.to_owned(),
            ),
        }
    }

    pub fn repository(&self) -> Option<&str> {
        match &self.0 {
            VersionedPackageManifest::Version1(manifest) => manifest.repository.as_deref(),
        }
    }

    pub fn set_repository(&mut self, repository: Option<String>) {
        match &mut self.0 {
            VersionedPackageManifest::Version1(manifest) => {
                manifest.repository = repository;
            }
        }
    }

    pub fn package_url(&self) -> Result<Option<UnpinnedAbsolutePackageUrl>> {
        if let Some(url) = self.repository() {
            let repo = RepositoryUrl::parse_host(url.to_string())?;
            return Ok(Some(UnpinnedAbsolutePackageUrl::new(repo, self.name().clone(), None)));
        };
        Ok(None)
    }

    /// Returns the merkle root of the meta.far.
    ///
    /// # Panics
    ///
    /// Panics if the PackageManifest is missing a "meta/" entry
    pub fn hash(&self) -> Hash {
        self.blobs().iter().find(|blob| blob.path == Self::META_FAR_BLOB_PATH).unwrap().merkle
    }

    /// Create a `PackageManifest` and populate a manifest directory given a blobs directory and the top level meta.far hash.
    ///
    /// The `blobs_dir` directory must be a flat file that contains all the package blobs.
    /// The `out_manifest_dir` will be a flat file populated with JSON representations of PackageManifests
    /// corresponding to the subpackages.
    pub fn from_blobs_dir(
        blobs_dir: &Path,
        meta_far_hash: Hash,
        out_manifest_dir: &Path,
    ) -> Result<Self, PackageManifestError> {
        let meta_far_path = blobs_dir.join(meta_far_hash.to_string());

        let mut meta_far_file = File::open(&meta_far_path)?;
        let meta_far_size = meta_far_file.metadata()?.len();

        let mut meta_far = fuchsia_archive::Utf8Reader::new(&mut meta_far_file)?;

        let meta_contents = meta_far.read_file(MetaContents::PATH)?;
        let meta_contents = MetaContents::deserialize(meta_contents.as_slice())?.into_contents();

        // The meta contents are unordered, so sort them to keep things consistent.
        let meta_contents = meta_contents.into_iter().collect::<BTreeMap<_, _>>();

        let meta_package = meta_far.read_file(MetaPackage::PATH)?;
        let meta_package = MetaPackage::deserialize(meta_package.as_slice())?;

        let meta_subpackages = match meta_far.read_file(MetaSubpackages::PATH) {
            Ok(meta_subpackages) => {
                let meta_subpackages =
                    MetaSubpackages::deserialize(meta_subpackages.as_slice())?.into_subpackages();

                // The meta subpackages are unordered, so sort them to keep things consistent.
                meta_subpackages.into_iter().collect::<BTreeMap<_, _>>()
            }
            Err(fuchsia_archive::Error::PathNotPresent(_)) => BTreeMap::new(),
            Err(e) => return Err(e.into()),
        };

        let mut sub_packages = vec![];
        for (name, hash) in meta_subpackages {
            let sub_package_manifest = Self::from_blobs_dir(blobs_dir, hash, out_manifest_dir)?;

            let source_pathbuf = out_manifest_dir.join(format!("{}_package_manifest.json", &hash));
            let source_path = source_pathbuf.as_path();

            let relative_path = Utf8Path::from_path(source_path).unwrap();

            let _ = sub_package_manifest
                .write_with_relative_paths(relative_path)
                .map_err(PackageManifestError::RelativeWrite)?;
            sub_packages.push((name, hash, source_path.to_owned()));
        }

        // Build the PackageManifest of this package.
        let mut builder = PackageManifestBuilder::new(meta_package);

        // Add the meta.far blob. We add this first since some scripts assume the first entry is the
        // meta.far entry.
        builder = builder.add_blob(BlobInfo {
            source_path: meta_far_path.into_os_string().into_string().map_err(|source_path| {
                PackageManifestError::InvalidBlobPath {
                    merkle: meta_far_hash,
                    source_path: source_path.into(),
                }
            })?,
            path: Self::META_FAR_BLOB_PATH.into(),
            merkle: meta_far_hash,
            size: meta_far_size,
        });

        for (blob_path, merkle) in meta_contents.into_iter() {
            let source_path = blobs_dir.join(merkle.to_string());

            if !source_path.exists() {
                return Err(PackageManifestError::IoErrorWithPath {
                    cause: io::ErrorKind::NotFound.into(),
                    path: source_path,
                });
            }

            let size = fs::metadata(&source_path)?.len();

            builder = builder.add_blob(BlobInfo {
                source_path: source_path.into_os_string().into_string().map_err(|source_path| {
                    PackageManifestError::InvalidBlobPath {
                        merkle,
                        source_path: source_path.into(),
                    }
                })?,
                path: blob_path,
                merkle,
                size,
            });
        }

        for (name, merkle, path) in sub_packages {
            builder = builder.add_subpackage(SubpackageInfo {
                manifest_path: path.to_str().expect("better work").to_string(),
                name: name.to_string(),
                merkle,
            });
        }

        Ok(builder.build())
    }

    /// Extract the package blobs from `archive_path` into the `blobs_dir` directory and
    /// extracts all the JSON representations of the subpackages' PackageManifests and
    /// top level PackageManifest into `out_manifest_dir`.
    ///
    /// Returns an in-memory `PackageManifest` for these files.
    pub fn from_archive(
        archive_path: &Path,
        blobs_dir: &Path,
        out_manifest_dir: &Path,
    ) -> Result<Self, PackageManifestError> {
        let archive_file = File::open(archive_path)?;
        let mut archive_reader = Utf8Reader::new(&archive_file)?;

        let far_paths =
            archive_reader.list().map(|entry| entry.path().to_owned()).collect::<Vec<_>>();

        for path in far_paths {
            let blob_path = blobs_dir.join(&path);

            if &path != "meta.far" && !blob_path.as_path().exists() {
                let contents = archive_reader.read_file(&path)?;
                let mut tmp = tempfile::NamedTempFile::new_in(blobs_dir)?;
                tmp.write_all(&contents)?;
                tmp.persist_if_changed(&blob_path)?;
            }
        }

        let meta_far = archive_reader.read_file("meta.far")?;
        let meta_far_hash = from_slice(&meta_far[..]).root();

        let meta_far_path = blobs_dir.join(meta_far_hash.to_string());
        let mut tmp = tempfile::NamedTempFile::new_in(blobs_dir)?;
        tmp.write_all(&meta_far)?;
        tmp.persist_if_changed(meta_far_path)?;

        PackageManifest::from_blobs_dir(blobs_dir, meta_far_hash, out_manifest_dir)
    }

    pub(crate) fn from_package(
        package: Package,
        repository: Option<String>,
    ) -> Result<Self, PackageManifestError> {
        let mut blobs = Vec::with_capacity(package.blobs().len());

        let mut push_blob = |blob_path, blob_entry: BlobEntry| {
            let source_path = blob_entry.source_path();

            blobs.push(BlobInfo {
                source_path: source_path.into_os_string().into_string().map_err(|source_path| {
                    PackageManifestError::InvalidBlobPath {
                        merkle: blob_entry.hash(),
                        source_path: source_path.into(),
                    }
                })?,
                path: blob_path,
                merkle: blob_entry.hash(),
                size: blob_entry.size(),
            });

            Ok::<(), PackageManifestError>(())
        };

        let mut package_blobs = package.blobs();

        // Add the meta.far blob. We add this first since some scripts assume the first entry is the
        // meta.far entry.
        if let Some((blob_path, blob_entry)) = package_blobs.remove_entry(Self::META_FAR_BLOB_PATH)
        {
            push_blob(blob_path, blob_entry)?;
        }

        for (blob_path, blob_entry) in package_blobs {
            push_blob(blob_path, blob_entry)?;
        }

        let package_subpackages = package.subpackages();

        let mut subpackages = Vec::with_capacity(package_subpackages.len());

        for subpackage in package_subpackages {
            subpackages.push(SubpackageInfo {
                manifest_path: subpackage
                    .package_manifest_path
                    .into_os_string()
                    .into_string()
                    .map_err(|package_manifest_path| {
                        PackageManifestError::InvalidSubpackagePath {
                            merkle: subpackage.merkle,
                            path: package_manifest_path.into(),
                        }
                    })?,
                name: subpackage.name.to_string(),
                merkle: subpackage.merkle,
            });
        }

        let manifest_v1 = PackageManifestV1 {
            package: PackageMetadata {
                name: package.meta_package().name().to_owned(),
                version: package.meta_package().variant().to_owned(),
            },
            blobs,
            repository,
            blob_sources_relative: Default::default(),
            subpackages,
        };
        Ok(PackageManifest(VersionedPackageManifest::Version1(manifest_v1)))
    }

    pub fn try_load_from(manifest_path: impl AsRef<Utf8Path>) -> anyhow::Result<Self> {
        fn inner(manifest_path: &Utf8Path) -> anyhow::Result<PackageManifest> {
            let file = File::open(manifest_path)
                .with_context(|| format!("Opening package manifest: {manifest_path}"))?;

            PackageManifest::from_reader(manifest_path, BufReader::new(file))
        }
        inner(manifest_path.as_ref())
    }

    pub fn from_reader(
        manifest_path: impl AsRef<Utf8Path>,
        reader: impl std::io::Read,
    ) -> anyhow::Result<Self> {
        fn inner(
            manifest_path: &Utf8Path,
            reader: impl std::io::Read,
        ) -> anyhow::Result<PackageManifest> {
            let versioned: VersionedPackageManifest = serde_json::from_reader(reader)?;

            let versioned = match versioned {
                VersionedPackageManifest::Version1(manifest) => VersionedPackageManifest::Version1(
                    manifest.resolve_source_paths(manifest_path)?,
                ),
            };

            Ok(PackageManifest(versioned))
        }
        inner(manifest_path.as_ref(), reader)
    }

    fn package_and_subpackage_blobs_impl(
        contents: &mut HashMap<String, BlobInfo>,
        visited_subpackages: &mut HashSet<String>,
        package_manifest: Self,
    ) -> Result<(), PackageManifestError> {
        let (blobs, subpackages) = package_manifest.into_blobs_and_subpackages();
        for blob in blobs {
            contents.insert(blob.merkle.to_string(), blob);
        }
        for sp in subpackages {
            let key = sp.merkle.to_string();
            if !visited_subpackages.contains(&key) {
                visited_subpackages.insert(key);
                let package_manifest = Self::try_load_from(&sp.manifest_path).map_err(|_| {
                    PackageManifestError::InvalidSubpackagePath {
                        merkle: sp.merkle,
                        path: sp.manifest_path.into(),
                    }
                })?;
                Self::package_and_subpackage_blobs_impl(
                    contents,
                    visited_subpackages,
                    package_manifest,
                )?;
            }
        }
        Ok(())
    }

    /// Returns a tuple of BlobInfo corresponding to the top level meta.far blob
    /// and a HashMap containing all of the blobs from all of the subpackages.
    fn package_and_subpackage_blobs(
        self,
    ) -> Result<(BlobInfo, HashMap<String, BlobInfo>), PackageManifestError> {
        let mut contents = HashMap::new();
        let mut visited_subpackages = HashSet::new();
        Self::package_and_subpackage_blobs_impl(
            &mut contents,
            &mut visited_subpackages,
            self.clone(),
        )?;

        let blobs = self.into_blobs();
        for blob in blobs {
            if blob.path == Self::META_FAR_BLOB_PATH
                && contents.remove(&blob.merkle.to_string()).is_some()
            {
                return Ok((blob, contents));
            }
        }
        Err(PackageManifestError::MetaPackage(MetaPackageError::MetaPackageMissing))
    }

    pub fn write_with_relative_paths(self, path: impl AsRef<Utf8Path>) -> anyhow::Result<Self> {
        fn inner(this: PackageManifest, path: &Utf8Path) -> anyhow::Result<PackageManifest> {
            let versioned = match this.0 {
                VersionedPackageManifest::Version1(manifest) => {
                    VersionedPackageManifest::Version1(manifest.write_with_relative_paths(path)?)
                }
            };

            Ok(PackageManifest(versioned))
        }
        inner(self, path.as_ref())
    }
}

pub struct PackageManifestBuilder {
    manifest: PackageManifestV1,
}

impl PackageManifestBuilder {
    pub fn new(meta_package: MetaPackage) -> Self {
        Self {
            manifest: PackageManifestV1 {
                package: PackageMetadata {
                    name: meta_package.name().to_owned(),
                    version: meta_package.variant().to_owned(),
                },
                blobs: vec![],
                repository: None,
                blob_sources_relative: Default::default(),
                subpackages: vec![],
            },
        }
    }

    pub fn repository(mut self, repository: impl Into<String>) -> Self {
        self.manifest.repository = Some(repository.into());
        self
    }

    pub fn add_blob(mut self, info: BlobInfo) -> Self {
        self.manifest.blobs.push(info);
        self
    }

    pub fn add_subpackage(mut self, info: SubpackageInfo) -> Self {
        self.manifest.subpackages.push(info);
        self
    }

    pub fn build(self) -> PackageManifest {
        PackageManifest(VersionedPackageManifest::Version1(self.manifest))
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(tag = "version")]
enum VersionedPackageManifest {
    #[serde(rename = "1")]
    Version1(PackageManifestV1),
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
struct PackageManifestV1 {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    repository: Option<String>,
    package: PackageMetadata,
    blobs: Vec<BlobInfo>,

    /// Are the blob source_paths relative to the working dir (default, as made
    /// by 'pm') or the file containing the serialized manifest (new, portable,
    /// behavior)
    #[serde(default, skip_serializing_if = "RelativeTo::is_default")]
    // TODO(fxbug.dev/114780): rename this to `paths_relative` since it applies
    // to both blobs and subpackages. (I'd change it now, but it's encoded in
    // JSON files so we may need a soft transition to support both at first.)
    blob_sources_relative: RelativeTo,

    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    subpackages: Vec<SubpackageInfo>,
}

impl PackageManifestV1 {
    pub fn write_with_relative_paths(
        self,
        manifest_path: impl AsRef<Utf8Path>,
    ) -> anyhow::Result<PackageManifestV1> {
        fn inner(
            this: PackageManifestV1,
            manifest_path: &Utf8Path,
        ) -> anyhow::Result<PackageManifestV1> {
            let manifest = if let RelativeTo::WorkingDir = &this.blob_sources_relative {
                // manifest contains working-dir relative source paths, make
                // them relative to the file, instead.
                let blobs = this
                    .blobs
                    .into_iter()
                    .map(|blob| relativize_blob_source_path(blob, manifest_path))
                    .collect::<anyhow::Result<_>>()?;
                let subpackages = this
                    .subpackages
                    .into_iter()
                    .map(|subpackage| {
                        relativize_subpackage_manifest_path(subpackage, manifest_path)
                    })
                    .collect::<anyhow::Result<_>>()?;
                PackageManifestV1 {
                    blobs,
                    subpackages,
                    blob_sources_relative: RelativeTo::File,
                    ..this
                }
            } else {
                this
            };

            let versioned_manifest = VersionedPackageManifest::Version1(manifest.clone());

            let mut tmp = if let Some(parent) = manifest_path.parent() {
                create_dir_all(parent)?;
                tempfile::NamedTempFile::new_in(parent)?
            } else {
                tempfile::NamedTempFile::new()?
            };

            serde_json::to_writer(&mut tmp, &versioned_manifest)?;
            tmp.persist_if_changed(manifest_path)?;

            Ok(manifest)
        }
        inner(self, manifest_path.as_ref())
    }

    pub fn resolve_source_paths(self, manifest_path: impl AsRef<Utf8Path>) -> anyhow::Result<Self> {
        fn inner(
            this: PackageManifestV1,
            manifest_path: &Utf8Path,
        ) -> anyhow::Result<PackageManifestV1> {
            if let RelativeTo::File = &this.blob_sources_relative {
                let blobs = this
                    .blobs
                    .into_iter()
                    .map(|blob| resolve_blob_source_path(blob, manifest_path))
                    .collect::<anyhow::Result<_>>()?;
                let subpackages = this
                    .subpackages
                    .into_iter()
                    .map(|subpackage| resolve_subpackage_manifest_path(subpackage, manifest_path))
                    .collect::<anyhow::Result<_>>()?;
                let blob_sources_relative = RelativeTo::WorkingDir;
                Ok(PackageManifestV1 { blobs, subpackages, blob_sources_relative, ..this })
            } else {
                Ok(this)
            }
        }
        inner(self, manifest_path.as_ref())
    }
}

/// If the path is a relative path, what is it relative from?
///
/// If 'RelativeTo::WorkingDir', then the path is assumed to be relative to the
/// working dir, and can be used directly as a path.
///
/// If 'RelativeTo::File', then the path is relative to the file that contained
/// the path.  To use the path, it must be resolved against the path to the
/// file.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize, Default)]
pub enum RelativeTo {
    #[serde(rename = "working_dir")]
    #[default]
    WorkingDir,
    #[serde(rename = "file")]
    File,
}

impl RelativeTo {
    pub(crate) fn is_default(&self) -> bool {
        matches!(self, RelativeTo::WorkingDir)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
struct PackageMetadata {
    name: PackageName,
    version: PackageVariant,
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize, PartialOrd, Ord)]
pub struct BlobInfo {
    pub source_path: String,
    pub path: String,
    pub merkle: fuchsia_merkle::Hash,
    pub size: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
pub struct SubpackageInfo {
    /// Path to a PackageManifest for the subpackage.
    pub manifest_path: String,

    /// The package-relative name of this declared subpackage.
    pub name: String,

    /// The package hash (meta.far merkle) of the subpackage.
    pub merkle: fuchsia_merkle::Hash,
}

fn relativize_blob_source_path(
    blob: BlobInfo,
    manifest_path: &Utf8Path,
) -> anyhow::Result<BlobInfo> {
    let source_path = path_relative_from_file(blob.source_path, manifest_path)?;
    let source_path = source_path.into_string();

    Ok(BlobInfo { source_path, ..blob })
}

fn resolve_blob_source_path(blob: BlobInfo, manifest_path: &Utf8Path) -> anyhow::Result<BlobInfo> {
    let source_path = resolve_path_from_file(&blob.source_path, manifest_path)
        .with_context(|| format!("Resolving blob path: {}", blob.source_path))?
        .into_string();
    Ok(BlobInfo { source_path, ..blob })
}

fn relativize_subpackage_manifest_path(
    subpackage: SubpackageInfo,
    manifest_path: &Utf8Path,
) -> anyhow::Result<SubpackageInfo> {
    let manifest_path = path_relative_from_file(subpackage.manifest_path, manifest_path)?;
    let manifest_path = manifest_path.into_string();

    Ok(SubpackageInfo { manifest_path, ..subpackage })
}

fn resolve_subpackage_manifest_path(
    subpackage: SubpackageInfo,
    manifest_path: &Utf8Path,
) -> anyhow::Result<SubpackageInfo> {
    let manifest_path = resolve_path_from_file(&subpackage.manifest_path, manifest_path)
        .with_context(|| {
            format!("Resolving subpackage manifest path: {}", subpackage.manifest_path)
        })?
        .into_string();
    Ok(SubpackageInfo { manifest_path, ..subpackage })
}

#[cfg(test)]
mod tests {
    use {
        super::*,
        crate::{path_to_string::PathToStringExt, PackageBuildManifest, PackageBuilder},
        assert_matches::assert_matches,
        camino::{Utf8Path, Utf8PathBuf},
        fuchsia_merkle::Hash,
        fuchsia_url::RelativePackageUrl,
        pretty_assertions::assert_eq,
        serde_json::{json, Value},
        std::fs::File,
        std::path::PathBuf,
        tempfile::{NamedTempFile, TempDir},
    };

    pub(super) fn zeros_hash_str() -> &'static str {
        "0000000000000000000000000000000000000000000000000000000000000000"
    }

    pub(super) fn ones_hash_str() -> &'static str {
        "1111111111111111111111111111111111111111111111111111111111111111"
    }

    pub(super) fn zeros_hash() -> Hash {
        zeros_hash_str().parse().unwrap()
    }

    pub(super) fn ones_hash() -> Hash {
        ones_hash_str().parse().unwrap()
    }

    pub struct TestEnv {
        pub dir_path: Utf8PathBuf,
        pub manifest_path: Utf8PathBuf,
        pub manifest_dir: Utf8PathBuf,
        pub subpackage_path: Utf8PathBuf,
        pub subpackage_dir: Utf8PathBuf,
        pub data_dir: Utf8PathBuf,
    }

    impl TestEnv {
        pub fn new() -> Self {
            let dir = TempDir::new().unwrap();
            let dir_path = Utf8Path::from_path(dir.path()).unwrap();

            let manifest_dir = dir_path.join("manifest_dir");
            let manifest_path = manifest_dir.join("package_manifest.json");

            let subpackage_dir = dir_path.join("subpackage_manifests");
            let subpackage_path = subpackage_dir.join(zeros_hash_str());

            let data_dir = dir_path.join("data_source");

            TestEnv {
                dir_path: dir_path.to_path_buf(),
                manifest_path,
                manifest_dir,
                subpackage_path,
                subpackage_dir,
                data_dir,
            }
        }

        pub fn build_files(&self) {
            std::fs::create_dir_all(&self.data_dir).unwrap();
            std::fs::create_dir_all(&self.subpackage_dir).unwrap();
            std::fs::create_dir_all(&self.manifest_dir).unwrap();
        }
    }

    #[test]
    fn test_version1_serialization() {
        let manifest = PackageManifest(VersionedPackageManifest::Version1(PackageManifestV1 {
            package: PackageMetadata {
                name: "example".parse().unwrap(),
                version: "0".parse().unwrap(),
            },
            blobs: vec![BlobInfo {
                source_path: "../p1".into(),
                path: "data/p1".into(),
                merkle: zeros_hash(),
                size: 1,
            }],
            subpackages: vec![],
            repository: None,
            blob_sources_relative: Default::default(),
        }));

        assert_eq!(
            serde_json::to_value(manifest).unwrap(),
            json!(
                {
                    "version": "1",
                    "package": {
                        "name": "example",
                        "version": "0"
                    },
                    "blobs": [
                        {
                            "source_path": "../p1",
                            "path": "data/p1",
                            "merkle": "0000000000000000000000000000000000000000000000000000000000000000",
                            "size": 1
                        },
                    ]
                }
            )
        );

        let manifest = PackageManifest(VersionedPackageManifest::Version1(PackageManifestV1 {
            package: PackageMetadata {
                name: "example".parse().unwrap(),
                version: "0".parse().unwrap(),
            },
            blobs: vec![BlobInfo {
                source_path: "../p1".into(),
                path: "data/p1".into(),
                merkle: zeros_hash(),
                size: 1,
            }],
            subpackages: vec![],
            repository: Some("testrepository.org".into()),
            blob_sources_relative: RelativeTo::File,
        }));

        assert_eq!(
            serde_json::to_value(manifest).unwrap(),
            json!(
                {
                    "version": "1",
                    "repository": "testrepository.org",
                    "package": {
                        "name": "example",
                        "version": "0"
                    },
                    "blobs": [
                        {
                            "source_path": "../p1",
                            "path": "data/p1",
                            "merkle": zeros_hash_str(),
                            "size": 1
                        },
                    ],
                    "blob_sources_relative": "file"
                }
            )
        );
    }

    #[test]
    fn test_version1_deserialization() {
        let manifest = serde_json::from_value::<VersionedPackageManifest>(json!(
            {
                "version": "1",
                "repository": "testrepository.org",
                "package": {
                    "name": "example",
                    "version": "0"
                },
                "blobs": [
                    {
                        "source_path": "../p1",
                        "path": "data/p1",
                        "merkle": zeros_hash_str(),
                        "size": 1
                    },
                ]
            }
        ))
        .expect("valid json");

        assert_eq!(
            manifest,
            VersionedPackageManifest::Version1(PackageManifestV1 {
                package: PackageMetadata {
                    name: "example".parse().unwrap(),
                    version: "0".parse().unwrap(),
                },
                blobs: vec![BlobInfo {
                    source_path: "../p1".into(),
                    path: "data/p1".into(),
                    merkle: zeros_hash(),
                    size: 1
                }],
                subpackages: vec![],
                repository: Some("testrepository.org".into()),
                blob_sources_relative: Default::default(),
            })
        );

        let manifest = serde_json::from_value::<VersionedPackageManifest>(json!(
            {
                "version": "1",
                "package": {
                    "name": "example",
                    "version": "0"
                },
                "blobs": [
                    {
                        "source_path": "../p1",
                        "path": "data/p1",
                        "merkle": zeros_hash_str(),
                        "size": 1
                    },
                ],
                "blob_sources_relative": "file"
            }
        ))
        .expect("valid json");

        assert_eq!(
            manifest,
            VersionedPackageManifest::Version1(PackageManifestV1 {
                package: PackageMetadata {
                    name: "example".parse().unwrap(),
                    version: "0".parse().unwrap(),
                },
                blobs: vec![BlobInfo {
                    source_path: "../p1".into(),
                    path: "data/p1".into(),
                    merkle: zeros_hash(),
                    size: 1
                }],
                subpackages: vec![],
                repository: None,
                blob_sources_relative: RelativeTo::File,
            })
        )
    }

    #[test]
    fn test_create_package_manifest_from_package() {
        let mut package_builder = Package::builder("package-name".parse().unwrap());
        package_builder.add_entry(
            String::from("bin/my_prog"),
            zeros_hash(),
            PathBuf::from("src/bin/my_prog"),
            1,
        );
        let package = package_builder.build().unwrap();
        let package_manifest = PackageManifest::from_package(package, None).unwrap();
        assert_eq!(&"package-name".parse::<PackageName>().unwrap(), package_manifest.name());
        assert_eq!(None, package_manifest.repository());
    }

    #[test]
    fn test_from_blobs_dir() {
        let temp = TempDir::new().unwrap();
        let temp_dir = Utf8Path::from_path(temp.path()).unwrap();

        let gen_dir = temp_dir.join("gen");
        std::fs::create_dir_all(&gen_dir).unwrap();

        let blobs_dir = temp_dir.join("blobs");
        std::fs::create_dir_all(&blobs_dir).unwrap();

        let manifests_dir = temp_dir.join("manifests");
        std::fs::create_dir_all(&manifests_dir).unwrap();

        // Helper to write some content into a blob.
        let write_blob = |contents| {
            let mut builder = fuchsia_merkle::MerkleTreeBuilder::new();
            builder.write(contents);
            let hash = builder.finish().root();

            let path = blobs_dir.join(hash.to_string());
            std::fs::write(&path, contents).unwrap();

            (path, hash)
        };

        // Create a package.
        let (file1_path, file1_hash) = write_blob(b"file 1");
        let (file2_path, file2_hash) = write_blob(b"file 2");

        std::fs::create_dir_all(gen_dir.join("meta")).unwrap();
        let meta_package_path = gen_dir.join("meta").join("package");
        std::fs::write(&meta_package_path, r#"{"name":"package","version":"0"}"#).unwrap();

        let external_contents = BTreeMap::from([
            ("file-1".into(), file1_path.to_string()),
            ("file-2".into(), file2_path.to_string()),
        ]);

        let far_contents =
            BTreeMap::from([(MetaPackage::PATH.into(), meta_package_path.to_string())]);

        let package_build_manifest =
            PackageBuildManifest::from_external_and_far_contents(external_contents, far_contents)
                .unwrap();

        let gen_meta_far_path = temp_dir.join("meta.far");
        let _package_manifest = crate::build::build(
            &package_build_manifest,
            &gen_meta_far_path,
            "package",
            vec![],
            None,
        )
        .unwrap();

        // Compute the meta.far hash, and copy it into the blobs/ directory.
        let meta_far_bytes = std::fs::read(&gen_meta_far_path).unwrap();
        let mut merkle_builder = fuchsia_merkle::MerkleTreeBuilder::new();
        merkle_builder.write(&meta_far_bytes);
        let meta_far_hash = merkle_builder.finish().root();

        let meta_far_path = blobs_dir.join(meta_far_hash.to_string());
        std::fs::write(&meta_far_path, &meta_far_bytes).unwrap();

        // We should be able to create a manifest from the blob directory that matches the one
        // created by the builder.
        assert_eq!(
            PackageManifest::from_blobs_dir(
                blobs_dir.as_std_path(),
                meta_far_hash,
                manifests_dir.as_std_path()
            )
            .unwrap(),
            PackageManifest(VersionedPackageManifest::Version1(PackageManifestV1 {
                package: PackageMetadata {
                    name: "package".parse().unwrap(),
                    version: PackageVariant::zero(),
                },
                blobs: vec![
                    BlobInfo {
                        source_path: meta_far_path.to_string(),
                        path: PackageManifest::META_FAR_BLOB_PATH.into(),
                        merkle: meta_far_hash,
                        size: 12288,
                    },
                    BlobInfo {
                        source_path: file1_path.to_string(),
                        path: "file-1".into(),
                        merkle: file1_hash,
                        size: 6,
                    },
                    BlobInfo {
                        source_path: file2_path.to_string(),
                        path: "file-2".into(),
                        merkle: file2_hash,
                        size: 6,
                    },
                ],
                subpackages: vec![],
                repository: None,
                blob_sources_relative: RelativeTo::WorkingDir,
            }))
        );
    }

    #[test]
    fn test_load_from_simple() {
        let env = TestEnv::new();
        env.build_files();

        let expected_blob_source_path = &env.data_dir.join("p1").to_string();

        let manifest = PackageManifest(VersionedPackageManifest::Version1(PackageManifestV1 {
            package: PackageMetadata {
                name: "example".parse().unwrap(),
                version: "0".parse().unwrap(),
            },
            blobs: vec![BlobInfo {
                source_path: expected_blob_source_path.clone(),
                path: "data/p1".into(),
                merkle: zeros_hash(),
                size: 1,
            }],
            subpackages: vec![SubpackageInfo {
                manifest_path: env.subpackage_path.to_string(),
                name: "subpackage0".into(),
                merkle: zeros_hash(),
            }],
            repository: None,
            blob_sources_relative: RelativeTo::WorkingDir,
        }));

        let manifest_file = File::create(&env.manifest_path).unwrap();
        serde_json::to_writer(manifest_file, &manifest).unwrap();

        let loaded_manifest = PackageManifest::try_load_from(&env.manifest_path).unwrap();
        assert_eq!(loaded_manifest.name(), &"example".parse::<PackageName>().unwrap());

        let (blobs, subpackages) = loaded_manifest.into_blobs_and_subpackages();

        assert_eq!(blobs.len(), 1);
        let blob = blobs.first().unwrap();
        assert_eq!(blob.path, "data/p1");

        assert_eq!(&blob.source_path, expected_blob_source_path);

        assert_eq!(subpackages.len(), 1);
        let subpackage = subpackages.first().unwrap();
        assert_eq!(subpackage.name, "subpackage0");
        assert_eq!(&subpackage.manifest_path, &env.subpackage_path.to_string());
    }

    #[test]
    fn test_load_from_resolves_source_paths() {
        let env = TestEnv::new();
        env.build_files();

        let manifest = PackageManifest(VersionedPackageManifest::Version1(PackageManifestV1 {
            package: PackageMetadata {
                name: "example".parse().unwrap(),
                version: "0".parse().unwrap(),
            },
            blobs: vec![BlobInfo {
                source_path: "../data_source/p1".into(),
                path: "data/p1".into(),
                merkle: zeros_hash(),
                size: 1,
            }],
            subpackages: vec![SubpackageInfo {
                manifest_path: "../subpackage_manifests/0000000000000000000000000000000000000000000000000000000000000000".into(),
                name: "subpackage0".into(),
                merkle: zeros_hash(),
            }],
            repository: None,
            blob_sources_relative: RelativeTo::File,
        }));

        let manifest_file = File::create(&env.manifest_path).unwrap();
        serde_json::to_writer(manifest_file, &manifest).unwrap();

        let loaded_manifest = PackageManifest::try_load_from(&env.manifest_path).unwrap();
        assert_eq!(
            loaded_manifest,
            PackageManifest(VersionedPackageManifest::Version1(PackageManifestV1 {
                package: PackageMetadata {
                    name: "example".parse::<PackageName>().unwrap(),
                    version: "0".parse().unwrap(),
                },
                blobs: vec![BlobInfo {
                    source_path: env.data_dir.join("p1").to_string(),
                    path: "data/p1".into(),
                    merkle: zeros_hash(),
                    size: 1,
                }],
                subpackages: vec![SubpackageInfo {
                    manifest_path: env.subpackage_path.to_string(),
                    name: "subpackage0".into(),
                    merkle: zeros_hash(),
                }],
                repository: None,
                blob_sources_relative: RelativeTo::WorkingDir,
            }))
        );
    }

    #[test]
    fn test_package_and_subpackage_blobs_meta_far_error() {
        let env = TestEnv::new();
        env.build_files();

        let manifest = PackageManifest(VersionedPackageManifest::Version1(PackageManifestV1 {
            package: PackageMetadata {
                name: "example".parse().unwrap(),
                version: "0".parse().unwrap(),
            },
            blobs: vec![BlobInfo {
                source_path: "../data_source/p1".into(),
                path: "data/p1".into(),
                merkle: zeros_hash(),
                size: 1,
            }],
            subpackages: vec![SubpackageInfo {
                manifest_path: "../subpackage_manifests/0000000000000000000000000000000000000000000000000000000000000000".into(),
                name: "subpackage0".into(),
                merkle: zeros_hash(),
            }],
            repository: None,
            blob_sources_relative: RelativeTo::File,
        }));

        let manifest_file = File::create(&env.manifest_path).unwrap();
        serde_json::to_writer(manifest_file, &manifest).unwrap();

        let sub_manifest = PackageManifest(VersionedPackageManifest::Version1(PackageManifestV1 {
            package: PackageMetadata {
                name: "sub_manifest".parse().unwrap(),
                version: "0".parse().unwrap(),
            },
            blobs: vec![BlobInfo {
                source_path: "../data_source/p2".into(),
                path: "data/p2".into(),
                merkle: ones_hash(),
                size: 1,
            }],
            subpackages: vec![],
            repository: None,
            blob_sources_relative: RelativeTo::File,
        }));

        let sub_manifest_file = File::create(&env.subpackage_path).unwrap();
        serde_json::to_writer(sub_manifest_file, &sub_manifest).unwrap();

        let loaded_manifest = PackageManifest::try_load_from(&env.manifest_path).unwrap();

        let result = loaded_manifest.package_and_subpackage_blobs();
        assert_matches!(
            result,
            Err(PackageManifestError::MetaPackage(MetaPackageError::MetaPackageMissing))
        );
    }

    #[test]
    fn test_package_and_subpackage_blobs() {
        let env = TestEnv::new();
        let subsubpackage_dir = &env.dir_path.join("subsubpackage_manifests");

        let expected_subsubpackage_manifest_path =
            subsubpackage_dir.join(zeros_hash_str()).to_string();

        env.build_files();
        std::fs::create_dir_all(subsubpackage_dir).unwrap();

        let manifest = PackageManifest(VersionedPackageManifest::Version1(PackageManifestV1 {
            package: PackageMetadata {
                name: "example".parse().unwrap(),
                version: "0".parse().unwrap(),
            },
            blobs: vec![BlobInfo {
                source_path: "../data_source/p1".into(),
                path: "data/p1".into(),
                merkle: zeros_hash(),
                size: 1,
            }, BlobInfo {
                source_path: "../data_source/p2".into(),
                path: "meta/".into(),
                merkle: Hash::from([2; fuchsia_hash::HASH_SIZE]),
                size: 1,
            }],
            subpackages: vec![SubpackageInfo {
                manifest_path: "../subpackage_manifests/0000000000000000000000000000000000000000000000000000000000000000".into(),
                name: "subpackage0".into(),
                merkle: ones_hash(),
            }],
            repository: None,
            blob_sources_relative: RelativeTo::File,
        }));

        let manifest_file = File::create(&env.manifest_path).unwrap();
        serde_json::to_writer(manifest_file, &manifest).unwrap();

        let sub_manifest = PackageManifest(VersionedPackageManifest::Version1(PackageManifestV1 {
            package: PackageMetadata {
                name: "sub_manifest".parse().unwrap(),
                version: "0".parse().unwrap(),
            },
            blobs: vec![BlobInfo {
                source_path: "../data_source/p3".into(),
                path: "data/p3".into(),
                merkle: "3333333333333333333333333333333333333333333333333333333333333333"
                    .parse()
                    .unwrap(),
                size: 1,
            }, BlobInfo {
                source_path: "../data_source/p4".into(),
                path: "meta/".into(),
                merkle: "4444444444444444444444444444444444444444444444444444444444444444"
                    .parse()
                    .unwrap(),
                size: 1,
            }],
            subpackages: vec![SubpackageInfo {
                manifest_path: "../subsubpackage_manifests/0000000000000000000000000000000000000000000000000000000000000000".into(),
                name: "subsubpackage0".into(),
                merkle: "4444444444444444444444444444444444444444444444444444444444444444".parse()
                .unwrap(),
            }],
            repository: None,
            blob_sources_relative: RelativeTo::File,
        }));

        let sub_manifest_file = File::create(&env.subpackage_path).unwrap();
        serde_json::to_writer(sub_manifest_file, &sub_manifest).unwrap();

        let sub_sub_manifest =
            PackageManifest(VersionedPackageManifest::Version1(PackageManifestV1 {
                package: PackageMetadata {
                    name: "sub_sub_manifest".parse().unwrap(),
                    version: "0".parse().unwrap(),
                },
                blobs: vec![BlobInfo {
                    source_path: "../data_source/p5".into(),
                    path: "meta/".into(),
                    merkle: "5555555555555555555555555555555555555555555555555555555555555555"
                        .parse()
                        .unwrap(),
                    size: 1,
                }],
                subpackages: vec![],
                repository: None,
                blob_sources_relative: RelativeTo::File,
            }));

        let sub_sub_manifest_file = File::create(expected_subsubpackage_manifest_path).unwrap();
        serde_json::to_writer(sub_sub_manifest_file, &sub_sub_manifest).unwrap();

        let loaded_manifest = PackageManifest::try_load_from(&env.manifest_path).unwrap();
        let expected_meta_far_source_path = env.data_dir.join("p2").to_string();

        let (meta_far, contents) = loaded_manifest.package_and_subpackage_blobs().unwrap();
        assert_eq!(
            meta_far,
            BlobInfo {
                source_path: expected_meta_far_source_path,
                path: "meta/".into(),
                merkle: Hash::from([2; fuchsia_hash::HASH_SIZE]),
                size: 1,
            }
        );

        // Does not contain top level meta.far
        assert_eq!(contents.len(), 4);
        assert_eq!(
            contents.get(zeros_hash_str()),
            Some(&BlobInfo {
                source_path: env.data_dir.join("p1").to_string(),
                path: "data/p1".into(),
                merkle: zeros_hash(),
                size: 1,
            })
        );
        assert_eq!(
            contents.get("3333333333333333333333333333333333333333333333333333333333333333"),
            Some(&BlobInfo {
                source_path: env.data_dir.join("p3").to_string(),
                path: "data/p3".into(),
                merkle: "3333333333333333333333333333333333333333333333333333333333333333"
                    .parse()
                    .unwrap(),
                size: 1,
            })
        );
        assert_eq!(
            contents.get("4444444444444444444444444444444444444444444444444444444444444444"),
            Some(&BlobInfo {
                source_path: env.data_dir.join("p4").to_string(),
                path: "meta/".into(),
                merkle: "4444444444444444444444444444444444444444444444444444444444444444"
                    .parse()
                    .unwrap(),
                size: 1,
            })
        );
        assert_eq!(
            contents.get("5555555555555555555555555555555555555555555555555555555555555555"),
            Some(&BlobInfo {
                source_path: env.data_dir.join("p5").to_string(),
                path: "meta/".into(),
                merkle: "5555555555555555555555555555555555555555555555555555555555555555"
                    .parse()
                    .unwrap(),
                size: 1,
            })
        );
    }

    #[test]
    fn test_package_and_subpackage_blobs_deduped() {
        let env = TestEnv::new();
        env.build_files();

        let expected_blob_source_path_1 = env.data_dir.join("p1").to_string();
        let expected_meta_far_source_path = env.data_dir.join("p2").to_string();
        let expected_blob_source_path_2 = env.data_dir.join("p3").to_string();

        let manifest = PackageManifest(VersionedPackageManifest::Version1(PackageManifestV1 {
            package: PackageMetadata {
                name: "example".parse().unwrap(),
                version: "0".parse().unwrap(),
            },
            blobs: vec![BlobInfo {
                source_path: "../data_source/p1".into(),
                path: "data/p1".into(),
                merkle: zeros_hash(),
                size: 1,
            }, BlobInfo {
                source_path: "../data_source/p2".into(),
                path: "meta/".into(),
                merkle:  Hash::from([2; fuchsia_hash::HASH_SIZE]),
                size: 1,
            }],
            subpackages: vec![SubpackageInfo {
                manifest_path: "../subpackage_manifests/0000000000000000000000000000000000000000000000000000000000000000".into(),
                name: "subpackage0".into(),
                merkle: ones_hash(),
            },
            SubpackageInfo {
                manifest_path: "../subpackage_manifests/0000000000000000000000000000000000000000000000000000000000000001".into(),
                name: "subpackage1".into(),
                merkle: ones_hash(),
            }
            ],
            repository: None,
            blob_sources_relative: RelativeTo::File,
        }));

        let manifest_file = File::create(&env.manifest_path).unwrap();
        serde_json::to_writer(manifest_file, &manifest).unwrap();

        let sub_manifest = PackageManifest(VersionedPackageManifest::Version1(PackageManifestV1 {
            package: PackageMetadata {
                name: "sub_manifest".parse().unwrap(),
                version: "0".parse().unwrap(),
            },
            blobs: vec![
                BlobInfo {
                    source_path: "../data_source/p3".into(),
                    path: "data/p3".into(),
                    merkle: "3333333333333333333333333333333333333333333333333333333333333333"
                        .parse()
                        .unwrap(),
                    size: 1,
                },
                BlobInfo {
                    source_path: "../data_source/p4".into(),
                    path: "meta/".into(),
                    merkle: "4444444444444444444444444444444444444444444444444444444444444444"
                        .parse()
                        .unwrap(),
                    size: 1,
                },
            ],
            subpackages: vec![],
            repository: None,
            blob_sources_relative: RelativeTo::File,
        }));

        let sub_manifest_file = File::create(&env.subpackage_path).unwrap();
        serde_json::to_writer(sub_manifest_file, &sub_manifest).unwrap();

        let loaded_manifest = PackageManifest::try_load_from(&env.manifest_path).unwrap();

        let (meta_far, contents) = loaded_manifest.package_and_subpackage_blobs().unwrap();
        assert_eq!(
            meta_far,
            BlobInfo {
                source_path: expected_meta_far_source_path,
                path: "meta/".into(),
                merkle: Hash::from([2; fuchsia_hash::HASH_SIZE]),
                size: 1,
            }
        );

        // Does not contain meta.far
        assert_eq!(contents.len(), 3);
        assert_eq!(
            contents.get("0000000000000000000000000000000000000000000000000000000000000000"),
            Some(&BlobInfo {
                source_path: expected_blob_source_path_1,
                path: "data/p1".into(),
                merkle: zeros_hash(),
                size: 1,
            })
        );
        assert_eq!(
            contents.get("3333333333333333333333333333333333333333333333333333333333333333"),
            Some(&BlobInfo {
                source_path: expected_blob_source_path_2,
                path: "data/p3".into(),
                merkle: "3333333333333333333333333333333333333333333333333333333333333333"
                    .parse()
                    .unwrap(),
                size: 1,
            })
        );
    }

    #[test]
    fn test_from_package_archive_bogus() {
        let temp = TempDir::new().unwrap();
        let temp_blobs_dir = temp.into_path();

        let temp = TempDir::new().unwrap();
        let temp_manifest_dir = temp.into_path();

        let temp_archive = TempDir::new().unwrap();
        let temp_archive_dir = temp_archive.path();

        let result =
            PackageManifest::from_archive(temp_archive_dir, &temp_blobs_dir, &temp_manifest_dir);
        assert!(result.is_err())
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_from_package_manifest_archive_manifest() {
        let outdir = TempDir::new().unwrap();

        let sub_outdir = outdir.path().join("subpackage_manifests");
        std::fs::create_dir(&sub_outdir).unwrap();

        // Create a file to write to the sub package metafar
        let sub_far_source_file_path = NamedTempFile::new_in(&sub_outdir).unwrap();
        std::fs::write(&sub_far_source_file_path, "some data for sub far").unwrap();

        // Create a file to include as a blob
        let sub_blob_source_file_path = sub_outdir.as_path().join("sub_blob_a");
        let blob_contents = "sub some data for blob";
        std::fs::write(&sub_blob_source_file_path, blob_contents).unwrap();

        // Create a file to include as a blob
        let sub_blob_source_file_path2 = sub_outdir.as_path().join("sub_blob_b");
        let blob_contents = "sub some data for blob2";
        std::fs::write(&sub_blob_source_file_path2, blob_contents).unwrap();

        // Create the sub builder
        let mut sub_builder = PackageBuilder::new("some_pkg_name");
        sub_builder
            .add_file_as_blob(
                "sub_blob_a",
                sub_blob_source_file_path.as_path().path_to_string().unwrap(),
            )
            .unwrap();
        sub_builder
            .add_file_as_blob(
                "sub_blob_b",
                sub_blob_source_file_path2.as_path().path_to_string().unwrap(),
            )
            .unwrap();
        sub_builder
            .add_file_to_far(
                "meta/some/file",
                sub_far_source_file_path.path().path_to_string().unwrap(),
            )
            .unwrap();

        let sub_metafar_path = sub_outdir.as_path().join("meta.far");
        let sub_manifest = sub_builder.build(&sub_outdir, &sub_metafar_path).unwrap();

        let manifest_outdir = TempDir::new().unwrap().into_path();
        let subpackage_manifest_path =
            manifest_outdir.join(format!("{}_package_manifest.json", sub_manifest.hash()));

        serde_json::to_writer(
            std::fs::File::create(&subpackage_manifest_path).unwrap(),
            &sub_manifest,
        )
        .unwrap();

        let subpackage_url = "subpackage_manifests".parse::<RelativePackageUrl>().unwrap();

        let metafar_path = outdir.path().join("meta.far");

        // Create a file to write to the package metafar
        let far_source_file_path = NamedTempFile::new_in(&outdir).unwrap();
        std::fs::write(&far_source_file_path, "some data for far").unwrap();

        // Create a file to include as a blob
        let blob_source_file_path = outdir.path().join("blob_c");
        let blob_contents = "some data for blob";
        std::fs::write(&blob_source_file_path, blob_contents).unwrap();

        // Create a file to include as a blob
        let blob_source_file_path2 = outdir.path().join("blob_d");
        let blob_contents = "some data for blob2";
        std::fs::write(&blob_source_file_path2, blob_contents).unwrap();

        // Create the builder
        let mut builder = PackageBuilder::new("some_pkg_name");
        builder
            .add_file_as_blob("blob_c", blob_source_file_path.as_path().path_to_string().unwrap())
            .unwrap();
        builder
            .add_file_as_blob("blob_d", blob_source_file_path2.as_path().path_to_string().unwrap())
            .unwrap();
        builder
            .add_file_to_far(
                "meta/some/file",
                far_source_file_path.path().path_to_string().unwrap(),
            )
            .unwrap();
        builder
            .add_subpackage(&subpackage_url, sub_manifest.hash(), subpackage_manifest_path)
            .unwrap();

        // Build the package
        let manifest = builder.build(&outdir, &metafar_path).unwrap();

        let archive_outdir = TempDir::new().unwrap();
        let archive_path = archive_outdir.path().join("test.far");
        let archive_file = File::create(archive_path.clone()).unwrap();
        manifest.clone().archive(&outdir, &archive_file).await.unwrap();

        let blobs_outdir = TempDir::new().unwrap().into_path();

        let manifest_2 =
            PackageManifest::from_archive(&archive_path, &blobs_outdir, &manifest_outdir).unwrap();
        assert_eq!(manifest_2.package_path(), manifest.package_path());

        let (_blob1_info, all_blobs_1) = manifest.package_and_subpackage_blobs().unwrap();
        let (_blob2_info, mut all_blobs_2) = manifest_2.package_and_subpackage_blobs().unwrap();

        for (merkle, blob1) in all_blobs_1 {
            let blob2 = all_blobs_2.remove_entry(&merkle).unwrap().1;
            assert_eq!(
                std::fs::read(&blob1.source_path).unwrap(),
                std::fs::read(&blob2.source_path).unwrap(),
            );
        }

        assert!(all_blobs_2.is_empty());
    }

    #[test]
    fn test_write_package_manifest_already_relative() {
        let temp = TempDir::new().unwrap();
        let temp_dir = Utf8Path::from_path(temp.path()).unwrap();

        let data_dir = temp_dir.join("data_source");
        let subpackage_dir = temp_dir.join("subpackage_manifests");
        let manifest_dir = temp_dir.join("manifest_dir");
        let manifest_path = manifest_dir.join("package_manifest.json");

        std::fs::create_dir_all(data_dir).unwrap();
        std::fs::create_dir_all(subpackage_dir).unwrap();
        std::fs::create_dir_all(&manifest_dir).unwrap();

        let manifest = PackageManifest(VersionedPackageManifest::Version1(PackageManifestV1 {
            package: PackageMetadata {
                name: "example".parse().unwrap(),
                version: "0".parse().unwrap(),
            },
            blobs: vec![BlobInfo {
                source_path: "../data_source/p1".into(),
                path: "data/p1".into(),
                merkle: "0000000000000000000000000000000000000000000000000000000000000000"
                    .parse()
                    .unwrap(),
                size: 1,
            }],
            subpackages: vec![SubpackageInfo {
                manifest_path: "../subpackage_manifests/0000000000000000000000000000000000000000000000000000000000000000".into(),
                name: "subpackage0".into(),
                merkle: zeros_hash(),
            }],
            repository: None,
            blob_sources_relative: RelativeTo::File,
        }));

        let result_manifest = manifest.clone().write_with_relative_paths(&manifest_path).unwrap();

        // The manifest should not have been changed in this case.
        assert_eq!(result_manifest, manifest);

        let parsed_manifest: Value =
            serde_json::from_reader(File::open(manifest_path).unwrap()).unwrap();
        let object = parsed_manifest.as_object().unwrap();
        let version = object.get("version").unwrap();

        let blobs_value = object.get("blobs").unwrap();
        let blobs = blobs_value.as_array().unwrap();
        let blob_value = blobs.first().unwrap();
        let blob = blob_value.as_object().unwrap();
        let source_path_value = blob.get("source_path").unwrap();
        let source_path = source_path_value.as_str().unwrap();

        let subpackages_value = object.get("subpackages").unwrap();
        let subpackages = subpackages_value.as_array().unwrap();
        let subpackage_value = subpackages.first().unwrap();
        let subpackage = subpackage_value.as_object().unwrap();
        let subpackage_manifest_path_value = subpackage.get("manifest_path").unwrap();
        let subpackage_manifest_path = subpackage_manifest_path_value.as_str().unwrap();

        assert_eq!(version, "1");
        assert_eq!(source_path, "../data_source/p1");
        assert_eq!(subpackage_manifest_path, "../subpackage_manifests/0000000000000000000000000000000000000000000000000000000000000000");
    }

    #[test]
    fn test_write_package_manifest_making_paths_relative() {
        let temp = TempDir::new().unwrap();
        let temp_dir = Utf8Path::from_path(temp.path()).unwrap();

        let data_dir = temp_dir.join("data_source");
        let subpackage_dir = temp_dir.join("subpackage_manifests");
        let manifest_dir = temp_dir.join("manifest_dir");
        let manifest_path = manifest_dir.join("package_manifest.json");
        let blob_source_path = data_dir.join("p2").to_string();
        let subpackage_manifest_path = subpackage_dir.join(ones_hash_str()).to_string();

        std::fs::create_dir_all(&data_dir).unwrap();
        std::fs::create_dir_all(&subpackage_dir).unwrap();
        std::fs::create_dir_all(&manifest_dir).unwrap();

        let manifest = PackageManifest(VersionedPackageManifest::Version1(PackageManifestV1 {
            package: PackageMetadata {
                name: "example".parse().unwrap(),
                version: "0".parse().unwrap(),
            },
            blobs: vec![BlobInfo {
                source_path: blob_source_path,
                path: "data/p2".into(),
                merkle: zeros_hash(),
                size: 1,
            }],
            subpackages: vec![SubpackageInfo {
                manifest_path: subpackage_manifest_path,
                name: "subpackage1".into(),
                merkle: ones_hash(),
            }],
            repository: None,
            blob_sources_relative: RelativeTo::WorkingDir,
        }));

        let result_manifest = manifest.write_with_relative_paths(&manifest_path).unwrap();
        let blob = result_manifest.blobs().first().unwrap();
        assert_eq!(blob.source_path, "../data_source/p2");
        let subpackage = result_manifest.subpackages().first().unwrap();
        assert_eq!(subpackage.manifest_path, "../subpackage_manifests/1111111111111111111111111111111111111111111111111111111111111111");

        let parsed_manifest: serde_json::Value =
            serde_json::from_reader(File::open(manifest_path).unwrap()).unwrap();

        let object = parsed_manifest.as_object().unwrap();

        let blobs_value = object.get("blobs").unwrap();
        let blobs = blobs_value.as_array().unwrap();
        let blob_value = blobs.first().unwrap();
        let blob = blob_value.as_object().unwrap();
        let source_path_value = blob.get("source_path").unwrap();
        let source_path = source_path_value.as_str().unwrap();

        let subpackages_value = object.get("subpackages").unwrap();
        let subpackages = subpackages_value.as_array().unwrap();
        let subpackage_value = subpackages.first().unwrap();
        let subpackage = subpackage_value.as_object().unwrap();
        let subpackage_manifest_path_value = subpackage.get("manifest_path").unwrap();
        let subpackage_manifest_path = subpackage_manifest_path_value.as_str().unwrap();

        assert_eq!(source_path, "../data_source/p2");
        assert_eq!(subpackage_manifest_path, "../subpackage_manifests/1111111111111111111111111111111111111111111111111111111111111111");
    }
}
