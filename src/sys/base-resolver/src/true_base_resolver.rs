// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    anyhow::{self, Context as _},
    fidl_fuchsia_boot as fboot, fidl_fuchsia_component_resolution as fcomponent_resolution,
    fidl_fuchsia_pkg as fpkg,
    futures::{future::TryFutureExt as _, stream::StreamExt as _},
    std::collections::HashMap,
    tracing::error,
};

mod component;
mod package;

pub(crate) async fn main() -> anyhow::Result<()> {
    tracing::info!("started");

    let blobfs =
        blobfs::Client::open_from_namespace_executable().context("failed to open /blob")?;
    let base_packages = determine_base_packages(
        &blobfs,
        &fuchsia_component::client::connect_to_protocol::<fboot::ArgumentsMarker>()
            .context("failed to connect to fuchsia.boot/Arguments")?,
    )
    .await
    .context("determine base packages")?;
    let authenticator = crate::context_authenticator::ContextAuthenticator::new();

    let mut service_fs = fuchsia_component::server::ServiceFs::new_local();
    service_fs.dir("svc").add_fidl_service(Services::PackageResolver);
    service_fs.dir("svc").add_fidl_service(Services::ComponentResolver);
    service_fs.take_and_serve_directory_handle().context("failed to serve outgoing namespace")?;
    let () = service_fs
        .for_each_concurrent(None, |request| async {
            match request {
                Services::PackageResolver(stream) => {
                    package::serve_request_stream(
                        stream,
                        &base_packages,
                        authenticator.clone(),
                        &blobfs,
                    )
                    .unwrap_or_else(|e| error!("failed to serve package resolver request: {:#}", e))
                    .await
                }
                Services::ComponentResolver(stream) => {
                    component::serve_request_stream(
                        stream,
                        &base_packages,
                        authenticator.clone(),
                        &blobfs,
                    )
                    .unwrap_or_else(|e| {
                        error!("failed to serve component resolver request: {:#}", e)
                    })
                    .await
                }
            }
        })
        .await;

    Ok(())
}

/// Panics if a base package has a non-zero variant.
/// The URLs in the returned map will not have a variant.
async fn determine_base_packages(
    blobfs: &blobfs::Client,
    boot_args: &fboot::ArgumentsProxy,
) -> anyhow::Result<HashMap<fuchsia_url::UnpinnedAbsolutePackageUrl, fuchsia_hash::Hash>> {
    let system_image = system_image::SystemImage::new(blobfs.clone(), boot_args)
        .await
        .context("failed to load system_image package")?;
    let static_packages =
        system_image.static_packages().await.context("failed to determine static packages")?;
    let base_repo = fuchsia_url::RepositoryUrl::parse_host("fuchsia.com".into())
        .expect("valid repository hostname");
    Ok(HashMap::from_iter(
        static_packages
            .into_contents()
            .chain([(system_image::SystemImage::package_path(), *system_image.hash())])
            .map(|(path, hash)| {
                let (name, variant) = path.into_name_and_variant();
                // TODO(fxbug.dev/53911) Remove variant checks when variant concept is deleted.
                if !variant.is_zero() {
                    panic!("base package variants must be zero: {name} {variant}");
                }
                (fuchsia_url::UnpinnedAbsolutePackageUrl::new(base_repo.clone(), name, None), hash)
            }),
    ))
}

enum Services {
    PackageResolver(fpkg::PackageResolverRequestStream),
    ComponentResolver(fcomponent_resolution::ResolverRequestStream),
}
