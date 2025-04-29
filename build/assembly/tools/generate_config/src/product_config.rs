// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::{common, ExtractProductPackageArgs, HybridProductArgs, ProductArgs};

use anyhow::Result;
use assembly_config_schema::AssemblyConfig;
use assembly_container::AssemblyContainer;
use camino::Utf8PathBuf;
use fuchsia_pkg::PackageManifest;

pub fn new(args: &ProductArgs) -> Result<()> {
    let mut config = AssemblyConfig::from_config_path(&args.config)?;

    // Copy the version information from product.build_info.
    config.product.release_version = Some(common::get_release_version(
        &None,
        &config.product.build_info.as_ref().map(|b| b.version.clone()),
    )?);

    // Build systems generally don't add package names to the config, so it
    // serializes index numbers in place of package names by default.
    // We add the package names in now, so all the rest of the rules can assume
    // the config has proper package names.
    let config = config.add_package_names()?;
    config.write_to_dir(&args.output, args.depfile.as_ref())?;
    Ok(())
}

pub fn hybrid(args: &HybridProductArgs) -> Result<()> {
    let config = AssemblyConfig::from_dir(&args.input)?;

    // Normally this would not be necessary, because all generated configs come
    // from this tool, which adds the package names above, but we still need to
    // support older product configs without names.
    let mut config = config.add_package_names()?;

    for package_manifest_path in &args.replace_package {
        let package_manifest = PackageManifest::try_load_from(&package_manifest_path)?;
        let package_name = package_manifest.name();
        if let Some(path) = find_package_in_product(&mut config, &package_name) {
            *path = package_manifest_path.clone();
        }
    }
    config.write_to_dir(&args.output, args.depfile.as_ref())?;
    Ok(())
}

pub fn extract_package(_: &ExtractProductPackageArgs) -> Result<()> {
    Ok(())
}

fn find_package_in_product<'a>(
    config: &'a mut AssemblyConfig,
    package_name: impl AsRef<str>,
) -> Option<&'a mut Utf8PathBuf> {
    config.product.packages.base.iter_mut().chain(&mut config.product.packages.cache).find_map(
        |(name, pkg)| {
            if name == package_name.as_ref() {
                return Some(&mut pkg.manifest);
            }
            return None;
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::fs::File;
    use std::io::Write;
    use tempfile::{tempdir, NamedTempFile};

    fn create_tmp_file(content: String) -> (NamedTempFile, Utf8PathBuf) {
        let file = NamedTempFile::new().unwrap();
        let path = Utf8PathBuf::from_path_buf(file.path().to_path_buf()).unwrap();
        let file_write = file.reopen();
        file_write.unwrap().write_all(content.as_bytes()).unwrap();
        (file, path)
    }

    #[test]
    fn test_versioned() {
        let (_version_file, version_path) = create_tmp_file("fake_version".to_string());
        let (_jiri_snapshot_file, jiri_snapshot_path) = create_tmp_file("snapshot".to_string());
        let (_latest_commit_date_file, latest_commit_date_path) =
            create_tmp_file("timestamp".to_string());

        let tmp_dir = tempdir().unwrap();
        let tmp_path = Utf8PathBuf::from_path_buf(tmp_dir.path().to_path_buf()).unwrap();
        let product_path = tmp_path.join("my_product");
        fs::create_dir(&product_path).unwrap();

        let config_path = product_path.join("product_configuration.json");
        let config_file = File::create(&config_path).unwrap();
        let config_value = serde_json::json!({
            "platform": {
                "build_type": "eng",
            },
            "product": {
                "build_info": {
                    "name": "fake_product_name",
                    "version": version_path,
                    "jiri_snapshot": jiri_snapshot_path,
                    "latest_commit_date": latest_commit_date_path,
                }
            }
        });
        serde_json::to_writer(&config_file, &config_value).unwrap();

        let args = ProductArgs { config: config_path, output: product_path.clone(), depfile: None };
        let _ = new(&args);
        let config = AssemblyConfig::from_dir(product_path).unwrap();
        let expected = "fake_version".to_string();
        assert_eq!(expected, config.product.release_version.unwrap());
    }

    #[test]
    fn test_unversioned() {
        let tmp_dir = tempdir().unwrap();
        let tmp_path = Utf8PathBuf::from_path_buf(tmp_dir.path().to_path_buf()).unwrap();
        let product_path = tmp_path.join("my_product");
        fs::create_dir(&product_path).unwrap();

        let config_path = product_path.join("product_configuration.json");
        let config_file = File::create(&config_path).unwrap();
        let config_value = serde_json::json!({
            "platform": {
                "build_type": "eng",
            },
            "product": {
            }
        });
        serde_json::to_writer(&config_file, &config_value).unwrap();

        let args = ProductArgs { config: config_path, output: product_path.clone(), depfile: None };
        let _ = new(&args);
        let config = AssemblyConfig::from_dir(product_path).unwrap();
        let expected = "unversioned".to_string();
        assert_eq!(expected, config.product.release_version.unwrap());
    }
}
