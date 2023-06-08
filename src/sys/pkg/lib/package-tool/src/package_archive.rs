// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    crate::{
        args::{PackageArchiveCreateCommand, PackageArchiveExtractCommand},
        to_writer_json_pretty, write_depfile, BLOBS_JSON_NAME, PACKAGE_MANIFEST_NAME,
    },
    anyhow::{Context as _, Result},
    camino::Utf8Path,
    fuchsia_pkg::{PackageManifest, SubpackageInfo},
    std::{collections::BTreeSet, fs::File},
};

pub async fn cmd_package_archive_create(cmd: PackageArchiveCreateCommand) -> Result<()> {
    let package_manifest = PackageManifest::try_load_from(&cmd.package_manifest)
        .with_context(|| format!("opening {}", cmd.package_manifest))?;

    if let Some(parent) = cmd.out.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating directory {}", parent.display()))?;
    }

    let output = File::create(&cmd.out)
        .with_context(|| format!("creating package archive file {}", cmd.out.display()))?;

    package_manifest
        .clone()
        .archive(cmd.root_dir, output)
        .await
        .with_context(|| format!("writing archive {}", cmd.out.display()))?;

    if let Some(depfile_path) = &cmd.depfile {
        let mut dep_paths: BTreeSet<String> = BTreeSet::<String>::new();

        // Getting top-level blobs
        for blob in package_manifest.blobs() {
            dep_paths.insert(blob.source_path.clone());
        }

        // Recursively checking each Subpackage, building `dep_paths` as we go.
        let mut visited_subpackages: BTreeSet<fuchsia_merkle::Hash> =
            BTreeSet::<fuchsia_merkle::Hash>::new();
        let mut subpackage_list: Vec<SubpackageInfo> = package_manifest.subpackages().to_vec();

        while let Some(subpackage) = subpackage_list.pop() {
            // If subpackage already seen, skip.
            if visited_subpackages.contains(&subpackage.merkle) {
                continue;
            }
            visited_subpackages.insert(subpackage.merkle);

            dep_paths.insert(subpackage.manifest_path.clone());

            let subpackage_manifest = PackageManifest::try_load_from(&subpackage.manifest_path)
                .with_context(|| format!("opening {}", &subpackage.manifest_path))?;

            // Gathering subpackage blobs.
            for subpackage_blob in subpackage_manifest.blobs() {
                dep_paths.insert(subpackage_blob.source_path.clone());
            }

            // Gathering possible additional subpackages.
            for inner_subpackage in subpackage_manifest.subpackages() {
                subpackage_list.push(inner_subpackage.clone());
            }
        }

        let far_path = Utf8Path::from_path(cmd.out.as_path()).unwrap();

        write_depfile(depfile_path.as_std_path(), far_path, dep_paths.into_iter())?;
    }

    Ok(())
}

