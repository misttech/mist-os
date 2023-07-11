// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    anyhow::{Context, Result},
    assembly_config_schema::DriverDetails,
    camino::Utf8Path,
    fuchsia_pkg::PackageManifest,
    serde::{Deserialize, Serialize},
    std::fs::File,
};

/// Possible driver package.
pub enum DriverPackageType {
    /// A base-driver package
    Base,
    /// A boot-driver package
    Boot,
}

/// A driver manifest fragment.
#[derive(Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct DriverManifest {
    /// Url of a driver to load at boot.
    pub driver_url: String,
}

/// A builder for the driver manifest package.
#[derive(Default)]
pub struct DriverManifestBuilder {
    drivers: Vec<DriverManifest>,
}

impl DriverManifestBuilder {
    /// Add a driver manifest fragment to the driver manifest.
    pub fn add_driver(&mut self, driver_details: DriverDetails, package_url: &str) -> Result<()> {
        let driver_manifests = driver_details
            .components
            .iter()
            .map(|component_path| DriverManifest {
                driver_url: format!("{}#{}", package_url, component_path),
            })
            .collect::<Vec<DriverManifest>>();

        self.drivers.extend(driver_manifests);
        Ok(())
    }

    /// Create the driver manifest.
    pub fn create_manifest_file(&self, manifest_path: &Utf8Path) -> Result<()> {
        if let Some(parent) = manifest_path.parent() {
            std::fs::create_dir_all(parent).context(format!(
                "Creating parent dir {} for {} in gendir",
                parent, manifest_path
            ))?;
        }
        let manifest_file = File::create(&manifest_path)
            .context(format!("Creating the driver manifest file: {}", manifest_path))?;
        serde_json::to_writer(manifest_file, &self.drivers)
            .context(format!("Writing the manifest file {}", manifest_path))?;
        Ok(())
    }

    /// Helper function to determine a driver's package url
    pub fn get_package_url(
        package_type: DriverPackageType,
        path: impl AsRef<Utf8Path>,
    ) -> Result<String> {
        // Load the PackageManifest from the given path
        let manifest = PackageManifest::try_load_from(&path).with_context(|| {
            format!("parsing driver package {} as a package manifest", path.as_ref())
        })?;
        match package_type {
            DriverPackageType::Base => {
                let repository = manifest.repository().unwrap_or("fuchsia.com");
                Ok(format!("fuchsia-pkg://{}/{}", repository, manifest.name()))
            }
            DriverPackageType::Boot => Ok(format!("fuchsia-boot:///{}", manifest.name())),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::BASE_DRIVER_MANIFEST_PATH;
    use assembly_test_util::generate_test_manifest;
    use camino::{Utf8Path, Utf8PathBuf};
    use std::fs;
    use std::io::Write;
    use tempfile::TempDir;

    #[test]
    fn create_manifest_file() -> Result<()> {
        let tmp = TempDir::new()?;
        let outdir = Utf8Path::from_path(tmp.path()).unwrap();

        std::fs::create_dir(outdir.join("driver"))?;
        let driver_package_manifest_file_path = outdir.join("driver/package_manifest.json");
        let mut driver_package_manifest_file = File::create(&driver_package_manifest_file_path)?;
        let package_manifest = generate_test_manifest("base_driver", None);
        serde_json::to_writer(&driver_package_manifest_file, &package_manifest)?;
        driver_package_manifest_file.flush()?;

        let driver_details = DriverDetails {
            package: driver_package_manifest_file_path.to_owned(),
            components: vec![Utf8PathBuf::from("meta/foobar.cm")],
        };
        let mut driver_manifest_builder = DriverManifestBuilder::default();
        driver_manifest_builder.add_driver(
            driver_details,
            &DriverManifestBuilder::get_package_url(
                DriverPackageType::Base,
                driver_package_manifest_file_path,
            )?,
        )?;

        let manifest_path = &outdir.join(BASE_DRIVER_MANIFEST_PATH);
        driver_manifest_builder.create_manifest_file(manifest_path)?;

        let manifest_contents = fs::read_to_string(manifest_path)?;
        assert_eq!(
            "[{\"driver_url\":\"fuchsia-pkg://testrepository.com/base_driver#meta/foobar.cm\"}]",
            manifest_contents
        );

        Ok(())
    }
}
