// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    crate::{repo_keys::RepoKeys, repository::PmRepository},
    anyhow::{Context, Result},
    camino::{Utf8Path, Utf8PathBuf},
    fidl_fuchsia_pkg_ext::RepositoryKey,
    fuchsia_hash::Hash,
    fuchsia_pkg::{PackageBuilder, PackageManifest},
    fuchsia_url::RelativePackageUrl,
    futures::io::AllowStdIo,
    maplit::hashmap,
    std::{
        collections::HashSet,
        fs::{copy, create_dir, create_dir_all, File},
        path::{Path, PathBuf},
    },
    tempfile::TempDir,
    tuf::{
        crypto::{Ed25519PrivateKey, HashAlgorithm},
        metadata::{Delegation, Delegations, MetadataDescription, MetadataPath, TargetPath},
        pouf::Pouf1,
        repo_builder::RepoBuilder,
        repository::FileSystemRepositoryBuilder,
    },
    walkdir::WalkDir,
};

#[cfg(not(target_os = "fuchsia"))]
use crate::repo_client::RepoClient;
#[cfg(not(target_os = "fuchsia"))]
use crate::repository::{FileSystemRepository, RepoProvider};
#[cfg(not(target_os = "fuchsia"))]
use anyhow::anyhow;

const EMPTY_REPO_PATH: &str = concat!(env!("ROOT_OUT_DIR"), "/test_data/ffx_lib_pkg/empty-repo");

#[cfg(not(target_os = "fuchsia"))]
#[cfg(test)]
pub(crate) const PKG1_HASH: &str =
    "2881455493b5870aaea36537d70a2adc635f516ac2092598f4b6056dabc6b25d";

#[cfg(not(target_os = "fuchsia"))]
#[cfg(test)]
pub(crate) const PKG2_HASH: &str =
    "050907f009ff634f9aa57bff541fb9e9c2c62b587c23578e77637cda3bd69458";

#[cfg(not(target_os = "fuchsia"))]
#[cfg(test)]
pub(crate) const PKG1_BIN_HASH: &str =
    "72e1e7a504f32edf4f23e7e8a3542c1d77d12541142261cfe272decfa75f542d";

#[cfg(not(target_os = "fuchsia"))]
#[cfg(test)]
pub(crate) const PKG1_LIB_HASH: &str =
    "8a8a5f07f935a4e8e1fd1a1eda39da09bb2438ec0adfb149679ddd6e7e1fbb4f";

#[cfg(not(target_os = "fuchsia"))]
#[cfg(test)]
pub(crate) const PKG2_BIN_HASH: &str =
    "548981eb310ddc4098fb5c63692e19ac4ae287b13d0e911fbd9f7819ac22491c";

#[cfg(not(target_os = "fuchsia"))]
#[cfg(test)]
pub(crate) const PKG2_LIB_HASH: &str =
    "ecc11f7f4b763c5a21be2b4159c9818bbe22ca7e6d8100a72f6a41d3d7b827a9";

#[cfg(not(target_os = "fuchsia"))]
#[cfg(test)]
pub(crate) const ANONSUBPKG_HASH: &str =
    "1d20cc5163f35cd977cf414d54f8d5b53bcb33944cff0b9560b0627a6bba7441";

#[cfg(not(target_os = "fuchsia"))]
#[cfg(test)]
pub(crate) const ANONSUBPKG_BIN_HASH: &str =
    "7fb2c78b2ae5ce6b591c4b017b382e5d4f1dc2a6f3e6977cac735632bbffec1f";

#[cfg(not(target_os = "fuchsia"))]
#[cfg(test)]
pub(crate) const ANONSUBPKG_LIB_HASH: &str =
    "8dda14b0bc837a825c71a8534e402b5c7d4dfbb7348f429c017e57d547be80df";

#[cfg(not(target_os = "fuchsia"))]
#[cfg(test)]
pub(crate) const NAMEDSUBPKG_HASH: &str =
    "1e67965c0601afbe00e43eb5c50509813400f1ae0ec838b44dfa018a8afcdeac";

#[cfg(not(target_os = "fuchsia"))]
#[cfg(test)]
pub(crate) const NAMEDSUBPKG_BIN_HASH: &str =
    "5c8e2b5f4f5be036f9694842767a226c776c89e21bf6705e89d03ab4543fea2e";

