// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::{anyhow, bail, Context, Result};
use assembly_bootfs_file_map::BootfsFileMap;
use assembly_components::ComponentBuilder;
use assembly_config_schema::{
    assembly_config::{CompiledPackageDefinition, MainPackageDefinition},
    FileEntry,
};
use assembly_tool::Tool;
use assembly_util::{DuplicateKeyError, InsertUniqueExt, MapEntry};
use camino::{Utf8Path, Utf8PathBuf};
use fuchsia_pkg::{PackageBuilder, RelativeTo};
use serde::Serialize;
use std::collections::BTreeMap;

#[derive(Debug, Default, PartialEq, Serialize)]
pub struct CompiledPackageBuilder {
    pub name: String,
    component_shards: BTreeMap<String, BTreeMap<String, Utf8PathBuf>>,
    main_definition: Option<MainPackageDefinition>,
    main_bundle_dir: Utf8PathBuf,
}

/// Builds `CompiledPackageDefinition`s which are specified for Assembly
/// to create into packages.
impl CompiledPackageBuilder {
    pub fn new(name: impl Into<String>) -> CompiledPackageBuilder {
        CompiledPackageBuilder { name: name.into(), ..Default::default() }
    }

    /// Add a package definition to the compiled package
    ///
    /// # Arguments
    ///
    /// * entry -- a reference to a [CompiledPackageDefinition]. Each package
    ///     should have exactly one [MainPackageDefinition]. An error will be
    ///     returned if a second is added.
    /// * bundle_dir -- location of this [CompiledPackageDefinition]'s AIB.
    ///     The locations of the files in the [CompiledPackageDefinition]
    ///     are defined relative to the bundle.
    pub fn add_package_def(
        &mut self,
        entry: &CompiledPackageDefinition,
        bundle_dir: impl AsRef<Utf8Path>,
    ) -> Result<&mut Self> {
        let name = entry.name();

        if name != self.name {
            bail!(
                "PackageEntry name '{name}' does not match CompiledPackageDefinition name '{}'",
                &self.name
            );
        }

        match entry {
            CompiledPackageDefinition::MainDefinition(def) => {
                if self.main_definition.is_some() {
                    bail!("Duplicate main definition for package {name}");
                }

                if def.bootfs_unpackaged && !def.contents.is_empty() {
                    bail!("Failure adding {}. Bootfs compiled packages should only define components, not contents", def.name);
                }

                self.main_definition = Some(def.clone());
                self.main_bundle_dir = bundle_dir.as_ref().into();
            }
            CompiledPackageDefinition::Additional(def) => {
                for (component_name, component_shards_to_add) in def.component_shards.clone() {
                    // Get the existing shards for this component, or create a new container for them.
                    let component_shards =
                        self.component_shards.entry(component_name.clone()).or_default();

                    // Attempt to add each of the component shards, using their filename (not their
                    // full path) as the sorting key, so that they have a stable sort order as they
                    // are moved around.
                    for shard_path in component_shards_to_add {
                        let filename = shard_path.file_name().ok_or_else(|| {
                            anyhow!(
                                "The component shard path does not have a filename: {}",
                                shard_path
                            )
                        })?;
                        component_shards
                            .try_insert_unique(MapEntry(
                                filename.to_string(),
                                bundle_dir.as_ref().join(shard_path),
                            ))
                            .map_err(|shard| {
                                anyhow!(
                                    "Duplicate component shard found for {}/meta/{}.cm: {} \n          {}\n        and\n          {})",
                                    &self.name,
                                    &component_name,
                                    shard.key(),
                                    shard.previous_value(),
                                    shard.new_value(),
                                )
                            })?;
                    }
                }
            }
        }

        Ok(self)
    }

    fn validate(&self) -> Result<()> {
        let main_definition = self
            .main_definition
            .as_ref()
            .with_context(|| format!("main definition for package '{}' not found", &self.name))?
            .clone();

        for name in self.component_shards.keys() {
            if !main_definition.components.contains_key(name) {
                bail!(
                    "component '{}' for package '{}' does not have a MainDefinition in any AIB",
                    name,
                    &self.name
                );
            }
        }

        Ok(())
    }

