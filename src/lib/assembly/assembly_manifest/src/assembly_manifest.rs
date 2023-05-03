// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::{Context, Result};
use camino::{Utf8Path, Utf8PathBuf};
use fuchsia_pkg::PackageManifest;
use serde::de::{self, Deserializer};
use serde::ser::Serializer;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeSet, HashMap};
use std::fs::File;
use utf8_path::path_relative_from;

/// A manifest containing a list of images produced by the Image Assembler.
///
/// ```
/// use assembly_manifest::AssemblyManifest;
///
/// let manifest = AssemblyManifest {
///     images: vec![
///         Image::ZBI {
///             path: "path/to/fuchsia.zbi",
///             signed: false,
///         },
///         Image::VBMeta("path/to/fuchsia.vbmeta"),
///         Image::FVM("path/to/fvm.blk"),
///         Image::FVMSparse("path/to/fvm.sparse.blk"),
///     ],
/// };
/// println!("{:?}", serde_json::to_value(manifest).unwrap());
/// ```
///
#[derive(Clone, Debug, Default, Eq, Hash, PartialEq)]
pub struct AssemblyManifest {
    /// List of images in the manifest.
    pub images: Vec<Image>,
}

/// Private helper for serializing the AssemblyManifest. An AssemblyManifest cannot be deserialized
/// without going through `try_from_path` in order to require that we use this helper
/// which ensure that the paths get relativized/derelativized properly
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(transparent)]
struct SerializationHelper {
    images: Vec<Image>,
}

/// An item in the AssemblyManifest.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub enum Image {
    /// Base Package.
    BasePackage(Utf8PathBuf),

    /// Zircon Boot Image.
    ZBI {
        /// Path to the ZBI image.
        path: Utf8PathBuf,
        /// Whether the ZBI is signed.
        signed: bool,
    },

    /// Verified Boot Metadata.
    VBMeta(Utf8PathBuf),

    /// BlobFS image.
    BlobFS {
        /// Path to the BlobFS image.
        path: Utf8PathBuf,
        /// Contents metadata.
        contents: BlobfsContents,
    },

    /// Fuchsia Volume Manager.
    FVM(Utf8PathBuf),

    /// Sparse FVM.
    FVMSparse(Utf8PathBuf),

    /// Sparse blobfs-only FVM.
    FVMSparseBlob(Utf8PathBuf),

    /// Fastboot FVM.
    FVMFastboot(Utf8PathBuf),

    /// Fxfs.
    Fxfs {
        /// Path to the Fxfs image.
        path: Utf8PathBuf,
        /// Blob contents metadata.
        contents: BlobfsContents,
    },

    /// Qemu Kernel.
    QemuKernel(Utf8PathBuf),
}

impl Image {
    /// Get the path of the image on the host.
    pub fn source(&self) -> &Utf8Path {
        match self {
            Image::BasePackage(s) => s.as_path(),
            Image::ZBI { path, signed: _ } => path.as_path(),
            Image::VBMeta(s) => s.as_path(),
            Image::BlobFS { path, .. } => path.as_path(),
            Image::FVM(s) => s.as_path(),
            Image::FVMSparse(s) => s.as_path(),
            Image::FVMSparseBlob(s) => s.as_path(),
            Image::FVMFastboot(s) => s.as_path(),
            Image::Fxfs { path, .. } => path.as_path(),
            Image::QemuKernel(s) => s.as_path(),
        }
    }

    /// Set the path of the image on the host.
    pub fn set_source(&mut self, source: impl Into<Utf8PathBuf>) {
        let source = source.into();
        match self {
            Image::BasePackage(s) => *s = source,
            Image::ZBI { path, signed: _ } => *path = source,
            Image::VBMeta(s) => *s = source,
            Image::BlobFS { path, .. } => *path = source,
            Image::FVM(s) => *s = source,
            Image::FVMSparse(s) => *s = source,
            Image::FVMSparseBlob(s) => *s = source,
            Image::FVMFastboot(s) => *s = source,
            Image::Fxfs { path, .. } => *path = source,
            Image::QemuKernel(s) => *s = source,
        }
    }
}