#[cfg(not(target_os = "fuchsia"))]
#[cfg(test)]
pub(crate) const NAMEDSUBPKG_LIB_HASH: &str =
    "208995a7397ea03e5b567a966690336d3c2cbbbf03bea95a4ee95724ac42f2e6";

#[cfg(not(target_os = "fuchsia"))]
#[cfg(test)]
pub(crate) const SUPERPKG_HASH: &str =
    "cb174413bb34d960976f6b82ef6e6d3d3a36ddccdec142faf3d79f76c4baa676";

#[cfg(not(target_os = "fuchsia"))]
#[cfg(test)]
pub(crate) const SUPERPKG_BIN_HASH: &str =
    "5ece8a1e67d2f16b861ad282667760cda93684b5ab5c80c4be6b9be47395b2b1";

#[cfg(not(target_os = "fuchsia"))]
#[cfg(test)]
pub(crate) const SUPERPKG_LIB_HASH: &str =
    "b54bb3adf0fc9492d5e12cce9cf0b3dc501075397e8eedb77991503139c1ad40";

pub fn repo_key() -> RepositoryKey {
    RepositoryKey::Ed25519(
        [
            29, 76, 86, 76, 184, 70, 108, 73, 249, 127, 4, 47, 95, 63, 36, 35, 101, 255, 212, 33,
            10, 154, 26, 130, 117, 157, 125, 88, 175, 214, 109, 113,
        ]
        .to_vec(),
    )
}

pub fn repo_private_key() -> Ed25519PrivateKey {
    Ed25519PrivateKey::from_ed25519(&[
        80, 121, 161, 145, 5, 165, 178, 98, 248, 146, 132, 195, 60, 32, 72, 122, 150, 223, 124,
        216, 217, 43, 74, 9, 221, 38, 156, 113, 181, 63, 234, 98, 190, 11, 152, 63, 115, 150, 218,
        103, 92, 64, 198, 185, 62, 71, 252, 237, 124, 30, 158, 168, 163, 42, 31, 233, 82, 186, 143,
        81, 151, 96, 179, 7,
    ])
    .unwrap()
}

fn copy_dir(from: &Path, to: &Path) -> Result<()> {
    let walker = WalkDir::new(from);
    for entry in walker.into_iter() {
        let entry = entry?;
        let to_path = to.join(entry.path().strip_prefix(from)?);
        if entry.metadata()?.is_dir() {
            if to_path.exists() {
                continue;
            } else {
                create_dir_all(&to_path).with_context(|| format!("creating {to_path:?}"))?;
            }
        } else {
            copy(entry.path(), &to_path)
                .with_context(|| format!("copying {:?} to {:?}", entry.path(), to_path))?;
        }
    }

    Ok(())
}

pub fn make_repo_keys() -> RepoKeys {
    let keys_dir = Utf8PathBuf::from(EMPTY_REPO_PATH).join("keys");
    let repo_keys = RepoKeys::from_dir(keys_dir.as_std_path()).unwrap();

    assert_eq!(repo_keys.root_keys().len(), 1);
    assert_eq!(repo_keys.targets_keys().len(), 1);
    assert_eq!(repo_keys.snapshot_keys().len(), 1);
    assert_eq!(repo_keys.timestamp_keys().len(), 1);

    repo_keys
}

pub fn make_repo_keys_dir(root: &Utf8Path) {
    let src = PathBuf::from(EMPTY_REPO_PATH).canonicalize().unwrap();
    copy_dir(&src.join("keys"), root.as_std_path()).unwrap();
}

pub fn make_empty_pm_repo_dir(root: &Utf8Path) {
    let src = PathBuf::from(EMPTY_REPO_PATH).canonicalize().unwrap();
    copy_dir(&src, root.as_std_path()).unwrap();
}

#[cfg(not(target_os = "fuchsia"))]
pub async fn make_readonly_empty_repository() -> Result<RepoClient<Box<dyn RepoProvider>>> {
    let backend = PmRepository::new(Utf8PathBuf::from(EMPTY_REPO_PATH));
    let mut client = RepoClient::from_trusted_remote(Box::new(backend) as Box<_>)
        .await
        .map_err(|e| anyhow!(e))?;
    client.update().await?;
    Ok(client)
}

