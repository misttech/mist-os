// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    crate::{
        args::{PackageArchiveCreateCommand, PackageArchiveExtractCommand},
        to_writer_json_pretty, BLOBS_JSON_NAME, META_FAR_MERKLE_NAME, PACKAGE_MANIFEST_NAME,
    },
    anyhow::{Context as _, Result},
    fuchsia_pkg::PackageManifest,
    std::fs::File,
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
        .archive(cmd.root_dir, output)
        .await
        .with_context(|| format!("writing archive {}", cmd.out.display()))?;

    Ok(())
}

pub async fn cmd_package_archive_extract(cmd: PackageArchiveExtractCommand) -> Result<()> {
    let blobs_dir = cmd.out.join("blobs");

    std::fs::create_dir_all(&blobs_dir)
        .with_context(|| format!("creating directory {}", blobs_dir.display()))?;

    let mut package_manifest = PackageManifest::from_archive(&cmd.archive, &blobs_dir)
        .with_context(|| format!("extracting package manifest {}", cmd.archive.display()))?;

    if let Some(repository) = cmd.repository {
        package_manifest.set_repository(Some(repository));
    }

    let package_manifest_path = cmd.out.join(PACKAGE_MANIFEST_NAME);

    let file = File::create(&package_manifest_path)
        .with_context(|| format!("creating {}", package_manifest_path.display()))?;

    to_writer_json_pretty(file, &package_manifest)?;

    // FIXME(fxbug.dev/101304): Write out the meta.far.merkle file, that contains the meta.far
    // merkle.
    if cmd.meta_far_merkle {
        std::fs::write(
            cmd.out.join(META_FAR_MERKLE_NAME),
            package_manifest.hash().to_string().as_bytes(),
        )?;
    }

    // FIXME(fxbug.dev/101304): Some tools still depend on the legacy `blobs.json` file. We
    // should migrate them over to using `package_manifest.json` so we can stop producing this file.
    if cmd.blobs_json {
        let blobs_json_path = cmd.out.join(BLOBS_JSON_NAME);
        let file = File::create(&blobs_json_path)
            .with_context(|| format!("creating {}", blobs_json_path.display()))?;
        to_writer_json_pretty(file, package_manifest.blobs())?;
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use {
        super::*,
        camino::{Utf8Path, Utf8PathBuf},
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
        metafar_contents: Vec<u8>,
    }

    fn create_package(pkg_dir: &Utf8Path) -> Package {
        let mut builder = PackageBuilder::new("some_pkg_name");
        builder.add_contents_as_blob("bin", BIN_CONTENTS, pkg_dir).unwrap();
        builder.add_contents_as_blob("lib", LIB_CONTENTS, pkg_dir).unwrap();

        // Build the package.
        let metafar_path = pkg_dir.join("meta.far");
        let manifest = builder.build(pkg_dir, &metafar_path).unwrap();
        let metafar_contents = std::fs::read(&metafar_path).unwrap();

        let manifest_path = pkg_dir.join(PACKAGE_MANIFEST_NAME);

        serde_json::to_writer(std::fs::File::create(&manifest_path).unwrap(), &manifest).unwrap();

        Package { manifest_path, metafar_contents }
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
        meta_far_path: &Utf8Path,
    ) -> BTreeMap<Utf8PathBuf, Vec<u8>> {
        let manifest_path = extract_dir.join("package_manifest.json");
        let bin_path = extract_dir.join("blobs").join(BIN_HASH);
        let lib_path = extract_dir.join("blobs").join(LIB_HASH);

        // Read the extracted files.
        let mut extract_contents = read_dir(extract_dir);

        // Check the extracted manifest is correct.
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(
                &extract_contents.remove(&manifest_path).unwrap()
            )
            .unwrap(),
            serde_json::json!({
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
            })
        );

        // Check that the meta.far is right.
        assert_eq!(extract_contents.remove(meta_far_path).unwrap(), package.metafar_contents,);

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
        })
        .await
        .unwrap();

        // Read the generated archive file.
        let mut archive = Utf8Reader::new(File::open(&archive_path).unwrap()).unwrap();

        assert_eq!(
            read_archive(&mut archive),
            BTreeMap::from([
                ("meta.far".to_string(), package.metafar_contents.clone()),
                (BIN_HASH.to_string(), BIN_CONTENTS.to_vec()),
                (LIB_HASH.to_string(), LIB_CONTENTS.to_vec()),
            ]),
        );

        // Extract the archive.
        let extract_dir = root.join("extract");
        cmd_package_archive_extract(PackageArchiveExtractCommand {
            out: extract_dir.clone().into(),
            repository: None,
            archive: archive_path.clone().into_std_path_buf(),
            meta_far_merkle: true,
            blobs_json: true,
        })
        .await
        .unwrap();

        let blobs_dir = extract_dir.join("blobs");
        let meta_far_path = blobs_dir.join(META_FAR_HASH);
        let mut extract_contents = check_extract(&package, &extract_dir, &meta_far_path);

        assert_eq!(
            &extract_contents.remove(&extract_dir.join(META_FAR_MERKLE_NAME)).unwrap(),
            META_FAR_HASH.as_bytes()
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
        let mut extract_contents = check_extract(&package, &extract_dir, &meta_far_path);

        assert_eq!(
            &extract_contents.remove(&extract_dir.join(META_FAR_MERKLE_NAME)).unwrap(),
            META_FAR_HASH.as_bytes()
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
            out: extract_dir.clone().into(),
            repository: None,
            meta_far_merkle: false,
            blobs_json: false,
            archive: archive_path.clone().into_std_path_buf(),
        })
        .await
        .unwrap();

        let extract_contents =
            check_extract(&package, &extract_dir, &extract_dir.join("blobs").join(META_FAR_HASH));
        assert_eq!(extract_contents, BTreeMap::from([]));
    }
}