    fn build_component(
        &self,
        cmc_tool: &dyn Tool,
        component_name: &String,
        cml: &Utf8PathBuf,
        outdir: impl AsRef<Utf8Path>,
    ) -> Result<Utf8PathBuf> {
        let mut component_builder = ComponentBuilder::new(component_name);
        component_builder.add_shard(self.main_bundle_dir.join(cml)).with_context(|| {
            format!("Adding cml for component: '{component_name}' to package: '{}'", &self.name)
        })?;

        if let Some(cml_shards) = self.component_shards.get(component_name) {
            for cml_shard in cml_shards.values() {
                component_builder.add_shard(cml_shard.as_path()).with_context(|| {
                    format!("Adding shard for: '{component_name}' to package '{}'", &self.name)
                })?;
            }
        }

        component_builder.build(
            &outdir,
            cmc_tool,
            self.main_bundle_dir.join("compiled_packages").join("include"),
        )
    }

    /// Build the compiled package as files in bootfs
    fn build_bootfs(
        &self,
        cmc_tool: &dyn Tool,
        bootfs_files: &mut BootfsFileMap,
        main_definition: &MainPackageDefinition,
        outdir: impl AsRef<Utf8Path>,
    ) -> Result<()> {
        let outdir = outdir.as_ref().join(&self.name);

        if !main_definition.contents.is_empty() {
            bail!("Failure adding {}. Bootfs compiled packages should only define components, not contents", &main_definition.name);
        }

        for (component_name, cml) in &main_definition.components {
            let component_manifest_path = &self
                .build_component(cmc_tool, component_name, cml, &outdir)
                .with_context(|| format!("building component {component_name}"))?;
            let component_manifest_file_name =
                component_manifest_path.file_name().context("component file name")?;
            let component_path = format!("meta/{component_manifest_file_name}");
            bootfs_files.add_entry(FileEntry {
                source: component_manifest_path.to_owned(),
                destination: component_path,
            })?;
        }

        Ok(())
    }

    /// Build the compiled package as a package
    fn build_package(
        &self,
        cmc_tool: &dyn Tool,
        main_definition: &MainPackageDefinition,
        outdir: impl AsRef<Utf8Path>,
    ) -> Result<Utf8PathBuf> {
        self.validate()?;
        let outdir = outdir.as_ref().join(&self.name);

        let mut package_builder = PackageBuilder::new(&self.name);
        package_builder.repository("fuchsia.com");

        for (component_name, cml) in &main_definition.components {
            let component_manifest_path = &self
                .build_component(cmc_tool, component_name, cml, &outdir)
                .with_context(|| format!("building component {component_name}"))?;
            let component_manifest_file_name =
                component_manifest_path.file_name().context("component file name")?;

            let component_path = format!("meta/{component_manifest_file_name}");
            package_builder.add_file_to_far(component_path, component_manifest_path)?;
        }

        for entry in &main_definition.contents {
            package_builder
                .add_file_as_blob(&entry.destination, &self.main_bundle_dir.join(&entry.source))?;
        }

        let package_manifest_path = outdir.join("package_manifest.json");
        package_builder.manifest_path(&package_manifest_path);
        package_builder.manifest_blobs_relative_to(RelativeTo::File);
        let metafar_path = outdir.join(format!("{}.far", &self.name));
        package_builder.build(&outdir, metafar_path).context("building package")?;

        Ok(package_manifest_path)
    }