impl AssemblyManifest {
    /// Return a new Assembly Manifest with all the paths relativized to a
    /// target path
    fn relativize(mut self, base_path: impl AsRef<Utf8Path>) -> Result<AssemblyManifest> {
        // Change the images list in-place so fields can be added to the AssemblyManifest without
        // changing this method
        let mut images = Vec::default();
        for image in self.images {
            match image {
                Image::BasePackage(path) => {
                    images.push(Image::BasePackage(path_relative_from(path, &base_path)?))
                }
                Image::VBMeta(path) => {
                    images.push(Image::VBMeta(path_relative_from(path, &base_path)?))
                }
                Image::FVM(path) => images.push(Image::FVM(path_relative_from(path, &base_path)?)),
                Image::FVMSparse(path) => {
                    images.push(Image::FVMSparse(path_relative_from(path, &base_path)?))
                }
                Image::FVMSparseBlob(path) => {
                    images.push(Image::FVMSparseBlob(path_relative_from(path, &base_path)?))
                }
                Image::FVMFastboot(path) => {
                    images.push(Image::FVMFastboot(path_relative_from(path, &base_path)?))
                }
                Image::QemuKernel(path) => {
                    images.push(Image::QemuKernel(path_relative_from(path, &base_path)?))
                }
                Image::ZBI { path, signed } => {
                    images.push(Image::ZBI { path: path_relative_from(path, &base_path)?, signed })
                }
                Image::BlobFS { path, contents } => images.push(Image::BlobFS {
                    path: path_relative_from(path, &base_path)?,
                    contents: contents.relativize(&base_path)?,
                }),
                Image::Fxfs { path, contents } => images.push(Image::Fxfs {
                    path: path_relative_from(path, &base_path)?,
                    contents: contents.relativize(&base_path)?,
                }),
            }
        }
        self.images = images;

        Ok(self)
    }

    /// Return a new Assembly manifest with all the paths made joined to the
    /// provided directory where the manifest is located
    fn derelativize(mut self, manifest_dir: impl AsRef<Utf8Path>) -> Result<AssemblyManifest> {
        // Change the images list in-place so fields can be added to the AssemblyManifest without
        // changing this method
        let mut images = Vec::default();
        for image in self.images {
            match image {
                Image::BasePackage(path) => {
                    images.push(Image::BasePackage(manifest_dir.as_ref().join(path)))
                }
                Image::VBMeta(path) => images.push(Image::VBMeta(manifest_dir.as_ref().join(path))),
                Image::FVM(path) => images.push(Image::FVM(manifest_dir.as_ref().join(path))),
                Image::FVMSparse(path) => {
                    images.push(Image::FVMSparse(manifest_dir.as_ref().join(path)))
                }
                Image::FVMSparseBlob(path) => {
                    images.push(Image::FVMSparseBlob(manifest_dir.as_ref().join(path)))
                }
                Image::FVMFastboot(path) => {
                    images.push(Image::FVMFastboot(manifest_dir.as_ref().join(path)))
                }
                Image::QemuKernel(path) => {
                    images.push(Image::QemuKernel(manifest_dir.as_ref().join(path)))
                }
                Image::ZBI { path, signed } => {
                    images.push(Image::ZBI { path: manifest_dir.as_ref().join(path), signed })
                }
                Image::BlobFS { path, contents } => images.push(Image::BlobFS {
                    path: manifest_dir.as_ref().join(path),
                    contents: contents.derelativize(&manifest_dir)?,
                }),
                Image::Fxfs { path, contents } => images.push(Image::Fxfs {
                    path: manifest_dir.as_ref().join(path),
                    contents: contents.derelativize(&manifest_dir)?,
                }),
            }
        }
        self.images = images;

        Ok(self)
    }

    /// Load an AssemblyManifest from a path on disk, handling path
    /// relativization
    pub fn try_load_from(path: impl AsRef<Utf8Path>) -> Result<Self> {
        let deserialized: SerializationHelper = assembly_util::read_config(path.as_ref())?;
        let manifest = AssemblyManifest { images: deserialized.images };
        manifest.derelativize(path.as_ref().parent().context("Invalid path")?)
    }

    /// Write a product bundle to a directory on disk at `path`.
    /// Make the paths recorded in the file relative to the file's location
    /// so it is portable.
    pub fn write(self, path: impl AsRef<Utf8Path>) -> Result<()> {
        let path = path.as_ref();
        // Relativize the paths in the Assembly Manifest
        let assembly_manifest =
            self.relativize(path.parent().with_context(|| format!("Invalid output path {path}"))?)?;

        // Write the images manifest.
        let images_json = File::create(path).context("Creating assembly manifest")?;
        serde_json::to_writer(
            images_json,
            &SerializationHelper { images: assembly_manifest.images },
        )
        .context("Writing assembly manifest")?;
        Ok(())
    }
}

