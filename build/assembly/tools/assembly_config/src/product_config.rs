// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::{common, ExtractProductPackageArgs, HybridProductArgs, ProductArgs};

use anyhow::{Context, Result};
use assembly_config_schema::AssemblyConfig;
use assembly_container::AssemblyContainer;
use assembly_release_info::{ProductReleaseInfo, ReleaseInfo};
use camino::Utf8PathBuf;
use fuchsia_pkg::{PackageBuilder, PackageManifest};
use product_input_bundle::ProductInputBundle;

pub fn new(args: &ProductArgs) -> Result<()> {
    let mut config = AssemblyConfig::from_config_path(&args.config)?;

    for path in &args.product_input_bundles {
        let pib = ProductInputBundle::from_dir(path)?;
        config.product_input_bundles.insert(pib.release_info.name.clone(), pib);
    }

    config.product.release_info = ProductReleaseInfo {
        info: ReleaseInfo {
            name: match config.product.build_info {
                Some(ref value) => value.name.clone(),
                // TODO(https://fxbug.dev/418249336): Make
                // product.build_info.name a required field.
                None => "unknown".to_string(),
            },
            repository: common::get_release_repository(&args.repo, &args.repo_file)?,
            version: common::get_release_version(
                &None,
                &config.product.build_info.as_ref().map(|b| b.version.clone()),
            )?,
        },
        pibs: config.product_input_bundles.values().map(|p| p.release_info.clone()).collect(),
    };

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

    // Replace PIBs that match an existing PIB by name.
    for path in &args.product_input_bundles {
        let pib = ProductInputBundle::from_dir(path)?;
        config.product_input_bundles.entry(pib.release_info.name.clone()).and_modify(|e| *e = pib);
    }

    config.write_to_dir(&args.output, args.depfile.as_ref())?;
    Ok(())
}

pub fn extract_package(args: &ExtractProductPackageArgs) -> Result<()> {
    let mut config = AssemblyConfig::from_dir(&args.config)?;

    if let Some(package_manifest_path) = find_package_in_product(&mut config, &args.package_name) {
        let manifest =
            PackageManifest::try_load_from(&package_manifest_path).with_context(|| {
                format!("Loading package manifest to extract: {}", &package_manifest_path)
            })?;
        let mut builder = PackageBuilder::from_manifest(manifest, &args.outdir)
            .with_context(|| format!("Loading package to extract: {}", &args.package_name))?;

        let metafar_path =
            args.output_package_manifest.parent().context("Invalid outdir")?.join("meta.far");
        builder.manifest_path(args.output_package_manifest.clone());
        builder
            .build(&args.outdir, &metafar_path)
            .with_context(|| format!("Writing out extracted package: {}", &args.package_name))?;
    } else {
        anyhow::bail!("Could not find package to extract: {}", &args.package_name);
    }

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
    use fuchsia_pkg::PackageName;
    use std::fs;
    use std::fs::File;
    use std::io::Write;
    use tempfile::{tempdir, NamedTempFile};
    use version_history::AbiRevision;

    const FAKE_ABI_REVISION: AbiRevision = AbiRevision::from_u64(0x5836508c2defac54);

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

        let args = ProductArgs {
            config: config_path,
            repo: None,
            repo_file: None,
            output: product_path.clone(),
            depfile: None,
            product_input_bundles: vec![],
        };
        let _ = new(&args);
        let config = AssemblyConfig::from_dir(product_path).unwrap();
        let expected = "fake_version".to_string();
        assert_eq!(expected, config.product.release_info.info.version);
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

        let args = ProductArgs {
            config: config_path,
            repo: None,
            repo_file: None,
            output: product_path.clone(),
            depfile: None,
            product_input_bundles: vec![],
        };
        let _ = new(&args);
        let config = AssemblyConfig::from_dir(product_path).unwrap();
        let expected = "unversioned".to_string();
        assert_eq!(expected, config.product.release_info.info.version);
    }

    #[test]
    fn test_extract_package() {
        let tmp_dir = tempdir().unwrap();
        let tmp_path = Utf8PathBuf::from_path_buf(tmp_dir.path().to_path_buf()).unwrap();
        let product_path = tmp_path.join("my_product");
        fs::create_dir(&product_path).unwrap();

        let packages_path = product_path.join("packages");
        fs::create_dir(&packages_path).unwrap();

        let gendir = tmp_path.join("gendir");
        fs::create_dir(&gendir).unwrap();

        let test_package_path = packages_path.join("test");
        let mut builder = PackageBuilder::new("test", FAKE_ABI_REVISION);
        builder.add_contents_as_blob("some/file", "foobar", &gendir).unwrap();
        builder.manifest_path(test_package_path);
        let metafar_path = packages_path.join("meta.far");
        builder.build(&packages_path, &metafar_path).unwrap();

        let config_path = product_path.join("product_configuration.json");
        let config_file = File::create(&config_path).unwrap();
        let config_value = serde_json::json!({
            "platform": {
                "build_type": "eng",
            },
            "product": {
                "packages": {
                    "base": {
                       "test" : {
                         "manifest": "packages/test"
                       },
                    },
                },
            },
        });
        serde_json::to_writer(&config_file, &config_value).unwrap();

        let outdir_path = tmp_path.join("outdir");
        let output_package_manifest_path = tmp_path.join("manifest.json");

        let args = ExtractProductPackageArgs {
            config: product_path,
            package_name: "test".into(),
            outdir: outdir_path,
            output_package_manifest: output_package_manifest_path.clone(),
            depfile: None,
        };
        extract_package(&args).unwrap();
        let extracted_package = PackageManifest::try_load_from(&output_package_manifest_path)
            .expect("Package manifest loaded");

        assert_eq!(extracted_package.name(), &"test".parse::<PackageName>().unwrap());
    }
}