    /// Build the components for the package and either build the package
    /// or extract the contents to bootfs
    ///
    /// Returns a path to the package manifest, if we actually build a package
    pub fn build(
        self,
        cmc_tool: &dyn Tool,
        bootfs_files: &mut BootfsFileMap,
        outdir: impl AsRef<Utf8Path>,
    ) -> Result<Option<Utf8PathBuf>> {
        let main_definition = &self.main_definition.as_ref().context("no main definition")?;
        if main_definition.bootfs_unpackaged {
            match self.build_bootfs(cmc_tool, bootfs_files, main_definition, outdir) {
                Ok(()) => Ok(Option::None),
                Err(e) => Err(e),
            }
        } else {
            Ok(Option::Some(self.build_package(cmc_tool, main_definition, outdir)?))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use assembly_config_schema::{assembly_config::AdditionalPackageContents, FileEntry};
    use assembly_tool::testing::FakeToolProvider;
    use assembly_tool::ToolProvider;
    use fuchsia_archive::Utf8Reader;
    use fuchsia_pkg::PackageManifest;
    use std::fs::File;
    use tempfile::TempDir;

    #[test]
    fn add_package_def_appends_entries_to_builder() {
        let mut compiled_package_builder = CompiledPackageBuilder::new("foo");
        let outdir_tmp = TempDir::new().unwrap();
        let outdir = Utf8Path::from_path(outdir_tmp.path()).unwrap();
        make_test_package_and_components(outdir);

        compiled_package_builder
            .add_package_def(
                &CompiledPackageDefinition::MainDefinition(MainPackageDefinition {
                    name: "foo".into(),
                    components: BTreeMap::from([
                        ("component1".into(), "cml1".into()),
                        ("component2".into(), "cml2".into()),
                    ]),
                    contents: vec![FileEntry {
                        source: outdir.join("file1"),
                        destination: "file1".into(),
                    }],
                    includes: Vec::default(),
                    bootfs_unpackaged: false,
                }),
                outdir,
            )
            .unwrap()
            .add_package_def(
                &CompiledPackageDefinition::Additional(AdditionalPackageContents {
                    name: "foo".into(),
                    component_shards: BTreeMap::from([(
                        "component2".into(),
                        vec!["shard1".into()],
                    )]),
                }),
                outdir,
            )
            .unwrap();

        assert!(compiled_package_builder.main_definition.is_some());
        assert_eq!(
            compiled_package_builder,
            CompiledPackageBuilder {
                name: "foo".into(),
                component_shards: BTreeMap::from([(
                    "component2".into(),
                    BTreeMap::from([("shard1".into(), outdir.join("shard1"))])
                )]),
                main_bundle_dir: outdir.into(),
                main_definition: Some(MainPackageDefinition {
                    name: "foo".into(),
                    components: BTreeMap::from([
                        ("component1".into(), "cml1".into()),
                        ("component2".into(), "cml2".into()),
                    ]),
                    contents: vec![FileEntry {
                        source: outdir.join("file1"),
                        destination: "file1".into()
                    }],
                    includes: Vec::default(),
                    bootfs_unpackaged: false,
                })
            }
        );
    }

    #[test]
    fn build_builds_package() {
        let mut compiled_package_builder = CompiledPackageBuilder::new("foo");
        let tools = FakeToolProvider::default();
        let outdir_tmp = TempDir::new().unwrap();
        let outdir = Utf8Path::from_path(outdir_tmp.path()).unwrap();
        let mut bootfs_files = BootfsFileMap::new();
        make_test_package_and_components(outdir);

        compiled_package_builder
            .add_package_def(
                &CompiledPackageDefinition::MainDefinition(MainPackageDefinition {
                    name: "foo".into(),
                    components: BTreeMap::from([
                        ("component1".into(), "cml1".into()),
                        ("component2".into(), "cml2".into()),
                    ]),
                    contents: vec![FileEntry {
                        source: outdir.join("file1"),
                        destination: "file1".into(),
                    }],
                    includes: Vec::default(),
                    bootfs_unpackaged: false,
                }),
                outdir,
            )
            .unwrap()
            .add_package_def(
                &CompiledPackageDefinition::Additional(AdditionalPackageContents {
                    name: "foo".into(),
                    component_shards: BTreeMap::from([(
                        "component2".into(),
                        vec!["shard1".into()],
                    )]),
                }),
                outdir,
            )
            .unwrap();

        compiled_package_builder
            .build(tools.get_tool("cmc").unwrap().as_ref(), &mut bootfs_files, outdir)
            .unwrap();

        let compiled_package_file = File::open(outdir.join("foo/foo.far")).unwrap();
        let mut far_reader = Utf8Reader::new(&compiled_package_file).unwrap();
        let manifest_path = outdir.join("foo/package_manifest.json");
        assert_far_contents_eq(
            &mut far_reader,
            "meta/contents",
            "file1=b5209759e76a8343c45b8c7abad13a1f0609512865ee7f7f5533212d8ab334dc\n",
        );
        assert_far_contents_eq(&mut far_reader, "meta/component1.cm", "component fake contents");
        let package_manifest = PackageManifest::try_load_from(manifest_path).unwrap();
        assert_eq!(package_manifest.name().as_ref(), "foo");
    }

    #[test]
    fn add_package_def_with_wrong_name_returns_err() {
        let mut compiled_package_builder = CompiledPackageBuilder::new("bar");

        let result = compiled_package_builder.add_package_def(
            &CompiledPackageDefinition::MainDefinition(MainPackageDefinition {
                name: "foo".into(),
                components: BTreeMap::new(),
                contents: Vec::default(),
                includes: Vec::default(),
                bootfs_unpackaged: false,
            }),
            "assembly/input/bundle/path/compiled_packages/include",
        );

        assert!(result.is_err());
    }

    #[test]
    fn add_package_def_with_bootfs_with_contents_returns_err() {
        let mut compiled_package_builder = CompiledPackageBuilder::new("bar");

        let result = compiled_package_builder.add_package_def(
            &CompiledPackageDefinition::MainDefinition(MainPackageDefinition {
                name: "bar".into(),
                components: BTreeMap::new(),
                contents: vec![FileEntry { source: "file1".into(), destination: "file1".into() }],
                includes: Vec::default(),
                bootfs_unpackaged: true,
            }),
            "assembly/input/bundle/path/compiled_packages/include",
        );

        assert!(result.is_err());
    }

    #[test]
    fn validate_without_main_definition_returns_err() {
        let mut compiled_package_builder = CompiledPackageBuilder::new("foo");
        compiled_package_builder
            .add_package_def(
                &CompiledPackageDefinition::Additional(AdditionalPackageContents {
                    name: "foo".into(),
                    component_shards: BTreeMap::from([(
                        "component2".into(),
                        vec!["shard1".into()],
                    )]),
                }),
                "assembly/input/bundle/path/compiled_packages/include",
            )
            .unwrap();

        let result = compiled_package_builder.validate();

        assert!(result.is_err());
    }

    #[test]
    fn add_package_def_with_duplicate_main_definition_returns_err() {
        let mut compiled_package_builder = CompiledPackageBuilder::new("foo");
        compiled_package_builder
            .add_package_def(
                &CompiledPackageDefinition::MainDefinition(MainPackageDefinition {
                    name: "foo".into(),
                    ..Default::default()
                }),
                "assembly/input/bundle/path/compiled_packages/include",
            )
            .unwrap();

        let result = compiled_package_builder.add_package_def(
            &CompiledPackageDefinition::MainDefinition(MainPackageDefinition {
                name: "foo".into(),
                ..Default::default()
            }),
            "assembly/input/bundle/path/compiled_packages/include",
        );

        assert!(result.is_err());
    }

    fn assert_far_contents_eq(
        far_reader: &mut Utf8Reader<&File>,
        path: &str,
        expected_contents: &str,
    ) {
        let contents = far_reader.read_file(path).unwrap();
        let contents = std::str::from_utf8(&contents).unwrap();
        assert_eq!(contents, expected_contents);
    }

    fn make_test_package_and_components(outdir: &Utf8Path) {
        // We're mocking the component compiler but we are
        // using the real packaging library.
        // Write the contents of the package.
        let file1 = outdir.join("file1");
        std::fs::write(file1, "file1 contents").unwrap();
        // Write the expected output component files since the component
        // compiler is mocked.
        let component1_dir = outdir.join("foo/component1");
        let component2_dir = outdir.join("foo/component2");
        std::fs::create_dir_all(&component1_dir).unwrap();
        std::fs::create_dir_all(&component2_dir).unwrap();
        std::fs::write(component1_dir.join("component1.cm"), "component fake contents").unwrap();
        std::fs::write(component2_dir.join("component2.cm"), "component fake contents").unwrap();
    }
}
