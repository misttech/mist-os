// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    anyhow::{self, Context as _},
    fidl::endpoints::ServerEnd,
    fidl_fuchsia_io as fio, fidl_fuchsia_pkg as fpkg,
    futures::stream::TryStreamExt as _,
    std::collections::HashMap,
    tracing::error,
};

pub(crate) async fn serve_request_stream(
    mut stream: fpkg::PackageResolverRequestStream,
    base_packages: &HashMap<fuchsia_url::UnpinnedAbsolutePackageUrl, fuchsia_hash::Hash>,
    authenticator: crate::context_authenticator::ContextAuthenticator,
    blobfs: &blobfs::Client,
) -> anyhow::Result<()> {
    while let Some(request) =
        stream.try_next().await.context("failed to read request from FIDL stream")?
    {
        match request {
            fpkg::PackageResolverRequest::Resolve { package_url, dir, responder } => {
                match resolve(&package_url, dir, base_packages, authenticator.clone(), blobfs)
                        .await
                    {
                        Ok(context) => responder.send(Ok(&context)),
                        Err(e) => {
                            let fidl_error = (&e).into();
                            error!("failed to resolve package {}: {:#}", package_url, anyhow::anyhow!(e));
                            responder.send(Err(fidl_error))
                        }
                    }
                    .context("sending fuchsia.pkg/PackageResolver.Resolve response")?;
            }
            fpkg::PackageResolverRequest::ResolveWithContext {
                package_url,
                context,
                dir,
                responder,
            } => {
                match resolve_with_context(
                    &package_url,
                    context,
                    dir,
                    base_packages,
                    authenticator.clone(),
                    blobfs,
                )
                .await
                {
                    Ok(context) => responder.send(Ok(&context)),
                    Err(e) => {
                        let fidl_error = (&e).into();
                        error!(
                            "failed to resolve with context package {}: {:#}",
                            package_url,
                            anyhow::anyhow!(e)
                        );
                        responder.send(Err(fidl_error))
                    }
                }
                .context("sending fuchsia.pkg/PackageResolver.ResolveWithContext response")?;
            }
            fpkg::PackageResolverRequest::GetHash { package_url, responder } => {
                error!(
                    "unsupported fuchsia.pkg/PackageResolver.GetHash called with {:?}",
                    package_url
                );
                let () = responder
                    .send(Err(fuchsia_zircon::Status::NOT_SUPPORTED.into_raw()))
                    .context("sending fuchsia.pkg/PackageResolver.GetHash response")?;
            }
        }
    }
    Ok(())
}

async fn resolve_with_context(
    package_url: &str,
    context: fpkg::ResolutionContext,
    dir: ServerEnd<fio::DirectoryMarker>,
    base_packages: &HashMap<fuchsia_url::UnpinnedAbsolutePackageUrl, fuchsia_hash::Hash>,
    authenticator: crate::context_authenticator::ContextAuthenticator,
    blobfs: &blobfs::Client,
) -> Result<fpkg::ResolutionContext, crate::ResolverError> {
    resolve_with_context_impl(
        &fuchsia_url::PackageUrl::parse(package_url)?,
        context,
        dir,
        base_packages,
        authenticator,
        blobfs,
    )
    .await
}

pub(crate) async fn resolve_with_context_impl(
    package_url: &fuchsia_url::PackageUrl,
    context: fpkg::ResolutionContext,
    dir: ServerEnd<fio::DirectoryMarker>,
    base_packages: &HashMap<fuchsia_url::UnpinnedAbsolutePackageUrl, fuchsia_hash::Hash>,
    authenticator: crate::context_authenticator::ContextAuthenticator,
    blobfs: &blobfs::Client,
) -> Result<fpkg::ResolutionContext, crate::ResolverError> {
    match package_url {
        fuchsia_url::PackageUrl::Absolute(url) => {
            if !context.bytes.is_empty() {
                return Err(crate::ResolverError::ContextWithAbsoluteUrl);
            }
            resolve_impl(url, dir, base_packages, authenticator, blobfs).await
        }
        fuchsia_url::PackageUrl::Relative(url) => {
            resolve_subpackage(url, context, dir, authenticator, blobfs).await
        }
    }
}

async fn resolve(
    url: &str,
    dir: ServerEnd<fio::DirectoryMarker>,
    base_packages: &HashMap<fuchsia_url::UnpinnedAbsolutePackageUrl, fuchsia_hash::Hash>,
    authenticator: crate::context_authenticator::ContextAuthenticator,
    blobfs: &blobfs::Client,
) -> Result<fpkg::ResolutionContext, crate::ResolverError> {
    resolve_impl(&url.parse()?, dir, base_packages, authenticator, blobfs).await
}

