// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use super::context_authenticator::ContextAuthenticator;
use super::ResolverError;
use crate::upgradable_packages::UpgradablePackages;
use anyhow::Context as _;
use fidl::endpoints::ServerEnd;
use fuchsia_url::UnpinnedAbsolutePackageUrl;
use futures::stream::TryStreamExt as _;
use std::collections::HashMap;
use std::sync::Arc;
use tracing::error;
use vfs::directory::entry_container::Directory;
use {fidl_fuchsia_io as fio, fidl_fuchsia_pkg as fpkg};

pub(crate) async fn serve_request_stream(
    mut stream: fpkg::PackageResolverRequestStream,
    base_packages: Arc<HashMap<fuchsia_url::UnpinnedAbsolutePackageUrl, fuchsia_hash::Hash>>,
    authenticator: ContextAuthenticator,
    open_packages: crate::RootDirCache,
    scope: package_directory::ExecutionScope,
    upgradable_packages: Option<Arc<UpgradablePackages>>,
) -> anyhow::Result<()> {
    while let Some(request) =
        stream.try_next().await.context("failed to read request from FIDL stream")?
    {
        match request {
            fpkg::PackageResolverRequest::Resolve { package_url, dir, responder } => {
                match resolve(
                    &package_url,
                    dir,
                    &base_packages,
                    authenticator.clone(),
                    &open_packages,
                    scope.clone(),
                    &upgradable_packages,
                )
                .await
                {
                    Ok(context) => responder.send(Ok(&context)),
                    Err(e) => {
                        let fidl_error = (&e).into();
                        error!(
                            "failed to resolve package {}: {:#}",
                            package_url,
                            anyhow::anyhow!(e)
                        );
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
                    &base_packages,
                    authenticator.clone(),
                    &open_packages,
                    scope.clone(),
                    &upgradable_packages,
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
                    .send(Err(zx::Status::NOT_SUPPORTED.into_raw()))
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
    authenticator: ContextAuthenticator,
    open_packages: &crate::RootDirCache,
    scope: package_directory::ExecutionScope,
    upgradable_packages: &Option<Arc<UpgradablePackages>>,
) -> Result<fpkg::ResolutionContext, ResolverError> {
    resolve_with_context_impl(
        &fuchsia_url::PackageUrl::parse(package_url)?,
        context,
        dir,
        base_packages,
        authenticator,
        open_packages,
        scope,
        upgradable_packages,
    )
    .await
}

pub(super) async fn resolve_with_context_impl(
    package_url: &fuchsia_url::PackageUrl,
    context: fpkg::ResolutionContext,
    dir: ServerEnd<fio::DirectoryMarker>,
    base_packages: &HashMap<fuchsia_url::UnpinnedAbsolutePackageUrl, fuchsia_hash::Hash>,
    authenticator: ContextAuthenticator,
    open_packages: &crate::RootDirCache,
    scope: package_directory::ExecutionScope,
    upgradable_packages: &Option<Arc<UpgradablePackages>>,
) -> Result<fpkg::ResolutionContext, ResolverError> {
    match package_url {
        fuchsia_url::PackageUrl::Absolute(url) => {
            if !context.bytes.is_empty() {
                return Err(ResolverError::ContextWithAbsoluteUrl);
            }
            resolve_impl(
                url,
                dir,
                base_packages,
                authenticator,
                open_packages,
                scope,
                upgradable_packages,
            )
            .await
        }
        fuchsia_url::PackageUrl::Relative(url) => {
            resolve_subpackage(url, context, dir, authenticator, open_packages, scope).await
        }
    }
}

async fn resolve(
    url: &str,
    dir: ServerEnd<fio::DirectoryMarker>,
    base_packages: &HashMap<fuchsia_url::UnpinnedAbsolutePackageUrl, fuchsia_hash::Hash>,
    authenticator: ContextAuthenticator,
    open_packages: &crate::RootDirCache,
    scope: package_directory::ExecutionScope,
    upgradable_packages: &Option<Arc<UpgradablePackages>>,
) -> Result<fpkg::ResolutionContext, ResolverError> {
    resolve_impl(
        &url.parse()?,
        dir,
        base_packages,
        authenticator,
        open_packages,
        scope,
        upgradable_packages,
    )
    .await
}

pub(super) async fn resolve_impl(
    url: &fuchsia_url::AbsolutePackageUrl,
    dir: ServerEnd<fio::DirectoryMarker>,
    base_packages: &HashMap<fuchsia_url::UnpinnedAbsolutePackageUrl, fuchsia_hash::Hash>,
    authenticator: ContextAuthenticator,
    open_packages: &crate::RootDirCache,
    scope: package_directory::ExecutionScope,
    upgradable_packages: &Option<Arc<UpgradablePackages>>,
) -> Result<fpkg::ResolutionContext, ResolverError> {
    let url = match url {
        fuchsia_url::AbsolutePackageUrl::Pinned(_) => {
            return Err(ResolverError::PackageHashNotSupported);
        }
        fuchsia_url::AbsolutePackageUrl::Unpinned(url) => url,
    };
    let hash =
        resolve_package(url, dir, base_packages, open_packages, scope, upgradable_packages).await?;
    Ok(authenticator.create(&hash))
}

pub(crate) async fn resolve_package(
    url: &fuchsia_url::UnpinnedAbsolutePackageUrl,
    dir: ServerEnd<fio::DirectoryMarker>,
    base_packages: &HashMap<fuchsia_url::UnpinnedAbsolutePackageUrl, fuchsia_hash::Hash>,
    open_packages: &crate::RootDirCache,
    scope: package_directory::ExecutionScope,
    upgradable_packages: &Option<Arc<UpgradablePackages>>,
) -> Result<fuchsia_hash::Hash, ResolverError> {
    // TODO(https://fxbug.dev/335388895) Remove zero-variant fallback once variant concept is gone.
    // Base packages must have a variant of zero, and the variant is cleared before adding the URL
    // to the base_packages map. Clients are allowed to specify or omit the variant (clients
    // generally omit so we minimize the number of allocations in that case).
    let url = match url.variant() {
        Some(variant) if variant.is_zero() => &{
            let mut url = url.clone();
            url.clear_variant();
            url
        },
        _ => url,
    };
    let hash = get_package_hash(url, base_packages, upgradable_packages)
        .await
        .ok_or_else(|| ResolverError::PackageNotInBase(url.clone().into()))?;
    let () = open_packages
        .get_or_insert(hash, None)
        .await
        .map_err(ResolverError::ServePackageDirectory)?
        .open(
            scope,
            fio::OpenFlags::RIGHT_READABLE | fio::OpenFlags::RIGHT_EXECUTABLE,
            vfs::path::Path::dot(),
            dir.into_channel().into(),
        );
    Ok(hash)
}

async fn get_package_hash(
    url: &UnpinnedAbsolutePackageUrl,
    base_packages: &HashMap<fuchsia_url::UnpinnedAbsolutePackageUrl, fuchsia_hash::Hash>,
    upgradable_packages: &Option<Arc<UpgradablePackages>>,
) -> Option<fuchsia_hash::Hash> {
    if let Some(hash) = base_packages.get(url) {
        return Some(*hash);
    }
    if let Some(upgradable_packages) = upgradable_packages {
        if let Some(hash) = upgradable_packages.get_hash(url).await {
            return Some(hash);
        }
    }
    None
}

async fn resolve_subpackage(
    package_url: &fuchsia_url::RelativePackageUrl,
    context: fpkg::ResolutionContext,
    dir: ServerEnd<fio::DirectoryMarker>,
    authenticator: ContextAuthenticator,
    open_packages: &crate::RootDirCache,
    scope: package_directory::ExecutionScope,
) -> Result<fpkg::ResolutionContext, ResolverError> {
    let super_hash = authenticator.clone().authenticate(context)?;
    let super_package =
        open_packages.get(&super_hash).ok_or_else(|| ResolverError::SuperpackageNotOpen {
            superpackage: super_hash,
            subpackage: package_url.clone(),
        })?;
    let subpackage = *super_package
        .subpackages()
        .await?
        .subpackages()
        .get(package_url)
        .ok_or_else(|| ResolverError::SubpackageNotFound)?;
    let () = open_packages
        .get_or_insert(subpackage, None)
        .await
        .map_err(ResolverError::ServePackageDirectory)?
        .open(
            scope,
            fio::OpenFlags::RIGHT_READABLE | fio::OpenFlags::RIGHT_EXECUTABLE,
            vfs::path::Path::dot(),
            dir.into_channel().into(),
        );
    Ok(authenticator.create(&subpackage))
}

#[cfg(test)]
mod tests {
    use super::*;
    use assert_matches::assert_matches;

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
                ContextAuthenticator::new(),
                &crate::root_dir::new_test(blobfs::Client::new_test().0).await.1,
                vfs::execution_scope::ExecutionScope::new(),
                &None,
            )
            .await,
            Err(ResolverError::PackageHashNotSupported)
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
                ContextAuthenticator::new(),
                &crate::root_dir::new_test(blobfs::Client::new_test().0).await.1,
                vfs::execution_scope::ExecutionScope::new(),
                &None,
            )
            .await,
            Err(ResolverError::PackageHashNotSupported)
        )
    }

    #[fuchsia::test]
    async fn resolve_clears_zero_variant() {
        let pkg = fuchsia_pkg_testing::PackageBuilder::new("name").build().await.unwrap();
        let blobfs = blobfs_ramdisk::BlobfsRamdisk::start().await.unwrap();
        pkg.write_to_blobfs(&blobfs).await;
        let open_packages = crate::root_dir::new_test(blobfs.client()).await.1;
        let (proxy, server) = fidl::endpoints::create_proxy();

        let _: fpkg::ResolutionContext = resolve(
            "fuchsia-pkg://fuchsia.test/name/0",
            server,
            &HashMap::from_iter([(
                "fuchsia-pkg://fuchsia.test/name".parse().unwrap(),
                *pkg.hash(),
            )]),
            ContextAuthenticator::new(),
            &open_packages,
            vfs::execution_scope::ExecutionScope::new(),
            &None,
        )
        .await
        .unwrap();

        assert_eq!(
            fuchsia_pkg::PackageDirectory::from_proxy(proxy).merkle_root().await.unwrap(),
            *pkg.hash()
        );
    }

    #[fuchsia::test]
    async fn resolve_does_not_clear_non_zero_variant() {
        assert_matches!(
            resolve(
                "fuchsia-pkg://fuchsia.test/name/1",
                fidl::endpoints::create_proxy().1,
                &HashMap::from_iter([(
                    "fuchsia-pkg://fuchsia.test/name".parse().unwrap(),
                    [0u8; 32].into()
                )]),
                ContextAuthenticator::new(),
                &crate::root_dir::new_test(blobfs::Client::new_test().0).await.1,
                vfs::execution_scope::ExecutionScope::new(),
                &None,
            )
            .await,
            Err(ResolverError::PackageNotInBase(_))
        );
    }
}
