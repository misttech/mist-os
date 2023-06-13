// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    anyhow::{Context, Result},
    assembly_blobfs::BlobManifest,
    camino::{Utf8Path, Utf8PathBuf},
    fuchsia_pkg::PackageManifest,
    serde::Deserialize,
    std::fs::File,
};

/// Builder for Fxfs images which can be preinstalled with a set of initial packages.
///
/// Example usage:
///
/// ```
/// let builder = FxfsBuilder::new();
/// builder.add_package("path/to/package_manifest.json")?;
/// builder.add_file("path/to/file.txt")?;
/// builder.build(gendir, "fxfs.blk").await?;
/// ```
///
pub struct FxfsBuilder {
    manifest: BlobManifest,
    size_bytes: u64,
}

impl FxfsBuilder {
    /// Construct a new FxfsBuilder.
    pub fn new() -> Self {
        FxfsBuilder { manifest: BlobManifest::default(), size_bytes: 0 }
    }

    /// Sets the target image size.
    pub fn set_size(&mut self, size_bytes: u64) {
        self.size_bytes += size_bytes
    }

    /// Add a package to fxfs by inserting every blob mentioned in the `package_manifest` on the
    /// host.
    pub fn add_package(&mut self, package_manifest: PackageManifest) -> Result<()> {
        self.manifest.add_package(package_manifest)
    }

    /// Add a package to fxfs by inserting every blob mentioned in the `package_manifest_path` on
    /// the host.
    pub fn add_package_from_path(
        &mut self,
        package_manifest_path: impl AsRef<Utf8Path>,
    ) -> Result<()> {
        let package_manifest_path = package_manifest_path.as_ref();
        let manifest = PackageManifest::try_load_from(package_manifest_path)
            .with_context(|| format!("Adding package: {}", package_manifest_path))?;
        self.add_package(manifest)
    }

    /// Build fxfs, and write it to `output`, while placing intermediate files in `gendir`.
    /// Returns a path where the blobs JSON was written to.
    pub async fn build(
        &self,
        gendir: impl AsRef<Utf8Path>,
        output: impl AsRef<Utf8Path>,
    ) -> Result<Utf8PathBuf> {
        // Delete the output file if it exists.
        let output = output.as_ref();
        if output.exists() {
            std::fs::remove_file(&output)
                .with_context(|| format!("Failed to delete previous Fxfs file: {}", output))?;
        }

        // Write the blob manifest.
        let blob_manifest_path = gendir.as_ref().join("blob.manifest");
        self.manifest.write(&blob_manifest_path).context("Failed to write to blob.manifest")?;

        let blobs_json_path = gendir.as_ref().join("blobs.json");

        struct Cleanup(Option<String>);
        impl Drop for Cleanup {
            fn drop(&mut self) {
                if let Some(path) = &self.0 {
                    // Best-effort, ignore warnings.
                    let _ = std::fs::remove_file(path);
                }
            }
        }
        let mut cleanup = Cleanup(Some(output.as_str().to_string()));
        fxfs_make_blob_image::make_blob_image(
            output.as_str(),
            blob_manifest_path.as_str(),
            blobs_json_path.as_str(),
            self.size_bytes,
        )
        .await?;
        cleanup.0 = None;
        Ok(blobs_json_path)
    }
}

/// Read blobs.json file into a BlobsJson struct
pub fn read_blobs_json(path_buf: impl AsRef<Utf8Path>) -> Result<BlobsJson> {
    let mut file =
        File::open(path_buf.as_ref()).context(format!("Unable to open file blobs json file"))?;
    let blobs_json: BlobsJson =
        assembly_util::from_reader(&mut file).context("Failed to read blobs json file")?;
    Ok(blobs_json)
}

pub type BlobsJson = Vec<BlobJsonEntry>;

#[derive(Debug, Deserialize, PartialEq, Eq)]
pub struct BlobJsonEntry {
    pub merkle: String,
    pub used_space_in_blobfs: u64,
}

#[cfg(test)]
mod tests {
    use {
        super::*,
        camino::Utf8Path,
        fuchsia_merkle::MerkleTree,
        fuchsia_pkg::{BlobInfo, MetaPackage, PackageManifest, PackageManifestBuilder},
        std::fs::File,
        std::io::Write as _,
        std::path::Path,
        tempfile::TempDir,
    };

    // Generates a package manifest to be used for testing. The `name` is used in the blob file
    // names to make each manifest somewhat unique.
    // TODO(fxbug.dev/76993): See if we can share this with BasePackage.
    fn generate_test_manifest(name: &str, file_path: impl AsRef<Path>) -> PackageManifest {
        let source_path = file_path.as_ref().to_string_lossy().into_owned();
        let file = File::open(&file_path).unwrap();
        let merkle = MerkleTree::from_reader(&file).unwrap().root();
        let builder = PackageManifestBuilder::new(MetaPackage::from_name(name.parse().unwrap()));
        let builder = builder.add_blob(BlobInfo {
            source_path,
            path: "data/file.txt".into(),
            merkle,
            size: file.metadata().unwrap().len(),
        });
        builder.build()
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn fxfs_builder() {
        let tmp = TempDir::new().unwrap();

        let dir = Utf8Path::from_path(tmp.path()).unwrap();
        let filepath = dir.join("file.txt");
        let mut file = File::create(&filepath).unwrap();
        write!(file, "Boaty McBoatface").unwrap();
        let manifest = generate_test_manifest("package", filepath);

        let output_path = dir.join("fxfs.blk");
        let output_path_clone = output_path.clone();

        let mut builder = FxfsBuilder::new();
        builder.set_size(32 * 1024 * 1024);
        builder.add_package(manifest).unwrap();

        let blobs_json_path = builder.build(&dir, output_path).await.unwrap();
        let actual_blobs_json = read_blobs_json(blobs_json_path).unwrap();
        let expected_blobs_json = vec![BlobJsonEntry {
            merkle: "1739e556c9f2800c6263d8926ae00652d3c9a008b7a5ee501719854fe55b3787".to_string(),
            used_space_in_blobfs: 4096,
        }];
        assert_eq!(expected_blobs_json, actual_blobs_json);

        assert!(output_path_clone.exists());
    }
}
