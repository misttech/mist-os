// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#[cfg(test)]
use crate::MetaSubpackages;
use crate::{MetaContents, MetaContentsError, MetaPackage, PackageManifest};
use anyhow::Result;
use fuchsia_merkle::Hash;
use fuchsia_url::{PackageName, RelativePackageUrl};
use std::collections::{BTreeMap, HashMap};
#[cfg(test)]
use std::io::{Read, Seek};
use std::path::PathBuf;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Package {
    meta_contents: MetaContents,
    meta_package: MetaPackage,
    subpackages: Vec<SubpackageEntry>,
    blobs: BTreeMap<String, BlobEntry>,
}

impl Package {
    /// Get the meta_contents.
    #[cfg(test)]
    pub fn meta_contents(&self) -> MetaContents {
        self.meta_contents.clone()
    }

    /// Get the meta_package.
    pub fn meta_package(&self) -> MetaPackage {
        self.meta_package.clone()
    }

    /// Get the meta_subpackages.
    #[cfg(test)]
    pub fn meta_subpackages(&self) -> MetaSubpackages {
        MetaSubpackages::from_iter(self.subpackages.iter().map(
            |SubpackageEntry { name, merkle, package_manifest_path: _ }| (name.clone(), *merkle),
        ))
    }

    /// Get the subpackages (including package_manifest.json paths, if known).
    pub fn subpackages(&self) -> Vec<SubpackageEntry> {
        self.subpackages.clone()
    }

    /// Get the blobs.
    pub fn blobs(&self) -> BTreeMap<String, BlobEntry> {
        self.blobs.clone()
    }

    /// Create a new `PackageBuilder` from a package name.
    pub(crate) fn builder(name: PackageName) -> Builder {
        Builder::new(name)
    }

    /// Generate a Package from a meta.far file.
    #[cfg(test)]
    pub fn from_meta_far<R: Read + Seek>(
        mut meta_far: R,
        blobs: BTreeMap<String, BlobEntry>,
        subpackages: Vec<SubpackageEntry>,
    ) -> Result<Self> {
        let mut meta_far = fuchsia_archive::Utf8Reader::new(&mut meta_far)?;
        let meta_contents =
            MetaContents::deserialize(meta_far.read_file(MetaContents::PATH)?.as_slice())?;
        let meta_package =
            MetaPackage::deserialize(meta_far.read_file(MetaPackage::PATH)?.as_slice())?;
        Ok(Package { meta_contents, meta_package, subpackages, blobs })
    }
}

pub(crate) struct Builder {
    contents: HashMap<String, Hash>,
    meta_package: MetaPackage,
    subpackages: Vec<SubpackageEntry>,
    blobs: BTreeMap<String, BlobEntry>,
}

impl Builder {
    pub(crate) fn new(name: PackageName) -> Self {
        Self {
            contents: HashMap::new(),
            meta_package: MetaPackage::from_name_and_variant_zero(name),
            subpackages: Vec::new(),
            blobs: BTreeMap::new(),
        }
    }

    pub(crate) fn add_entry(
        &mut self,
        blob_path: String,
        hash: Hash,
        source_path: PathBuf,
        size: u64,
    ) {
        if blob_path != PackageManifest::META_FAR_BLOB_PATH {
            self.contents.insert(blob_path.clone(), hash);
        }
        self.blobs.insert(blob_path, BlobEntry { source_path, size, hash });
    }

    pub(crate) fn add_subpackage(
        &mut self,
        name: RelativePackageUrl,
        merkle: Hash,
        package_manifest_path: PathBuf,
    ) {
        self.subpackages.push(SubpackageEntry { name, merkle, package_manifest_path });
    }