#[cfg(not(target_os = "fuchsia"))]
pub async fn make_writable_empty_repository(
    root: Utf8PathBuf,
) -> Result<RepoClient<Box<dyn RepoProvider>>> {
    make_empty_pm_repo_dir(&root);
    let backend = PmRepository::new(root);
    let mut client = RepoClient::from_trusted_remote(Box::new(backend) as Box<_>).await?;
    client.update().await?;
    Ok(client)
}

pub fn make_package_manifest(
    name: &str,
    build_path: &Path,
    subpackages: Vec<(RelativePackageUrl, Hash, PathBuf)>,
) -> (PathBuf, PackageManifest) {
    make_package_manifest_with_abi_revision(
        name,
        build_path,
        subpackages,
        // ABI revision for API level 7
        0xECCEA2F70ACD6FC0.into(),
    )
}

pub fn make_package_manifest_with_abi_revision(
    name: &str,
    build_path: &Path,
    subpackages: Vec<(RelativePackageUrl, Hash, PathBuf)>,
    abi_revision: version_history::AbiRevision,
) -> (PathBuf, PackageManifest) {
    let package_path = build_path.join(name);

    let mut builder = PackageBuilder::new(name, abi_revision);

    builder
        .add_contents_as_blob(
            format!("bin/{name}"),
            format!("binary {name}").as_bytes(),
            &package_path,
        )
        .unwrap();
    builder
        .add_contents_as_blob(
            format!("lib/{name}"),
            format!("lib {name}").as_bytes(),
            &package_path,
        )
        .unwrap();
    builder
        .add_contents_to_far(
            format!("meta/{name}.cm"),
            format!("cm {name}").as_bytes(),
            &package_path,
        )
        .unwrap();
    builder
        .add_contents_to_far(
            format!("meta/{name}.cmx"),
            format!("cmx {name}").as_bytes(),
            &package_path,
        )
        .unwrap();

    for (name, hash, manifest_path) in subpackages {
        builder.add_subpackage(&name, hash, manifest_path).unwrap();
    }

    let meta_far_path = package_path.join("meta.far");
    let manifest = builder.build(&package_path, &meta_far_path).unwrap();

    (meta_far_path, manifest)
}

pub async fn make_package_archive(archive_name: &str, outdir: &Path) -> Utf8PathBuf {
    let build_path = TempDir::new().unwrap();
    let package_path = build_path.path().join(archive_name);
    let (_, manifest) = make_package_manifest(archive_name, build_path.path(), Vec::new());
    let far_file = format!("{archive_name}.far");

    let archive_path = outdir.join(&far_file);
    let archive_file = File::create(archive_path.clone()).unwrap();
    manifest.archive(&package_path, &archive_file).await.unwrap();
    Utf8PathBuf::from_path_buf(archive_path).expect("convert archive pathbuf to utf8pathbuf")
}

pub async fn make_pm_repo_dir(repo_dir: &Path) {
    let keys_dir = repo_dir.join("keys");
    create_dir_all(&keys_dir).unwrap();
    copy_dir(Utf8PathBuf::from(EMPTY_REPO_PATH).join("keys").as_std_path(), &keys_dir).unwrap();

    let metadata_dir = repo_dir.join("repository");
    let blobs_dir = metadata_dir.join("blobs");
    make_repo_dir(&metadata_dir, &blobs_dir).await;
}

