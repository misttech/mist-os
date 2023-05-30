// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::{Context, Result};
use assembly_config_schema::FileEntry;
use assembly_platform_configuration::DomainConfig;
use camino::{Utf8Path, Utf8PathBuf};
use fidl::persist;
use fuchsia_pkg::{PackageBuilder, PackageManifest, RelativeTo};
use std::io::Write;

/// A builder that takes domain configs and places them in a new package that
/// can be placed in the assembly.
pub struct DomainConfigPackage {
    config: DomainConfig,
}

impl DomainConfigPackage {
    /// Construct a new domain config package.
    pub fn new(config: DomainConfig) -> Self {
        Self { config }
    }

    /// Build the package and return the package manifest and path to manifest.
    pub fn build(self, outdir: impl AsRef<Utf8Path>) -> Result<(Utf8PathBuf, PackageManifest)> {
        let outdir = outdir.as_ref();
        std::fs::create_dir_all(&outdir)
            .with_context(|| format!("creating directory {}", &outdir))?;

        let mut builder = PackageBuilder::new(&self.config.name);
        let manifest_path = outdir.join("package_manifest.json");
        let metafar_path = outdir.join("meta.far");
        builder.manifest_path(&manifest_path);
        builder.manifest_blobs_relative_to(RelativeTo::File);

        // Find all the directory routes to expose.
        let mut exposes = vec![];
        for (directory, directory_config) in self.config.directories {
            let subdir = cml::RelativePath::new(&directory)
                .with_context(|| format!("Calculating relative path for {directory}"))?;
            let name = cml::Name::try_new(&directory)
                .with_context(|| format!("Calculating name for {directory}"))?;
            exposes.push(cml::Expose {
                // unwrap is safe, because try_new cannot fail with "pkg".
                directory: Some(cml::OneOrMany::One(cml::Name::try_new("pkg").unwrap())),
                r#as: Some(name),
                subdir: Some(subdir),
                ..cml::Expose::new_from(cml::OneOrMany::One(cml::ExposeFromRef::Framework))
            });

            // Add an empty file to the directory to ensure the directory gets created.
            let empty_file_name = "_ensure_directory_creation";
            let empty_file_path = outdir.join(empty_file_name);
            let destination = Utf8PathBuf::from(&directory).join(empty_file_name);
            std::fs::write(&empty_file_path, "").context("writing empty file")?;
            builder
                .add_file_as_blob(destination, &empty_file_path)
                .with_context(|| format!("adding empty file {empty_file_path}"))?;

            // Add the necessary config files to the directory.
            for (_, FileEntry { source, destination }) in directory_config.entries {
                let destination = Utf8PathBuf::from(&directory).join(destination);
                builder
                    .add_file_as_blob(destination, &source)
                    .with_context(|| format!("adding config {source}"))?;
            }
        }

        let cml = cml::Document { expose: Some(exposes), ..Default::default() };
        let out_data = cml::compile(&cml, cml::CompileOptions::default())
            .with_context(|| format!("compiling domain config routes"))?;
        let cm_name = format!("{}.cm", &self.config.name);
        let cm_path = outdir.join(&cm_name);
        let mut cm_file = std::fs::File::create(&cm_path)
            .with_context(|| format!("creating domain config routes: {cm_path}"))?;
        cm_file
            .write_all(&persist(&out_data)?)
            .with_context(|| format!("writing domain config routes: {cm_path}"))?;
        builder
            .add_file_to_far(format!("meta/{cm_name}"), &cm_path)
            .with_context(|| format!("adding file to domain config package: {cm_path}"))?;

        let manifest = builder
            .build(&outdir, metafar_path)
            .with_context(|| format!("building domain config package: {}", &self.config.name))?;
        Ok((manifest_path, manifest))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use assembly_platform_configuration::DomainConfigDirectory;
    use assembly_util::NamedMap;
    use assert_matches::assert_matches;
    use cm_rust::{
        CapabilityName, ComponentDecl, ExposeDecl, ExposeDirectoryDecl, ExposeSource, ExposeTarget,
    };
    use fidl::unpersist;
    use fidl_fuchsia_component_decl::Component;
    use fuchsia_archive::Utf8Reader;
    use fuchsia_hash::Hash;
    use fuchsia_pkg::PackageName;
    use pretty_assertions::assert_eq;
    use std::fs::File;
    use std::path::PathBuf;
    use std::str::FromStr;
    use tempfile::tempdir;

    #[test]
    fn build() {
        let tmp = tempdir().unwrap();
        let outdir = Utf8Path::from_path(tmp.path()).unwrap();

        // Prepare the domain config input.
        let config_source = outdir.join("config_source.json");
        std::fs::write(&config_source, "bleep bloop").unwrap();
        let mut entries = NamedMap::<FileEntry>::new("config files");
        entries
            .try_insert_unique(
                "config.json".to_string(),
                FileEntry { source: config_source.clone(), destination: "config.json".into() },
            )
            .unwrap();
        let config = DomainConfig {
            name: "my-package".into(),
            directories: [("config-dir".into(), DomainConfigDirectory { entries })].into(),
        };

        let package = DomainConfigPackage::new(config);
        let (path, manifest) = package.build(&outdir).unwrap();

        // Assert the manifest is correct.
        assert_eq!(path, outdir.join("package_manifest.json"));
        let loaded_manifest = PackageManifest::try_load_from(path).unwrap();
        assert_eq!(manifest, loaded_manifest);
        assert_eq!(manifest.name(), &PackageName::from_str("my-package").unwrap());
        let blobs = manifest.into_blobs();
        assert_eq!(blobs.len(), 3);
        let blob = blobs.iter().find(|&b| &b.path == "meta/").unwrap();
        assert_eq!(blob.source_path, outdir.join("meta.far").to_string());
        let blob =
            blobs.iter().find(|&b| &b.path == "config-dir/_ensure_directory_creation").unwrap();
        assert_eq!(blob.source_path, outdir.join("_ensure_directory_creation").to_string());
        let blob = blobs.iter().find(|&b| &b.path == "config-dir/config.json").unwrap();
        assert_eq!(blob.source_path, config_source.to_string());
        assert_eq!(
            blob.merkle,
            Hash::from_str("ba2747adb0a7126408af2ea0071fa8ae85d70ee2ab171aa0d0073f28b3ebcfcb")
                .unwrap()
        );
        assert_eq!(blob.size, 11);

        // Assert the contents of the package are correct.
        let far_path = outdir.join("meta.far");
        let mut far_reader = Utf8Reader::new(File::open(&far_path).unwrap()).unwrap();
        let package = far_reader.read_file("meta/package").unwrap();
        assert_eq!(package, br#"{"name":"my-package","version":"0"}"#);
        let cm_bytes = far_reader.read_file("meta/my-package.cm").unwrap();
        let fidl_component_decl: Component = unpersist(&cm_bytes).unwrap();
        let component = ComponentDecl::try_from(fidl_component_decl).unwrap();
        assert_eq!(component.exposes.len(), 1);
        assert_matches!(&component.exposes[0], ExposeDecl::Directory(ExposeDirectoryDecl {
            source: ExposeSource::Framework,
            source_name,
            target: ExposeTarget::Parent,
            target_name,
            rights: _,
            subdir: Some(subdir),
            availability: _,
        }) => {
            assert_eq!(source_name, &CapabilityName::from("pkg"));
            assert_eq!(target_name, &CapabilityName::from("config-dir"));
            assert_eq!(subdir, &PathBuf::from("config-dir"));
        });
        let contents = far_reader.read_file("meta/contents").unwrap();
        let contents = std::str::from_utf8(&contents).unwrap();
        let expected_contents = "\
            config-dir/_ensure_directory_creation=15ec7bf0b50732b49f8228e07d24365338f9e3ab994b00af08e5a3bffe55fd8b\n\
            config-dir/config.json=ba2747adb0a7126408af2ea0071fa8ae85d70ee2ab171aa0d0073f28b3ebcfcb\n\
        "
        .to_string();
        assert_eq!(expected_contents, contents);
    }

    #[test]
    fn build_no_directories() {
        let tmp = tempdir().unwrap();
        let outdir = Utf8Path::from_path(tmp.path()).unwrap();

        // Prepare the domain config input.
        let config =
            DomainConfig { name: "my-package".into(), directories: NamedMap::new("directories") };
        let package = DomainConfigPackage::new(config);
        let (path, manifest) = package.build(&outdir).unwrap();

        // Assert the manifest is correct.
        assert_eq!(path, outdir.join("package_manifest.json"));
        let loaded_manifest = PackageManifest::try_load_from(path).unwrap();
        assert_eq!(manifest, loaded_manifest);
        assert_eq!(manifest.name(), &PackageName::from_str("my-package").unwrap());
        let blobs = manifest.into_blobs();
        assert_eq!(blobs.len(), 1);
        let blob = blobs.iter().find(|&b| b.path == "meta/").unwrap();
        assert_eq!(blob.source_path, outdir.join("meta.far").to_string());

        // Assert the contents of the package are correct.
        let far_path = outdir.join("meta.far");
        let mut far_reader = Utf8Reader::new(File::open(&far_path).unwrap()).unwrap();
        let package = far_reader.read_file("meta/package").unwrap();
        assert_eq!(package, br#"{"name":"my-package","version":"0"}"#);
        let cm_bytes = far_reader.read_file("meta/my-package.cm").unwrap();
        let fidl_component_decl: Component = unpersist(&cm_bytes).unwrap();
        let component = ComponentDecl::try_from(fidl_component_decl).unwrap();
        assert_eq!(component.exposes.len(), 0);
        let contents = far_reader.read_file("meta/contents").unwrap();
        let contents = std::str::from_utf8(&contents).unwrap();
        let expected_contents = "\
        "
        .to_string();
        assert_eq!(expected_contents, contents);
    }

    #[test]
    fn build_no_configs() {
        let tmp = tempdir().unwrap();
        let outdir = Utf8Path::from_path(tmp.path()).unwrap();

        // Prepare the domain config input.
        let entries = NamedMap::<FileEntry>::new("config files");
        let config = DomainConfig {
            name: "my-package".into(),
            directories: [("config-dir".into(), DomainConfigDirectory { entries })].into(),
        };

        let package = DomainConfigPackage::new(config);
        let (path, manifest) = package.build(&outdir).unwrap();

        // Assert the manifest is correct.
        assert_eq!(path, outdir.join("package_manifest.json"));
        let loaded_manifest = PackageManifest::try_load_from(path).unwrap();
        assert_eq!(manifest, loaded_manifest);
        assert_eq!(manifest.name(), &PackageName::from_str("my-package").unwrap());
        let blobs = manifest.into_blobs();
        assert_eq!(blobs.len(), 2);
        let blob = blobs.iter().find(|&b| &b.path == "meta/").unwrap();
        assert_eq!(blob.source_path, outdir.join("meta.far").to_string());
        let blob =
            blobs.iter().find(|&b| &b.path == "config-dir/_ensure_directory_creation").unwrap();
        assert_eq!(blob.source_path, outdir.join("_ensure_directory_creation").to_string());

        // Assert the contents of the package are correct.
        let far_path = outdir.join("meta.far");
        let mut far_reader = Utf8Reader::new(File::open(&far_path).unwrap()).unwrap();
        let package = far_reader.read_file("meta/package").unwrap();
        assert_eq!(package, br#"{"name":"my-package","version":"0"}"#);
        let cm_bytes = far_reader.read_file("meta/my-package.cm").unwrap();
        let fidl_component_decl: Component = unpersist(&cm_bytes).unwrap();
        let component = ComponentDecl::try_from(fidl_component_decl).unwrap();
        assert_eq!(component.exposes.len(), 1);
        assert_matches!(&component.exposes[0], ExposeDecl::Directory(ExposeDirectoryDecl {
            source: ExposeSource::Framework,
            source_name,
            target: ExposeTarget::Parent,
            target_name,
            rights: _,
            subdir: Some(subdir),
            availability: _,
        }) => {
            assert_eq!(source_name, &CapabilityName::from("pkg"));
            assert_eq!(target_name, &CapabilityName::from("config-dir"));
            assert_eq!(subdir, &PathBuf::from("config-dir"));
        });
        let contents = far_reader.read_file("meta/contents").unwrap();
        let contents = std::str::from_utf8(&contents).unwrap();
        let expected_contents = "\
            config-dir/_ensure_directory_creation=15ec7bf0b50732b49f8228e07d24365338f9e3ab994b00af08e5a3bffe55fd8b\n\
        "
        .to_string();
        assert_eq!(expected_contents, contents);
    }
}