/// A specific Image type.

#[derive(Debug, Serialize)]
struct ImageSerializeHelper<'a> {
    #[serde(rename = "type")]
    partition_type: &'a str,
    name: &'a str,
    path: &'a Utf8Path,
    #[serde(skip_serializing_if = "Option::is_none")]
    signed: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    contents: Option<ImageContentsSerializeHelper<'a>>,
}

#[derive(Debug, Serialize)]
#[serde(untagged)]
enum ImageContentsSerializeHelper<'a> {
    Blobfs(&'a BlobfsContents),
}

impl Serialize for Image {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let helper = match self {
            Image::BasePackage(path) => ImageSerializeHelper {
                partition_type: "far",
                name: "base-package",
                path,
                signed: None,
                contents: None,
            },
            Image::ZBI { path, signed } => ImageSerializeHelper {
                partition_type: "zbi",
                name: "zircon-a",
                path,
                signed: Some(*signed),
                contents: None,
            },
            Image::VBMeta(path) => ImageSerializeHelper {
                partition_type: "vbmeta",
                name: "zircon-a",
                path,
                signed: None,
                contents: None,
            },
            Image::BlobFS { path, contents } => ImageSerializeHelper {
                partition_type: "blk",
                name: "blob",
                path,
                signed: None,
                contents: Some(ImageContentsSerializeHelper::Blobfs(contents)),
            },
            Image::FVM(path) => ImageSerializeHelper {
                partition_type: "blk",
                name: "storage-full",
                path,
                signed: None,
                contents: None,
            },
            Image::FVMSparse(path) => ImageSerializeHelper {
                partition_type: "blk",
                name: "storage-sparse",
                path,
                signed: None,
                contents: None,
            },
            Image::FVMSparseBlob(path) => ImageSerializeHelper {
                partition_type: "blk",
                name: "storage-sparse-blob",
                path,
                signed: None,
                contents: None,
            },
            Image::FVMFastboot(path) => ImageSerializeHelper {
                partition_type: "blk",
                name: "fvm.fastboot",
                path,
                signed: None,
                contents: None,
            },
            Image::Fxfs { path, contents } => ImageSerializeHelper {
                partition_type: "fxfs-blk",
                name: "storage-full",
                path,
                signed: None,
                contents: Some(ImageContentsSerializeHelper::Blobfs(contents)),
            },
            Image::QemuKernel(path) => ImageSerializeHelper {
                partition_type: "kernel",
                name: "qemu-kernel",
                path,
                signed: None,
                contents: None,
            },
        };
        helper.serialize(serializer)
    }
}

/// Detailed metadata on the contents of a particular image output.
#[derive(Clone, Debug, Default, Deserialize, Eq, Hash, PartialEq, Serialize)]
pub struct BlobfsContents {
    /// Information about packages included in the image.
    pub packages: PackagesMetadata,
    /// Maximum total size of all the blobs stored in this image.
    pub maximum_contents_size: Option<u64>,
}

/// Contains unique set of blobs.
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize)]
#[serde(transparent)]
pub struct PackageSetBlobInfo(pub BTreeSet<PackageBlob>);

impl BlobfsContents {
    /// Add base package info into BlobfsContents
    pub fn add_base_package(
        &mut self,
        path: impl AsRef<Utf8Path>,
        merkle_size_map: &HashMap<String, u64>,
    ) -> anyhow::Result<()> {
        Self::add_package(&mut self.packages.base, path, merkle_size_map)?;
        Ok(())
    }

    /// Add cache package info into BlobfsContents
    pub fn add_cache_package(
        &mut self,
        path: impl AsRef<Utf8Path>,
        merkle_size_map: &HashMap<String, u64>,
    ) -> anyhow::Result<()> {
        Self::add_package(&mut self.packages.cache, path, merkle_size_map)?;
        Ok(())
    }