pub(crate) async fn resolve_impl(
    url: &fuchsia_url::AbsolutePackageUrl,
    dir: ServerEnd<fio::DirectoryMarker>,
    base_packages: &HashMap<fuchsia_url::UnpinnedAbsolutePackageUrl, fuchsia_hash::Hash>,
    authenticator: crate::context_authenticator::ContextAuthenticator,
    blobfs: &blobfs::Client,
) -> Result<fpkg::ResolutionContext, crate::ResolverError> {
    let url_storage;
    let url = match url {
        fuchsia_url::AbsolutePackageUrl::Pinned(_) => {
            return Err(crate::ResolverError::PackageHashNotSupported);
        }
        fuchsia_url::AbsolutePackageUrl::Unpinned(url) => {
            // TODO(fxbug.dev/53911) Remove zero-variant fallback once variant concept is removed.
            // Base packages must have a variant of zero, and the variant is cleared before adding
            // the URL to the base_packages map. Clients are allowed to specify or omit the
            // variant (clients generally omit so we minimize the number of allocations in that
            // case).
            match url.variant() {
                Some(variant) if variant.is_zero() => {
                    let mut url = url.clone();
                    url.clear_variant();
                    url_storage = url;
                    &url_storage
                }
                _ => url,
            }
        }
    };
    let hash = base_packages
        .get(url)
        .ok_or_else(|| crate::ResolverError::PackageNotInBase(url.clone().into()))?;
    let () = package_directory::serve(
        package_directory::ExecutionScope::new(),
        blobfs.clone(),
        *hash,
        fio::OpenFlags::RIGHT_READABLE | fio::OpenFlags::RIGHT_EXECUTABLE,
        dir,
    )
    .await
    .map_err(crate::ResolverError::ServePackageDirectory)?;
    Ok(authenticator.create(hash))
}

async fn resolve_subpackage(
    package_url: &fuchsia_url::RelativePackageUrl,
    context: fpkg::ResolutionContext,
    dir: ServerEnd<fio::DirectoryMarker>,
    authenticator: crate::context_authenticator::ContextAuthenticator,
    blobfs: &blobfs::Client,
) -> Result<fpkg::ResolutionContext, crate::ResolverError> {
    let super_hash = authenticator.clone().authenticate(context)?;
    let super_package = package_directory::RootDir::new(blobfs.clone(), super_hash)
        .await
        .map_err(crate::ResolverError::CreatePackageDirectory)?;
    let subpackage = *super_package
        .subpackages()
        .await?
        .subpackages()
        .get(package_url)
        .ok_or_else(|| crate::ResolverError::SubpackageNotFound)?;
    let () = package_directory::serve(
        package_directory::ExecutionScope::new(),
        blobfs.clone(),
        subpackage,
        fio::OpenFlags::RIGHT_READABLE | fio::OpenFlags::RIGHT_EXECUTABLE,
        dir,
    )
    .await
    .map_err(crate::ResolverError::ServePackageDirectory)?;
    Ok(authenticator.create(&subpackage))
}

#[cfg(test)]
mod tests {
    use {super::*, assert_matches::assert_matches};

    #[fuchsia::test]
    async fn resolve_rejects_pinned_url() {
        assert_matches!(
            resolve(
                "fuchsia-pkg://fuchsia.test/name?\
                    hash=0000000000000000000000000000000000000000000000000000000000000000",
                fidl::endpoints::create_endpoints().1,
                &HashMap::from_iter([(
                    "fuchsia-pkg://fuchsia.test/name".parse().unwrap(),
                    [0; 32].into()
                )]),
                crate::context_authenticator::ContextAuthenticator::new(),
                &blobfs::Client::new_test().0
            )
            .await,
            Err(crate::ResolverError::PackageHashNotSupported)
        )
    }

    #[fuchsia::test]
    async fn resolve_with_context_rejects_pinned_url() {
        assert_matches!(
            resolve_with_context(
                "fuchsia-pkg://fuchsia.test/name?\
                    hash=0000000000000000000000000000000000000000000000000000000000000000",
                fpkg::ResolutionContext { bytes: vec![] },
                fidl::endpoints::create_endpoints().1,
                &HashMap::from_iter([(
                    "fuchsia-pkg://fuchsia.test/name".parse().unwrap(),
                    [0; 32].into()
                )]),
                crate::context_authenticator::ContextAuthenticator::new(),
                &blobfs::Client::new_test().0
            )
            .await,
            Err(crate::ResolverError::PackageHashNotSupported)
        )
    }

    #[fuchsia::test]
    async fn resolve_clears_zero_variant() {
        let pkg = fuchsia_pkg_testing::PackageBuilder::new("name").build().await.unwrap();
        let blobfs = blobfs_ramdisk::BlobfsRamdisk::start().await.unwrap();
        let blobfs_client = blobfs.client();
        pkg.write_to_blobfs_dir(&blobfs.root_dir().unwrap());
        let (proxy, server) = fidl::endpoints::create_proxy().unwrap();

        let _: fpkg::ResolutionContext = resolve(
            "fuchsia-pkg://fuchsia.test/name/0",
            server,
            &HashMap::from_iter([(
                "fuchsia-pkg://fuchsia.test/name".parse().unwrap(),
                *pkg.meta_far_merkle_root(),
            )]),
            crate::context_authenticator::ContextAuthenticator::new(),
            &blobfs_client,
        )
        .await
        .unwrap();

        assert_eq!(
            fuchsia_pkg::PackageDirectory::from_proxy(proxy).merkle_root().await.unwrap(),
            *pkg.meta_far_merkle_root()
        );
    }

    #[fuchsia::test]
    async fn resolve_does_not_clear_non_zero_variant() {
        assert_matches!(
            resolve(
                "fuchsia-pkg://fuchsia.test/name/1",
                fidl::endpoints::create_proxy().unwrap().1,
                &HashMap::from_iter([(
                    "fuchsia-pkg://fuchsia.test/name".parse().unwrap(),
                    [0u8; 32].into()
                )]),
                crate::context_authenticator::ContextAuthenticator::new(),
                &blobfs::Client::new_test().0
            )
            .await,
            Err(crate::ResolverError::PackageNotInBase(_))
        );
    }
}