pub async fn cmd_package_archive_extract(cmd: PackageArchiveExtractCommand) -> Result<()> {
    let blobs_dir = cmd.out.join("blobs");

    std::fs::create_dir_all(&blobs_dir)
        .with_context(|| format!("creating directory {blobs_dir}"))?;

    let manifests_dir = cmd.out.join("manifests");

    std::fs::create_dir_all(&manifests_dir)
        .with_context(|| format!("creating directory {manifests_dir}"))?;

    let mut package_manifest = PackageManifest::from_archive(
        &cmd.archive,
        blobs_dir.as_std_path(),
        manifests_dir.as_std_path(),
    )
    .with_context(|| format!("extracting package manifest {}", cmd.archive.display()))?;

    if let Some(repository) = cmd.repository {
        package_manifest.set_repository(Some(repository));
    }

    let package_manifest_path = cmd.out.join(PACKAGE_MANIFEST_NAME);
    let package_manifest = package_manifest
        .write_with_relative_paths(&package_manifest_path)
        .with_context(|| format!("creating {package_manifest_path}"))?;

    // FIXME(fxbug.dev/101304): Some tools still depend on the legacy `blobs.json` file. We
    // should migrate them over to using `package_manifest.json` so we can stop producing this file.
    if cmd.blobs_json {
        let blobs_json_path = cmd.out.join(BLOBS_JSON_NAME);
        let file = File::create(&blobs_json_path)
            .with_context(|| format!("creating {blobs_json_path}"))?;
        to_writer_json_pretty(file, package_manifest.blobs())?;
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use {
        super::*,
        crate::convert_to_depfile_filepath,
        camino::Utf8PathBuf,
        fuchsia_archive::Utf8Reader,
        fuchsia_pkg::PackageBuilder,
        pretty_assertions::assert_eq,
        std::{collections::BTreeMap, io::Write, process::Command},
        tempfile::TempDir,
    };

    const PM_BIN: &str = "host_x64/test_data/package-tool/pm";

    const BIN_CONTENTS: &[u8] = b"bin";
    const LIB_CONTENTS: &[u8] = b"lib";

    const META_FAR_HASH: &str = "f1a91cbbd41fef65416522a9de7e1d8be0f962ec6371cb747a403cff03d656e6";
    const BIN_HASH: &str = "5d202ed772f4de29ecd7bc9a3f20278cd69ae160e36ba8b434512ca45003c7a3";
    const LIB_HASH: &str = "65f1e8f09fdc18cbcfc8f2a472480643761478595e891138de8055442dcc3233";

    struct Package {
        manifest_path: Utf8PathBuf,
        meta_far_contents: Vec<u8>,
    }

    fn create_package(pkg_dir: &Utf8Path) -> Package {
        let mut builder = PackageBuilder::new("some_pkg_name");
        builder.abi_revision(0x406C7CA7EF077DB4);
        builder.add_contents_as_blob("bin", BIN_CONTENTS, pkg_dir).unwrap();
        builder.add_contents_as_blob("lib", LIB_CONTENTS, pkg_dir).unwrap();

        // Build the package.
        let metafar_path = pkg_dir.join("meta.far");
        let manifest = builder.build(pkg_dir, &metafar_path).unwrap();
        let meta_far_contents = std::fs::read(&metafar_path).unwrap();

        let manifest_path = pkg_dir.join(PACKAGE_MANIFEST_NAME);

        serde_json::to_writer(std::fs::File::create(&manifest_path).unwrap(), &manifest).unwrap();

        Package { manifest_path, meta_far_contents }
    }

    fn read_archive(archive: &mut Utf8Reader<std::fs::File>) -> BTreeMap<String, Vec<u8>> {
        archive
            .list()
            .map(|e| e.path().to_owned())
            .collect::<Vec<_>>()
            .into_iter()
            .map(|path| {
                let contents = archive.read_file(&path).unwrap();
                (path, contents)
            })
            .collect()
    }

    fn read_dir(dir: &Utf8Path) -> BTreeMap<Utf8PathBuf, Vec<u8>> {
        walkdir::WalkDir::new(dir)
            .into_iter()
            .filter_map(|entry| {
                let entry = entry.unwrap();
                if entry.metadata().unwrap().is_file() {
                    let path = Utf8Path::from_path(entry.path()).unwrap().into();
                    let contents = std::fs::read(entry.path()).unwrap();
                    Some((path, contents))
                } else {
                    None
                }
            })
            .collect()
    }

    fn check_extract(
        package: &Package,
        extract_dir: &Utf8Path,
        blob_sources_relative: Option<&str>,
        mut meta_far_path: Utf8PathBuf,
        mut bin_path: Utf8PathBuf,
        mut lib_path: Utf8PathBuf,
    ) -> BTreeMap<Utf8PathBuf, Vec<u8>> {
        let manifest_path = extract_dir.join("package_manifest.json");

        // Read the extracted files.
        let mut extract_contents = read_dir(extract_dir);

        // Check the extracted manifest is correct.
        let mut expected = serde_json::json!({
            "version": "1",
            "package": {
                "name": "some_pkg_name",
                "version": "0"
            },
            "blobs": [
                {
                    "merkle": META_FAR_HASH,
                    "path": "meta/",
                    "size": 16384,
                    "source_path": meta_far_path,
                },
                {
                    "merkle": BIN_HASH,
                    "path": "bin",
                    "size": 3,
                    "source_path": bin_path,
                },
                {
                    "merkle": LIB_HASH,
                    "path": "lib",
                    "size": 3,
                    "source_path": lib_path,
                },
            ]
        });

        if let Some(blob_sources_relative) = blob_sources_relative {
            expected
                .as_object_mut()
                .unwrap()
                .insert("blob_sources_relative".into(), blob_sources_relative.into());

            if blob_sources_relative == "file" {
                meta_far_path = extract_dir.join(meta_far_path);
                bin_path = extract_dir.join(bin_path);
                lib_path = extract_dir.join(lib_path);
            }
        }

        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(
                &extract_contents.remove(&manifest_path).unwrap()
            )
            .unwrap(),
            expected,
        );

        // Check that the meta.far is right.
        assert_eq!(extract_contents.remove(&meta_far_path).unwrap(), package.meta_far_contents);

        // Check the rest of the blobs are correct.
        assert_eq!(extract_contents.remove(&bin_path).unwrap(), BIN_CONTENTS);
        assert_eq!(extract_contents.remove(&lib_path).unwrap(), LIB_CONTENTS);

        extract_contents
    }

    #[fuchsia::test]
    async fn test_archive_create_and_extract() {
        let tmp = TempDir::new().unwrap();
        let root = Utf8Path::from_path(tmp.path()).unwrap();

        let pkg_dir = root.join("pkg");
        let package = create_package(&pkg_dir);

        // Write the archive.
        let archive_path = root.join("archive.far");
        cmd_package_archive_create(PackageArchiveCreateCommand {
            out: archive_path.clone().into(),
            root_dir: pkg_dir.to_owned(),
            package_manifest: package.manifest_path.clone(),
            depfile: None,
        })
        .await
        .unwrap();

        // Read the generated archive file.
        let mut archive = Utf8Reader::new(File::open(&archive_path).unwrap()).unwrap();

        assert_eq!(
            read_archive(&mut archive),
            BTreeMap::from([
                ("meta.far".to_string(), package.meta_far_contents.clone()),
                (BIN_HASH.to_string(), BIN_CONTENTS.to_vec()),
                (LIB_HASH.to_string(), LIB_CONTENTS.to_vec()),
            ]),
        );

        // Extract the archive.
        let extract_dir = root.join("extract");
        cmd_package_archive_extract(PackageArchiveExtractCommand {
            out: extract_dir.clone(),
            repository: None,
            archive: archive_path.clone().into(),
            blobs_json: true,
        })
        .await
        .unwrap();

        let mut extract_contents = check_extract(
            &package,
            &extract_dir,
            Some("file"),
            Utf8Path::new("blobs").join(META_FAR_HASH),
            Utf8Path::new("blobs").join(BIN_HASH),
            Utf8Path::new("blobs").join(LIB_HASH),
        );

        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(
                &extract_contents.remove(&extract_dir.join(BLOBS_JSON_NAME)).unwrap(),
            )
            .unwrap(),
            serde_json::json!([
                    {
                        "source_path": format!("blobs/{META_FAR_HASH}"),
                        "path": "meta/",
                        "merkle": META_FAR_HASH,
                        "size": 16384,
                    },
                    {
                        "source_path": format!("blobs/{BIN_HASH}"),
                        "path": "bin",
                        "merkle": BIN_HASH,
                        "size": 3,
                    },
                    {
                        "source_path": format!("blobs/{LIB_HASH}"),
                        "path": "lib",
                        "merkle": LIB_HASH,
                        "size": 3,
                    },
                ]
            )
        );

        assert_eq!(extract_contents, BTreeMap::new());
    }

    #[fuchsia::test]
    async fn test_package_tool_archive_and_pm_expand() {
        let tmp = TempDir::new().unwrap();
        let root = Utf8Path::from_path(tmp.path()).unwrap();

        let pkg_dir = root.join("pkg");
        let package = create_package(&pkg_dir);

        let archive_path = root.join("archive.far");
        cmd_package_archive_create(PackageArchiveCreateCommand {
            out: archive_path.clone().into(),
            root_dir: pkg_dir.to_owned(),
            package_manifest: package.manifest_path.clone(),
            depfile: None,
        })
        .await
        .unwrap();

        let extract_dir = root.join("extract");
        let status = Command::new(PM_BIN)
            .args(["-o", extract_dir.as_str(), "expand", archive_path.as_str()])
            .status()
            .unwrap();
        assert!(status.success());

        let blobs_dir = extract_dir.join("blobs");

        // Check that the `pm archive` extracts what we expect.
        let meta_far_path = extract_dir.join("meta.far");
        let mut extract_contents = check_extract(
            &package,
            &extract_dir,
            None,
            meta_far_path.clone(),
            blobs_dir.join(BIN_HASH),
            blobs_dir.join(LIB_HASH),
        );

        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(
                &extract_contents.remove(&extract_dir.join(BLOBS_JSON_NAME)).unwrap(),
            )
            .unwrap(),
            serde_json::json!([
                    {
                        "source_path": meta_far_path,
                        "path": "meta/",
                        "merkle": META_FAR_HASH,
                        "size": 16384,
                    },
                    {
                        "source_path": blobs_dir.join(BIN_HASH),
                        "path": "bin",
                        "merkle": BIN_HASH,
                        "size": 3,
                    },
                    {
                        "source_path": blobs_dir.join(LIB_HASH),
                        "path": "lib",
                        "merkle": LIB_HASH,
                        "size": 3,
                    },
                ]
            )
        );

        // We don't care about the other pm legacy files:
        // * blobs.manifest
        // * package.manifest
        extract_contents.remove(&extract_dir.join("blobs.manifest")).unwrap();
        extract_contents.remove(&extract_dir.join("package.manifest")).unwrap();
        assert_eq!(extract_contents, BTreeMap::new());
    }

    #[fuchsia::test]
    async fn test_archive_create_with_depfile() {
        let tmp = TempDir::new().unwrap();
        let root = Utf8Path::from_path(tmp.path()).unwrap();

        let pkg_dir = root.join("pkg");
        let package = create_package(&pkg_dir);
        let depfile_path = root.join("archive.far.d");

        // Write the archive.
        let archive_path = root.join("archive.far");
        cmd_package_archive_create(PackageArchiveCreateCommand {
            out: archive_path.clone().into(),
            root_dir: pkg_dir.to_owned(),
            package_manifest: package.manifest_path.clone(),
            depfile: Some(depfile_path.clone()),
        })
        .await
        .unwrap();

        // Read the generated archive file.
        let mut archive = Utf8Reader::new(File::open(&archive_path).unwrap()).unwrap();

        assert_eq!(
            read_archive(&mut archive),
            BTreeMap::from([
                ("meta.far".to_string(), package.meta_far_contents.clone()),
                (BIN_HASH.to_string(), BIN_CONTENTS.to_vec()),
                (LIB_HASH.to_string(), LIB_CONTENTS.to_vec()),
            ]),
        );

        let expected_deps = vec![
            convert_to_depfile_filepath(root.join("pkg/meta.far").as_str()),
            convert_to_depfile_filepath(root.join("pkg/bin").as_str()),
            convert_to_depfile_filepath(root.join("pkg/lib").as_str()),
        ];

        assert_eq!(
            std::fs::read_to_string(&depfile_path).unwrap(),
            format!(
                "{}: {}",
                convert_to_depfile_filepath(root.join("archive.far").as_str()),
                expected_deps
                    .iter()
                    .map(|p| p.as_str())
                    .collect::<BTreeSet<_>>()
                    .into_iter()
                    .collect::<Vec<_>>()
                    .join(" "),
            ),
        );
    }

    #[fuchsia::test]
    async fn test_pm_archive_and_package_tool_extract() {
        let tmp = TempDir::new().unwrap();
        let root = Utf8Path::from_path(tmp.path()).unwrap();

        let pkg_dir = root.join("pkg");
        std::fs::create_dir(&pkg_dir).unwrap();

        // `pm archive` also needs a FINI build manifest.
        let build_manifest_path = pkg_dir.join("build.manifest");
        let mut f = std::fs::File::create(&build_manifest_path).unwrap();
        writeln!(f, "bin={}", pkg_dir.join("bin")).unwrap();
        writeln!(f, "lib={}", pkg_dir.join("lib")).unwrap();
        drop(f);

        let package = create_package(&pkg_dir);

        let mut archive_path = root.join("archive");
        let status = Command::new(PM_BIN)
            .args([
                "-m",
                build_manifest_path.as_str(),
                "-o",
                pkg_dir.as_str(),
                "--abi-revision",
                "0x406C7CA7EF077DB4",
                "archive",
                "--output",
                archive_path.as_str(),
            ])
            .status()
            .unwrap();
        assert!(status.success());

        // pm will automatically add the suffix `.far`.
        archive_path.set_extension("far");

        // Extract the archive.
        let extract_dir = root.join("extract");
        cmd_package_archive_extract(PackageArchiveExtractCommand {
            out: extract_dir.clone(),
            repository: None,
            blobs_json: false,
            archive: archive_path.clone().into_std_path_buf(),
        })
        .await
        .unwrap();

        let extract_contents = check_extract(
            &package,
            &extract_dir,
            Some("file"),
            Utf8Path::new("blobs").join(META_FAR_HASH),
            Utf8Path::new("blobs").join(BIN_HASH),
            Utf8Path::new("blobs").join(LIB_HASH),
        );
        assert_eq!(extract_contents, BTreeMap::from([]));
    }
}