    fn add_package_blobs(
        package_manifest: PackageManifest,
        package_blobs: &mut Vec<PackageBlob>,
        merkle_size_map: &HashMap<String, u64>,
    ) -> anyhow::Result<()> {
        let (blobs, subpackages) = package_manifest.into_blobs_and_subpackages();
        for blob in blobs {
            package_blobs.push(PackageBlob {
                merkle: blob.merkle.to_string(),
                path: blob.path.to_string(),
                used_space_in_blobfs: *merkle_size_map.get(&blob.merkle.to_string()).with_context(
                    || format!("Blob merkle not found. {} {}", blob.path, blob.merkle),
                )?,
            });
        }
        for subpackage in subpackages {
            Self::add_package_blobs(
                PackageManifest::try_load_from(subpackage.manifest_path)?,
                package_blobs,
                merkle_size_map,
            )?;
        }
        Ok(())
    }

    fn add_package(
        package_set: &mut PackageSetMetadata,
        path: impl AsRef<Utf8Path>,
        merkle_size_map: &HashMap<String, u64>,
    ) -> anyhow::Result<()> {
        let manifest = path.as_ref().to_owned();
        let package_manifest = PackageManifest::try_load_from(&manifest)?;
        let name = package_manifest.name().to_string();
        let mut package_blobs: Vec<PackageBlob> = vec![];
        Self::add_package_blobs(package_manifest, &mut package_blobs, merkle_size_map)
            .with_context(|| format!("adding package: {manifest}"))?;
        package_blobs.sort();
        package_set.0.push(PackageMetadata { name, manifest, blobs: package_blobs });
        Ok(())
    }

    /// Relativize all manifest locations in the BlobFsContents to the output file's location
    pub fn relativize(mut self, base_path: impl AsRef<Utf8Path>) -> Result<BlobfsContents> {
        // Modify in-place so we can add fields to the struct without updating this code
        for package in self.packages.base.0.iter_mut() {
            package.manifest = path_relative_from(&package.manifest, &base_path)?
        }
        for package in self.packages.cache.0.iter_mut() {
            package.manifest = path_relative_from(&package.manifest, &base_path)?
        }
        Ok(self)
    }

    /// Join all manifest paths in the BlobFsContents to the location of the file it is serialized in
    fn derelativize(mut self, manifest_dir: impl AsRef<Utf8Path>) -> Result<BlobfsContents> {
        // Modify in-place so we can add fields without updating this code
        for package in self.packages.base.0.iter_mut() {
            package.manifest = manifest_dir.as_ref().join(&package.manifest)
        }
        for package in self.packages.cache.0.iter_mut() {
            package.manifest = manifest_dir.as_ref().join(&package.manifest)
        }
        Ok(self)
    }
}

/// Metadata on packages included in a given image.
#[derive(Clone, Debug, Default, Deserialize, Eq, Hash, PartialEq, Serialize)]
pub struct PackagesMetadata {
    /// Paths to package manifests for the base package set.
    pub base: PackageSetMetadata,
    /// Paths to package manifests for the cache package set.
    pub cache: PackageSetMetadata,
}

/// Metadata for a certain package set (e.g. base or cache).
#[derive(Clone, Debug, Default, Deserialize, Eq, Hash, PartialEq, Serialize)]
#[serde(transparent)]
pub struct PackageSetMetadata(pub Vec<PackageMetadata>);

/// Metadata on a single package included in a given image.
#[derive(Clone, Debug, Default, Deserialize, Eq, Hash, PartialEq, Serialize)]
pub struct PackageMetadata {
    /// The package's name.
    pub name: String,
    /// Path to the package's manifest.
    pub manifest: Utf8PathBuf,
    /// List of blobs in this package.
    pub blobs: Vec<PackageBlob>,
}

#[derive(Clone, Debug, Ord, PartialOrd, PartialEq, Eq, Hash, Deserialize, Serialize)]
pub struct PackageBlob {
    // Merkle hash of this blob
    pub merkle: String,
    // Path of blob in package
    pub path: String,
    // Space used by this blob in blobfs
    pub used_space_in_blobfs: u64,
}