    pub(crate) fn build(self) -> Result<Package, MetaContentsError> {
        Ok(Package {
            meta_contents: MetaContents::from_map(self.contents)?,
            meta_package: self.meta_package,
            subpackages: self.subpackages,
            blobs: self.blobs,
        })
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct BlobEntry {
    source_path: PathBuf,
    hash: Hash,
    size: u64,
}

impl BlobEntry {
    pub fn source_path(&self) -> PathBuf {
        self.source_path.clone()
    }

    pub fn size(&self) -> u64 {
        self.size
    }

    pub fn hash(&self) -> Hash {
        self.hash
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct SubpackageEntry {
    pub name: RelativePackageUrl,
    pub merkle: Hash,
    pub package_manifest_path: PathBuf,
}

#[cfg(test)]
mod test_package {
    use super::*;
    use crate::build::{build_with_file_system, FileSystem};
    use crate::PackageBuildManifest;

    use maplit::{btreemap, hashmap};
    use std::fs::File;
    use std::io;
    use std::str::FromStr;
    use tempfile::tempdir;

    fn zeros_hash() -> Hash {
        "0000000000000000000000000000000000000000000000000000000000000000".parse().unwrap()
    }

    fn ones_hash() -> Hash {
        "1111111111111111111111111111111111111111111111111111111111111111".parse().unwrap()
    }

    #[test]
    fn test_create_package() {
        let meta_package = MetaPackage::from_name_and_variant_zero("package-name".parse().unwrap());

        let subpackages = vec![
            SubpackageEntry {
                name: RelativePackageUrl::parse("a_0_subpackage").unwrap(),
                merkle: zeros_hash(),
                package_manifest_path: PathBuf::from("some/path/to/package_manifest.json"),
            },
            SubpackageEntry {
                name: RelativePackageUrl::parse("other-1-subpackage").unwrap(),
                merkle: ones_hash(),
                package_manifest_path: PathBuf::from("another/path/to/package_manifest.json"),
            },
        ];

        let meta_subpackages = MetaSubpackages::from_iter(hashmap! {
            subpackages[0].name.clone() => subpackages[0].merkle,
            subpackages[1].name.clone() => subpackages[1].merkle,
        });

        let map = hashmap! {
        "bin/my_prog".to_string() =>
            Hash::from_str(
             "0000000000000000000000000000000000000000000000000000000000000000")
            .unwrap(),
        "lib/mylib.so".to_string() =>
            Hash::from_str(
               "1111111111111111111111111111111111111111111111111111111111111111")
            .unwrap(),
            };
        let meta_contents = MetaContents::from_map(map).unwrap();
        let blob_entry = BlobEntry {
            source_path: PathBuf::from("src/bin/my_prog"),
            size: 1,
            hash: Hash::from_str(
                "0000000000000000000000000000000000000000000000000000000000000000",
            )
            .unwrap(),
        };
        let blobs = btreemap! {
            "bin/my_prog".to_string() => blob_entry.clone(),
            "bin/my_prog2".to_string() => blob_entry,
        };
        let package = Package {
            meta_contents: meta_contents.clone(),
            meta_package: meta_package.clone(),
            subpackages: subpackages.clone(),
            blobs: blobs.clone(),
        };
        assert_eq!(meta_package, package.meta_package());
        assert_eq!(meta_subpackages, package.meta_subpackages());
        assert_eq!(subpackages, package.subpackages());
        assert_eq!(meta_contents, package.meta_contents());
        assert_eq!(blobs, package.blobs());
    }

    struct FakeFileSystem {
        content_map: HashMap<String, Vec<u8>>,
    }

    impl<'a> FileSystem<'a> for FakeFileSystem {
        type File = &'a [u8];
        fn open(&'a self, path: &str) -> Result<Self::File, io::Error> {
            Ok(self.content_map.get(path).unwrap().as_slice())
        }
        fn len(&self, path: &str) -> Result<u64, io::Error> {
            Ok(self.content_map.get(path).unwrap().len() as u64)
        }
        fn read(&self, path: &str) -> Result<Vec<u8>, io::Error> {
            Ok(self.content_map.get(path).unwrap().clone())
        }
    }

    #[test]
    fn test_from_meta_far_valid_meta_far() {
        let outdir = tempdir().unwrap();
        let meta_far_path = outdir.path().join("base.far");

        let creation_manifest = PackageBuildManifest::from_external_and_far_contents(
            btreemap! {
                "lib/mylib.so".to_string() => "host/mylib.so".to_string()
            },
            btreemap! {
                "meta/my_component.cml".to_string() => "host/my_component.cml".to_string(),
                "meta/package".to_string() => "host/meta/package".to_string()
            },
        )
        .unwrap();
        let component_manifest_contents = "my_component.cml contents";
        let mut meta_package_json_bytes = vec![];
        let meta_package =
            MetaPackage::from_name_and_variant_zero("my-package-name".parse().unwrap());
        meta_package.serialize(&mut meta_package_json_bytes).unwrap();

        let subpackages = vec![
            SubpackageEntry {
                name: RelativePackageUrl::parse("a_0_subpackage").unwrap(),
                merkle: zeros_hash(),
                package_manifest_path: PathBuf::from("some/path/to/package_manifest.json"),
            },
            SubpackageEntry {
                name: RelativePackageUrl::parse("other-1-subpackage").unwrap(),
                merkle: ones_hash(),
                package_manifest_path: PathBuf::from("another/path/to/package_manifest.json"),
            },
        ];

        let meta_subpackages = MetaSubpackages::from_iter(hashmap! {
            subpackages[0].name.clone() => subpackages[0].merkle,
            subpackages[1].name.clone() => subpackages[1].merkle,
        });

        let mut meta_subpackages_json_bytes = vec![];
        meta_subpackages.serialize(&mut meta_subpackages_json_bytes).unwrap();

        let file_system = FakeFileSystem {
            content_map: hashmap! {
                "host/mylib.so".to_string() => "mylib.so contents".as_bytes().to_vec(),
                "host/my_component.cml".to_string() => component_manifest_contents.as_bytes().to_vec(),
                format!("host/{}", MetaPackage::PATH) => meta_package_json_bytes,
                format!("host/{}", MetaSubpackages::PATH) => meta_subpackages_json_bytes,
            },
        };

        build_with_file_system(
            &creation_manifest,
            &meta_far_path,
            "my-package-name",
            subpackages.clone(),
            None,
            &file_system,
        )
        .unwrap();

        let blob_entry = BlobEntry {
            source_path: PathBuf::from("src/bin/my_prog"),
            size: 1,
            hash: Hash::from_str(
                "0000000000000000000000000000000000000000000000000000000000000000",
            )
            .unwrap(),
        };
        let blobs = btreemap! {
            "bin/my_prog".to_string() => blob_entry.clone(),
            "bin/my_prog2".to_string() => blob_entry,
        };
        let package =
            Package::from_meta_far(File::open(&meta_far_path).unwrap(), blobs.clone(), subpackages)
                .unwrap();
        assert_eq!(blobs, package.blobs());
        assert_eq!(
            &"my-package-name".parse::<PackageName>().unwrap(),
            package.meta_package().name()
        );
        assert_eq!(
            package.meta_subpackages().subpackages().get(&"a_0_subpackage".try_into().unwrap()),
            Some(&zeros_hash())
        );
        assert_eq!(
            package.meta_subpackages().subpackages().get(&"other-1-subpackage".try_into().unwrap()),
            Some(&ones_hash())
        );
    }

    #[test]
    fn test_from_meta_far_empty_meta_far() {
        let dir = tempdir().unwrap();
        let file_path = dir.path().join("meta.far");
        File::create(&file_path).unwrap();
        let file = File::open(&file_path).unwrap();
        let blob_entry = BlobEntry {
            source_path: PathBuf::from("src/bin/my_prog"),
            size: 1,
            hash: Hash::from_str(
                "0000000000000000000000000000000000000000000000000000000000000000",
            )
            .unwrap(),
        };
        let blobs = btreemap! {
            "bin/my_prog".to_string() => blob_entry.clone(),
            "bin/my_prog2".to_string() => blob_entry,
        };
        let package = Package::from_meta_far(file, blobs, Vec::new());
        assert!(package.is_err());
    }
}
