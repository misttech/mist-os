// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    anyhow::Result,
    errors::{ffx_bail, ffx_error},
    ffx_core::ffx_plugin,
    ffx_repository_add_args::AddCommand,
    fidl_fuchsia_developer_ffx::RepositoryRegistryProxy,
    fidl_fuchsia_developer_ffx_ext::RepositoryError,
    fuchsia_repo::repository::RepoProvider,
    fuchsia_url::RepositoryUrl,
    sdk_metadata::get_repositories,
};

#[ffx_plugin("ffx-repo-add", RepositoryRegistryProxy = "daemon::protocol")]
pub async fn add_from_product(cmd: AddCommand, repos: RepositoryRegistryProxy) -> Result<()> {
    if cmd.prefix.is_empty() {
        ffx_bail!("name cannot be empty");
    }
    let repositories = get_repositories(cmd.product_bundle_dir)?;
    for repository in repositories {
        // Validate that we can construct a valid repository url from the name.
        let repo_alias = repository.aliases().first().unwrap();
        let repo_url = RepositoryUrl::parse_host(format!("{}.{}", cmd.prefix, &repo_alias))
            .map_err(|err| {
                ffx_error!(
                    "invalid repository name for {:?} {:?}: {}",
                    cmd.prefix,
                    &repo_alias,
                    err
                )
            })?;

        let repo_name = repo_url.host();

        match repos.add_repository(repo_name, &repository.spec().into()).await? {
            Ok(()) => {
                println!("added repository {}", repo_name);
            }
            Err(err) => {
                let err = RepositoryError::from(err);
                ffx_bail!("Adding repository {} failed: {}", repo_name, err);
            }
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use {
        super::*,
        assembly_partitions_config::PartitionsConfig,
        assert_matches::assert_matches,
        camino::Utf8Path,
        fidl_fuchsia_developer_ffx::RepositoryRegistryMarker,
        fidl_fuchsia_developer_ffx::{
            FileSystemRepositorySpec, RepositoryRegistryRequest, RepositorySpec,
        },
        fuchsia_async as fasync,
        futures::{channel::mpsc, SinkExt as _, StreamExt as _, TryStreamExt as _},
        pretty_assertions::assert_eq,
        sdk_metadata::{ProductBundle, ProductBundleV2, Repository},
    };

    #[fasync::run_singlethreaded(test)]
    async fn test_add_from_product() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = Utf8Path::from_path(tmp.path()).unwrap().canonicalize_utf8().unwrap();

        let (mut sender, receiver) = mpsc::unbounded();

        let (repos, mut stream) =
            fidl::endpoints::create_proxy_and_stream::<RepositoryRegistryMarker>().unwrap();

        let task = fuchsia_async::Task::local(async move {
            while let Ok(Some(req)) = stream.try_next().await {
                match req {
                    RepositoryRegistryRequest::AddRepository { name, repository, responder } => {
                        sender.send((name, repository)).await.unwrap();
                        responder.send(&mut Ok(())).unwrap();
                    }
                    other => panic!("Unexpected request: {:?}", other),
                }
            }
        });

        let blobs_dir = dir.join("blobs");
        let fuchsia_metadata_dir = dir.join("fuchsia");
        let example_metadata_dir = dir.join("example");

        let pb = ProductBundle::V2(ProductBundleV2 {
            product_name: "test".into(),
            product_version: "test-product-version".into(),
            partitions: PartitionsConfig::default(),
            sdk_version: "test-sdk-version".into(),
            system_a: None,
            system_b: None,
            system_r: None,
            repositories: vec![
                Repository {
                    name: "fuchsia.com".into(),
                    metadata_path: fuchsia_metadata_dir.clone(),
                    blobs_path: blobs_dir.clone(),
                    delivery_blob_type: None,
                    root_private_key_path: None,
                    targets_private_key_path: None,
                    snapshot_private_key_path: None,
                    timestamp_private_key_path: None,
                },
                Repository {
                    name: "example.com".into(),
                    metadata_path: example_metadata_dir.clone(),
                    blobs_path: blobs_dir.clone(),
                    delivery_blob_type: None,
                    root_private_key_path: None,
                    targets_private_key_path: None,
                    snapshot_private_key_path: None,
                    timestamp_private_key_path: None,
                },
            ],
            update_package_hash: None,
            virtual_devices_path: None,
        });
        pb.write(&dir).unwrap();

        add_from_product(
            AddCommand { prefix: "my-repo".to_owned(), product_bundle_dir: dir.to_path_buf() },
            repos,
        )
        .await
        .unwrap();

        // Drop the task so the channel will close.
        drop(task);

        assert_eq!(
            receiver.collect::<Vec<_>>().await,
            vec![
                (
                    "my-repo.fuchsia.com".to_owned(),
                    RepositorySpec::FileSystem(FileSystemRepositorySpec {
                        metadata_repo_path: Some(fuchsia_metadata_dir.to_string()),
                        blob_repo_path: Some(blobs_dir.to_string()),
                        aliases: Some(vec!["fuchsia.com".into()]),
                        ..Default::default()
                    })
                ),
                (
                    "my-repo.example.com".to_owned(),
                    RepositorySpec::FileSystem(FileSystemRepositorySpec {
                        metadata_repo_path: Some(example_metadata_dir.to_string()),
                        blob_repo_path: Some(blobs_dir.to_string()),
                        aliases: Some(vec!["example.com".into()]),
                        ..Default::default()
                    })
                ),
            ]
        );
    }

    #[fasync::run_singlethreaded(test)]
    async fn test_add_from_product_rejects_invalid_names() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = Utf8Path::from_path(tmp.path()).unwrap();

        let blobs_dir = dir.join("blobs");
        let fuchsia_metadata_dir = dir.join("fuchsia");
        let example_metadata_dir = dir.join("example");

        let pb = ProductBundle::V2(ProductBundleV2 {
            product_name: "test".into(),
            product_version: "test-product-version".into(),
            partitions: PartitionsConfig::default(),
            sdk_version: "test-sdk-version".into(),
            system_a: None,
            system_b: None,
            system_r: None,
            repositories: vec![
                Repository {
                    name: "fuchsia.com".into(),
                    metadata_path: fuchsia_metadata_dir.clone(),
                    blobs_path: blobs_dir.clone(),
                    delivery_blob_type: None,
                    root_private_key_path: None,
                    targets_private_key_path: None,
                    snapshot_private_key_path: None,
                    timestamp_private_key_path: None,
                },
                Repository {
                    name: "example.com".into(),
                    metadata_path: example_metadata_dir.clone(),
                    blobs_path: blobs_dir.clone(),
                    delivery_blob_type: None,
                    root_private_key_path: None,
                    targets_private_key_path: None,
                    snapshot_private_key_path: None,
                    timestamp_private_key_path: None,
                },
            ],
            update_package_hash: None,
            virtual_devices_path: None,
        });
        pb.write(&dir).unwrap();

        let repos =
            setup_fake_repos(move |req| panic!("should not receive any requests: {:?}", req));

        for prefix in ["", "my_repo", "MyRepo", "😀"] {
            assert_matches!(
                add_from_product(
                    AddCommand { prefix: prefix.to_owned(), product_bundle_dir: dir.to_path_buf() },
                    repos.clone(),
                )
                .await,
                Err(_)
            );
        }
    }
}