#[derive(Debug, Deserialize)]
struct ImageDeserializeHelper {
    #[serde(rename = "type")]
    partition_type: String,
    name: String,
    path: Utf8PathBuf,
    signed: Option<bool>,
    contents: Option<ImageContentsDeserializeHelper>,
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum ImageContentsDeserializeHelper {
    Blobfs(BlobfsContents),
}

impl<'de> Deserialize<'de> for Image {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let helper = ImageDeserializeHelper::deserialize(deserializer)?;
        match (&helper.partition_type[..], &helper.name[..], &helper.signed) {
            ("far", "base-package", None) => Ok(Image::BasePackage(helper.path)),
            ("zbi", "zircon-a", Some(signed)) => {
                Ok(Image::ZBI { path: helper.path, signed: *signed })
            }
            ("vbmeta", "zircon-a", None) => Ok(Image::VBMeta(helper.path)),
            ("blk", "blob", None) => {
                if let Some(contents) = helper.contents {
                    let ImageContentsDeserializeHelper::Blobfs(contents) = contents;
                    Ok(Image::BlobFS { path: helper.path, contents })
                } else {
                    Err(de::Error::missing_field("contents"))
                }
            }
            ("blk", "storage-full", None) => Ok(Image::FVM(helper.path)),
            ("fxfs-blk", "storage-full", None) => {
                if let Some(contents) = helper.contents {
                    let ImageContentsDeserializeHelper::Blobfs(contents) = contents;
                    Ok(Image::Fxfs { path: helper.path, contents })
                } else {
                    Err(de::Error::missing_field("contents"))
                }
            }
            ("blk", "storage-sparse", None) => Ok(Image::FVMSparse(helper.path)),
            ("blk", "storage-sparse-blob", None) => Ok(Image::FVMSparseBlob(helper.path)),
            ("blk", "fvm.fastboot", None) => Ok(Image::FVMFastboot(helper.path)),
            ("kernel", "qemu-kernel", None) => Ok(Image::QemuKernel(helper.path)),
            (partition_type, name, _) => Err(de::Error::unknown_variant(
                &format!("({partition_type}, {name})"),
                &[
                    "(far, base-package)",
                    "(zbi, zircon-a)",
                    "(vbmeta, zircon-a)",
                    "(blk, blob)",
                    "(blk, storage-full)",
                    "(blk, storage-sparse)",
                    "(blk, storage-sparse-blob)",
                    "(blk, fvm.fastboot)",
                    "(kernel, qemu-kernel)",
                ],
            )),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{json, Value};
    use std::io::Write;
    use tempfile::{tempdir, NamedTempFile};

    #[test]
    fn image_source() {
        let mut image = Image::FVM("path/to/fvm.blk".into());
        assert_eq!(image.source(), &Utf8PathBuf::from("path/to/fvm.blk"));
        image.set_source("path/to/fvm2.blk");
        assert_eq!(image.source(), &Utf8PathBuf::from("path/to/fvm2.blk"));
    }

    #[test]
    fn relativize() {
        let manifest = AssemblyManifest {
            images: vec![
                Image::BasePackage("base.far".into()),
                Image::ZBI { path: "fuchsia.zbi".into(), signed: true },
                Image::VBMeta("fuchsia.vbmeta".into()),
                Image::BlobFS { path: "blob.blk".into(), contents: Default::default() },
                Image::FVM("fvm.blk".into()),
                Image::FVMSparse("fvm.sparse.blk".into()),
                Image::FVMSparseBlob("fvm.blob.sparse.blk".into()),
                Image::FVMFastboot("fvm.fastboot.blk".into()),
                Image::QemuKernel("qemu/kernel".into()),
            ],
        };

        let relativized_manifest =
            generate_test_manifest().relativize(Utf8PathBuf::from("path/to")).unwrap();
        assert_eq!(relativized_manifest, manifest);
    }

    #[test]
    fn relativize_fxfs() {
        let manifest = AssemblyManifest {
            images: vec![
                Image::BasePackage("base.far".into()),
                Image::ZBI { path: "fuchsia.zbi".into(), signed: true },
                Image::VBMeta("fuchsia.vbmeta".into()),
                Image::Fxfs { path: "fxfs.blk".into(), contents: Default::default() },
                Image::QemuKernel("qemu/kernel".into()),
            ],
        };

        let relativized_manifest =
            generate_test_manifest_fxfs().relativize(Utf8PathBuf::from("path/to")).unwrap();
        assert_eq!(relativized_manifest, manifest);
    }

    #[test]
    fn derelativize() {
        let manifest = AssemblyManifest {
            images: vec![
                Image::BasePackage("base.far".into()),
                Image::ZBI { path: "fuchsia.zbi".into(), signed: true },
                Image::VBMeta("fuchsia.vbmeta".into()),
                Image::BlobFS { path: "blob.blk".into(), contents: Default::default() },
                Image::FVM("fvm.blk".into()),
                Image::FVMSparse("fvm.sparse.blk".into()),
                Image::FVMSparseBlob("fvm.blob.sparse.blk".into()),
                Image::FVMFastboot("fvm.fastboot.blk".into()),
                Image::QemuKernel("qemu/kernel".into()),
            ],
        };

        let derelativized_manifest = manifest.derelativize(Utf8PathBuf::from("path/to")).unwrap();
        let manifest = generate_test_manifest();
        assert_eq!(manifest, derelativized_manifest);
    }

    #[test]
    fn derelativize_fxfs() {
        let manifest = AssemblyManifest {
            images: vec![
                Image::BasePackage("base.far".into()),
                Image::ZBI { path: "fuchsia.zbi".into(), signed: true },
                Image::VBMeta("fuchsia.vbmeta".into()),
                Image::Fxfs { path: "fxfs.blk".into(), contents: Default::default() },
                Image::QemuKernel("qemu/kernel".into()),
            ],
        };

        let derelativized_manifest = manifest.derelativize(Utf8PathBuf::from("path/to")).unwrap();
        let manifest = generate_test_manifest_fxfs();
        assert_eq!(manifest, derelativized_manifest);
    }

    #[test]
    fn serialize() {
        let manifest = AssemblyManifest {
            images: vec![
                Image::BasePackage("path/to/base.far".into()),
                Image::ZBI { path: "path/to/fuchsia.zbi".into(), signed: true },
                Image::VBMeta("path/to/fuchsia.vbmeta".into()),
                Image::BlobFS { path: "path/to/blob.blk".into(), contents: Default::default() },
                Image::FVM("path/to/fvm.blk".into()),
                Image::FVMSparse("path/to/fvm.sparse.blk".into()),
                Image::FVMSparseBlob("path/to/fvm.blob.sparse.blk".into()),
                Image::FVMFastboot("path/to/fvm.fastboot.blk".into()),
                Image::QemuKernel("path/to/qemu/kernel".into()),
            ],
        };

        assert_eq!(
            generate_test_value(),
            serde_json::to_value(SerializationHelper { images: manifest.images }).unwrap()
        );
    }

    #[test]
    fn serialize_fxfs() {
        let manifest = AssemblyManifest {
            images: vec![
                Image::BasePackage("path/to/base.far".into()),
                Image::ZBI { path: "path/to/fuchsia.zbi".into(), signed: true },
                Image::VBMeta("path/to/fuchsia.vbmeta".into()),
                Image::Fxfs { path: "path/to/fxfs.blk".into(), contents: Default::default() },
                Image::QemuKernel("path/to/qemu/kernel".into()),
            ],
        };

        assert_eq!(
            generate_test_value_fxfs(),
            serde_json::to_value(SerializationHelper { images: manifest.images }).unwrap()
        );
    }

    #[test]
    fn serialize_unsigned_zbi() {
        let manifest = AssemblyManifest {
            images: vec![Image::ZBI { path: "path/to/fuchsia.zbi".into(), signed: false }],
        };

        let value = json!([
            {
                "type": "zbi",
                "name": "zircon-a",
                "path": "path/to/fuchsia.zbi",
                "signed": false,
            }
        ]);
        assert_eq!(
            value,
            serde_json::to_value(SerializationHelper { images: manifest.images }).unwrap()
        );
    }

    #[test]
    fn deserialize_zbi_missing_signed() {
        let invalid = json!([
            {
                "type": "zbi",
                "name": "zircon-a",
                "path": "path/to/fuchsia.zbi",
            }
        ]);
        let result: Result<SerializationHelper, _> = serde_json::from_value(invalid);
        assert!(result.unwrap_err().is_data());
    }

    #[test]
    fn deserialize() {
        let manifest: AssemblyManifest = generate_test_manifest();
        assert_eq!(manifest.images.len(), 9);

        for image in &manifest.images {
            let (expected, actual) = match image {
                Image::BasePackage(path) => ("path/to/base.far", path),
                Image::ZBI { path, signed } => {
                    assert!(signed);
                    ("path/to/fuchsia.zbi", path)
                }
                Image::VBMeta(path) => ("path/to/fuchsia.vbmeta", path),
                Image::BlobFS { path, contents } => {
                    assert_eq!(contents, &BlobfsContents::default());
                    ("path/to/blob.blk", path)
                }
                Image::FVM(path) => ("path/to/fvm.blk", path),
                Image::FVMSparse(path) => ("path/to/fvm.sparse.blk", path),
                Image::FVMSparseBlob(path) => ("path/to/fvm.blob.sparse.blk", path),
                Image::FVMFastboot(path) => ("path/to/fvm.fastboot.blk", path),
                Image::QemuKernel(path) => ("path/to/qemu/kernel", path),
                _ => panic!("Unexpected item {:?}", image),
            };
            assert_eq!(&Utf8PathBuf::from(expected), actual);
        }
    }

    #[test]
    fn deserialize_fxfs() {
        let manifest: AssemblyManifest = generate_test_manifest_fxfs();
        assert_eq!(manifest.images.len(), 5);

        for image in &manifest.images {
            let (expected, actual) = match image {
                Image::BasePackage(path) => ("path/to/base.far", path),
                Image::ZBI { path, signed } => {
                    assert!(signed);
                    ("path/to/fuchsia.zbi", path)
                }
                Image::VBMeta(path) => ("path/to/fuchsia.vbmeta", path),
                Image::Fxfs { path, contents } => {
                    assert_eq!(contents, &BlobfsContents::default());
                    ("path/to/fxfs.blk", path)
                }
                Image::QemuKernel(path) => ("path/to/qemu/kernel", path),
                _ => panic!("Unexpected item {:?}", image),
            };
            assert_eq!(&Utf8PathBuf::from(expected), actual);
        }
    }

    #[test]
    fn deserialize_invalid() {
        let invalid = json!([
            {
                "type": "far-invalid",
                "name": "base-package",
                "path": "path/to/base.far",
            },
        ]);
        let result: Result<SerializationHelper, _> = serde_json::from_value(invalid);
        assert!(result.unwrap_err().is_data());
    }

    #[test]
    fn test_blobfs_contents_add_base_or_cache_package() -> anyhow::Result<()> {
        let content = generate_test_package_manifest_content();

        let mut package_manifest_temp_file = NamedTempFile::new()?;
        let dir = tempdir().unwrap();
        let root = Utf8Path::from_path(dir.path()).unwrap();
        write!(package_manifest_temp_file, "{content}")?;
        let path = package_manifest_temp_file.into_temp_path();
        path.persist(dir.path().join("package_manifest_temp_file.json"))?;

        let mut contents = BlobfsContents::default();
        let mut merkle_size_map: HashMap<String, u64> = HashMap::new();
        merkle_size_map.insert(
            "7ddff816740d5803358dd4478d8437585e8d5c984b4361817d891807a16ff581".to_string(),
            10,
        );
        merkle_size_map.insert(
            "8cb3466c6e66592c8decaeaa3e399652fbe71dad5c3df1a5e919743a33815567".to_string(),
            20,
        );
        merkle_size_map.insert(
            "eabdb84d26416c1821fd8972e0d835eedaf7468e5a9ebe01e5944462411aec70".to_string(),
            30,
        );
        contents
            .add_base_package(root.join("package_manifest_temp_file.json"), &merkle_size_map)?;
        contents
            .add_cache_package(root.join("package_manifest_temp_file.json"), &merkle_size_map)?;
        let actual_package_blobs_base = &(contents.packages.base.0[0].blobs);
        let actual_package_blobs_cache = &(contents.packages.cache.0[0].blobs);

        let package_blob1 = PackageBlob {
            merkle: "7ddff816740d5803358dd4478d8437585e8d5c984b4361817d891807a16ff581".to_string(),
            path: "bin/def".to_string(),
            used_space_in_blobfs: 10,
        };

        let package_blob2 = PackageBlob {
            merkle: "8cb3466c6e66592c8decaeaa3e399652fbe71dad5c3df1a5e919743a33815567".to_string(),
            path: "lib/ghi".to_string(),
            used_space_in_blobfs: 20,
        };

        let package_blob3 = PackageBlob {
            merkle: "eabdb84d26416c1821fd8972e0d835eedaf7468e5a9ebe01e5944462411aec70".to_string(),
            path: "abc/".to_string(),
            used_space_in_blobfs: 30,
        };

        let expected_blobs = vec![package_blob1, package_blob2, package_blob3];
        // Verify if all 3 blobs are available and also if they are sorted.
        assert_eq!(actual_package_blobs_base, &expected_blobs);
        assert_eq!(actual_package_blobs_cache, &expected_blobs);

        Ok(())
    }

    fn generate_test_package_manifest_content() -> String {
        let content = r#"{
            "package": {
                "name": "test_package",
                "version": "0"
            },
            "blobs": [
                {
                    "path": "abc/",
                    "merkle": "eabdb84d26416c1821fd8972e0d835eedaf7468e5a9ebe01e5944462411aec70",
                    "size": 2048,
                    "source_path": "../../blobs/eabdb84d26416c1821fd8972e0d835eedaf7468e5a9ebe01e5944462411aec78"
                },
                {
                    "path": "bin/def",
                    "merkle": "7ddff816740d5803358dd4478d8437585e8d5c984b4361817d891807a16ff581",
                    "size": 188416,
                    "source_path": "../../blobs/7ddff816740d5803358dd4478d8437585e8d5c984b4361817d891807a16ff581"
                },
                {
                    "path": "lib/ghi",
                    "merkle": "8cb3466c6e66592c8decaeaa3e399652fbe71dad5c3df1a5e919743a33815567",
                    "size": 692224,
                    "source_path": "../../blobs/8cb3466c6e66592c8decaeaa3e399652fbe71dad5c3df1a5e919743a33815567"
                }
            ],
            "version": "1",
            "blob_sources_relative": "file",
            "repository": "fuchsia.com"
        }
        "#;
        content.to_string()
    }

