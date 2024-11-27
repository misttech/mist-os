// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use fuchsia_url::{AbsolutePackageUrl, PinnedAbsolutePackageUrl};
use futures::prelude::*;
use std::collections::HashMap;
use update_package::{UpdateImagePackage, UpdatePackage};
use {fidl_fuchsia_io as fio, fidl_fuchsia_pkg as fpkg};

/// Error encountered while resolving a package.
#[derive(Debug, thiserror::Error)]
pub enum ResolveError {
    #[error("fidl error while resolving {1}")]
    Fidl(#[source] fidl::Error, AbsolutePackageUrl),

    #[error("error while resolving {1}")]
    Error(#[source] fidl_fuchsia_pkg_ext::ResolveError, AbsolutePackageUrl),
}

/// Resolves the update package given by `url` through the pkg_resolver.
pub(super) async fn resolve_update_package(
    pkg_resolver: &fpkg::PackageResolverProxy,
    url: &AbsolutePackageUrl,
) -> Result<UpdatePackage, ResolveError> {
    let dir = resolve_package(pkg_resolver, url.clone()).await?;
    Ok(UpdatePackage::new(dir))
}

/// Resolves each package URL through the package resolver with some concurrency, yielding results
/// of the resolved package directories. The output order is not guaranteed to match the input
/// order.
pub(super) fn resolve_packages<'a, I>(
    pkg_resolver: &'a fpkg::PackageResolverProxy,
    urls: I,
    concurrent_package_resolves: usize,
) -> impl Stream<Item = Result<fio::DirectoryProxy, ResolveError>> + 'a
where
    I: 'a + Iterator<Item = &'a PinnedAbsolutePackageUrl>,
{
    stream::iter(urls)
        .map(move |url| resolve_package(pkg_resolver, url.clone().into()))
        .buffer_unordered(concurrent_package_resolves)
}

/// Resolves each package URL through the package resolver with some concurrency, returning a mapping of the package urls to the resolved image package directories.
pub(super) async fn resolve_image_packages<'a, I>(
    pkg_resolver: &'a fpkg::PackageResolverProxy,
    urls: I,
    concurrent_package_resolves: usize,
) -> Result<HashMap<AbsolutePackageUrl, UpdateImagePackage>, ResolveError>
where
    I: 'a + Iterator<Item = &'a AbsolutePackageUrl>,
{
    stream::iter(urls)
        .map(move |url| async move {
            Result::<_, ResolveError>::Ok((
                url.clone(),
                UpdateImagePackage::new(resolve_package(pkg_resolver, url.clone()).await?),
            ))
        })
        .buffer_unordered(concurrent_package_resolves)
        .try_collect()
        .await
}

async fn resolve_package(
    pkg_resolver: &fpkg::PackageResolverProxy,
    url: AbsolutePackageUrl,
) -> Result<fio::DirectoryProxy, ResolveError> {
    let (dir, dir_server_end) = fidl::endpoints::create_proxy();
    let res = pkg_resolver.resolve(&url.to_string(), dir_server_end);
    let res = res.await.map_err(|e| ResolveError::Fidl(e, url.clone()))?;

    let _: fpkg::ResolutionContext = res.map_err(|raw| ResolveError::Error(raw.into(), url))?;
    Ok(dir)
}