pub async fn make_repo_dir(metadata_dir: &Path, blobs_dir: &Path) {
    create_dir_all(metadata_dir).unwrap();
    create_dir_all(blobs_dir).unwrap();

    // Construct some packages for the repository.
    let build_tmp = tempfile::tempdir().unwrap();
    let build_path = build_tmp.path();

    let packages = ["package1", "package2"].map(|name| {
        let (meta_far_path, manifest) = make_package_manifest(name, build_path, Vec::new());

        // Copy the package blobs into the blobs directory.
        let mut meta_far_merkle = None;
        for blob in manifest.blobs() {
            let merkle = blob.merkle.to_string();

            if blob.path == "meta/" {
                meta_far_merkle = Some(merkle.clone());
            }

            let mut src = std::fs::File::open(&blob.source_path).unwrap();
            let mut dst = std::fs::File::create(blobs_dir.join(merkle)).unwrap();
            std::io::copy(&mut src, &mut dst).unwrap();
        }

        (name, meta_far_path, meta_far_merkle.unwrap())
    });

    // Write TUF metadata
    let repo =
        FileSystemRepositoryBuilder::<Pouf1>::new(metadata_dir).targets_prefix("targets").build();

    let repo_keys = make_repo_keys();
    let root_keys = repo_keys.root_keys().iter().map(|k| &**k).collect::<Vec<_>>();
    let targets_keys = repo_keys.targets_keys().iter().map(|k| &**k).collect::<Vec<_>>();
    let snapshot_keys = repo_keys.snapshot_keys().iter().map(|k| &**k).collect::<Vec<_>>();
    let timestamp_keys = repo_keys.timestamp_keys().iter().map(|k| &**k).collect::<Vec<_>>();

    let mut builder = RepoBuilder::create(repo)
        .trusted_root_keys(&root_keys)
        .trusted_targets_keys(&targets_keys)
        .trusted_snapshot_keys(&snapshot_keys)
        .trusted_timestamp_keys(&timestamp_keys)
        .stage_root()
        .unwrap();

    // Add all the packages to the metadata.
    for (name, meta_far_path, meta_far_merkle) in packages {
        builder = builder
            .add_target_with_custom(
                TargetPath::new(format!("{name}/0")).unwrap(),
                AllowStdIo::new(File::open(meta_far_path).unwrap()),
                hashmap! { "merkle".into() => meta_far_merkle.into() },
            )
            .await
            .unwrap();
    }

    // Even though we don't use delegations, add a simple one to make sure we at least preserve them
    // when we modify repositories.
    let delegations_keys = targets_keys.clone();
    let delegations = Delegations::new(
        delegations_keys
            .iter()
            .map(|k| (k.public().key_id().clone(), k.public().clone()))
            .collect(),
        vec![Delegation::new(
            MetadataPath::new("delegation").unwrap(),
            false,
            1,
            delegations_keys.iter().map(|k| k.public().key_id().clone()).collect(),
            HashSet::from([TargetPath::new("some-delegated-target").unwrap()]),
        )
        .unwrap()],
    )
    .unwrap();

    builder
        .stage_targets_with_builder(|b| b.delegations(delegations))
        .unwrap()
        .stage_snapshot_with_builder(|b| {
            b.insert_metadata_description(
                MetadataPath::new("delegation").unwrap(),
                MetadataDescription::from_slice(&[0u8], 1, &[HashAlgorithm::Sha256]).unwrap(),
            )
        })
        .unwrap()
        .commit()
        .await
        .unwrap();
}

pub async fn make_pm_repository(dir: impl Into<Utf8PathBuf>) -> PmRepository {
    let dir = dir.into();
    let metadata_dir = dir.join("repository");
    let blobs_dir = metadata_dir.join("blobs");
    make_repo_dir(metadata_dir.as_std_path(), blobs_dir.as_std_path()).await;

    let keys_dir = dir.join("keys");
    create_dir(&keys_dir).unwrap();

    let empty_repo_dir = PathBuf::from(EMPTY_REPO_PATH).canonicalize().unwrap();
    copy_dir(&empty_repo_dir.join("keys"), keys_dir.as_std_path()).unwrap();

    PmRepository::new(dir)
}

#[cfg(not(target_os = "fuchsia"))]
pub async fn make_file_system_repository(
    metadata_dir: impl Into<Utf8PathBuf>,
    blobs_dir: impl Into<Utf8PathBuf>,
) -> RepoClient<Box<dyn RepoProvider>> {
    let metadata_dir = metadata_dir.into();
    let blobs_dir = blobs_dir.into();
    make_repo_dir(metadata_dir.as_std_path(), blobs_dir.as_std_path()).await;

    let backend = FileSystemRepository::new(metadata_dir, blobs_dir);
    let mut client = RepoClient::from_trusted_remote(Box::new(backend) as Box<_>).await.unwrap();
    client.update().await.unwrap();
    client
}