    fn generate_test_manifest() -> AssemblyManifest {
        AssemblyManifest {
            images: serde_json::from_value::<SerializationHelper>(generate_test_value())
                .unwrap()
                .images,
        }
    }

    fn generate_test_manifest_fxfs() -> AssemblyManifest {
        AssemblyManifest {
            images: serde_json::from_value::<SerializationHelper>(generate_test_value_fxfs())
                .unwrap()
                .images,
        }
    }

    fn generate_test_value() -> Value {
        json!([
            {
                "type": "far",
                "name": "base-package",
                "path": "path/to/base.far",
            },
            {
                "type": "zbi",
                "name": "zircon-a",
                "path": "path/to/fuchsia.zbi",
                "signed": true,
            },
            {
                "type": "vbmeta",
                "name": "zircon-a",
                "path": "path/to/fuchsia.vbmeta",
            },
            {
                "type": "blk",
                "name": "blob",
                "path": "path/to/blob.blk",
                "contents": {
                    "packages": {
                        "base": [],
                        "cache": [],
                    },
                    "maximum_contents_size": None::<u64>
                },
            },
            {
                "type": "blk",
                "name": "storage-full",
                "path": "path/to/fvm.blk",
            },
            {
                "type": "blk",
                "name": "storage-sparse",
                "path": "path/to/fvm.sparse.blk",
            },
            {
                "type": "blk",
                "name": "storage-sparse-blob",
                "path": "path/to/fvm.blob.sparse.blk",
            },
            {
                "type": "blk",
                "name": "fvm.fastboot",
                "path": "path/to/fvm.fastboot.blk",
            },
            {
                "type": "kernel",
                "name": "qemu-kernel",
                "path": "path/to/qemu/kernel",
            },
        ])
    }

    fn generate_test_value_fxfs() -> Value {
        json!([
            {
                "type": "far",
                "name": "base-package",
                "path": "path/to/base.far",
            },
            {
                "type": "zbi",
                "name": "zircon-a",
                "path": "path/to/fuchsia.zbi",
                "signed": true,
            },
            {
                "type": "vbmeta",
                "name": "zircon-a",
                "path": "path/to/fuchsia.vbmeta",
            },
            {
                "type": "fxfs-blk",
                "name": "storage-full",
                "path": "path/to/fxfs.blk",
                "contents": {
                    "packages": {
                        "base": [],
                        "cache": [],
                    },
                    "maximum_contents_size": None::<u64>
                },
            },
            {
                "type": "kernel",
                "name": "qemu-kernel",
                "path": "path/to/qemu/kernel",
            },
        ])
    }
}
