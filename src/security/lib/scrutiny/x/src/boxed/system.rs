// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use super::api;
use super::blob::BlobDirectoryError;
use super::product_bundle::ProductBundle;
use super::update_package::Error as UpdatePackageError;
use super::update_package::UpdatePackage;
use std::rc::Rc;
use thiserror::Error;

/// Errors that may occur when constructing a [`System`].
#[derive(Debug, Error)]
pub enum Error {
    #[error("failed to extract blob directory from product bundle: {0}")]
    BlobDirectory(#[from] BlobDirectoryError),
    #[error("failed to construct update package: {0}")]
    UpdatePackage(#[from] UpdatePackageError),
}

#[derive(Clone)]
pub(crate) struct System(Rc<SystemData>);

impl System {
    /// Constructs a [`System`] backed by `product_bundle`.
    pub fn new(product_bundle: ProductBundle) -> Result<Self, Error> {
        let build_dir = product_bundle.directory().clone();
        let blob_set = product_bundle.blob_set()?;
        let update_package: Box<dyn api::UpdatePackage> = Box::new(UpdatePackage::new(
            Some(product_bundle.data_source().clone()),
            product_bundle.update_package_hash().clone(),
            blob_set,
        )?);
        Ok(Self(Rc::new(SystemData { build_dir, update_package })))
    }
}

impl api::System for System {
    fn build_dir(&self) -> Box<dyn api::Path> {
        self.0.build_dir.clone()
    }

    fn zbi(&self) -> Box<dyn api::Zbi> {
        todo!()
    }

    fn update_package(&self) -> Box<dyn api::UpdatePackage> {
        self.0.update_package.clone()
    }

    fn kernel_flags(&self) -> Box<dyn api::KernelFlags> {
        todo!()
    }

    fn vb_meta(&self) -> Box<dyn api::VbMeta> {
        todo!()
    }

    fn additional_boot_configuration(&self) -> Box<dyn api::AdditionalBootConfiguration> {
        todo!()
    }

    fn component_manager_configuration(&self) -> Box<dyn api::ComponentManagerConfiguration> {
        todo!()
    }
}

struct SystemData {
    build_dir: Box<dyn api::Path>,
    update_package: Box<dyn api::UpdatePackage>,
}

#[cfg(test)]
mod tests {
    use super::super::product_bundle::test::*;
    use super::super::product_bundle::ProductBundle;
    use super::super::update_package::test::FAKE_UPDATE_PACKAGE;
    use super::api::System as _;
    use super::*;
    use dyn_clone::DynClone;
    use fuchsia_merkle::Hash;
    use fuchsia_merkle::MerkleTree;
    use std::collections::HashMap;
    use std::fs;
    use std::path::Path;
    use tempfile::TempDir;

    fn path<P: AsRef<Path> + DynClone + 'static>(p: P) -> Box<dyn api::Path> {
        Box::new(p)
    }

    fn write_blob<Dir: AsRef<Path>>(directory: Dir, contents: &[u8]) -> Hash {
        let hash = MerkleTree::from_reader(contents).expect("compute fuchsia merkle tree").root();
        let filename = format!("{}", hash);
        fs::write(directory.as_ref().join(filename), contents).expect("write blob to file");
        hash
    }

    #[fuchsia::test]
    fn system_from_product_bundle() {
        let pb_dir = TempDir::new().expect("create tempdir for product bundle");
        let pb_path = path(pb_dir.path().to_path_buf());
        let blobs_path = pb_dir.path().join(V2_SDK_A_PRODUCT_BUNDLE_REPOSITORY_BLOBS_PATH);
        fs::create_dir_all(&blobs_path).expect("create blobs directory");

        let mut update_blob_hashes: Vec<Hash> = vec![];
        let mut blobs_map: HashMap<Hash, Vec<u8>> = HashMap::new();

        FAKE_UPDATE_PACKAGE.blobs.iter().for_each(|blob| {
            let hash = write_blob(&blobs_path, blob.as_slice());
            blobs_map.insert(hash.clone(), blob.clone());
            update_blob_hashes.push(hash);
        });

        // Write product bundle manifest.
        let update_package_hash = FAKE_UPDATE_PACKAGE.hash.clone();
        v2_sdk_a_product_bundle(pb_dir.path(), Some(update_package_hash.into()))
            .write(utf8_path(pb_dir.path()))
            .expect("write product bundle manifest");

        // Instantiate product bundle.
        let product_bundle = ProductBundle::new(pb_path.clone()).expect("create product bundle");

        // Instantiate system.
        let system = System::new(product_bundle).expect("create system instance");
        assert_eq!(system.build_dir().to_string(), pb_path.to_string());
        let update_package = system.update_package();
        assert_eq!(update_package.hash().to_string(), update_package_hash.to_string());
    }
}
