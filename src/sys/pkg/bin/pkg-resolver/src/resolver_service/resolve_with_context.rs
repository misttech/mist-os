// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    super::QueuedResolver,
    crate::eager_package_manager::EagerPackageManager,
    anyhow::anyhow,
    fidl_fuchsia_io as fio, fidl_fuchsia_metrics as fmetrics, fidl_fuchsia_pkg as fpkg,
    fidl_fuchsia_pkg_ext as pkg,
    tracing::{error, info},
};

pub(super) async fn resolve_with_context(
    package_url: String,
    context: fpkg::ResolutionContext,
    gc_protection: fpkg::GcProtection,
    dir: fidl::endpoints::ServerEnd<fio::DirectoryMarker>,
    package_resolver: &QueuedResolver,
    pkg_cache: &pkg::cache::Client,
    eager_package_manager: Option<&async_lock::RwLock<EagerPackageManager<QueuedResolver>>>,
    cobalt_sender: fidl_contrib::protocol_connector::ProtocolSender<fmetrics::MetricEvent>,
) -> Result<fpkg::ResolutionContext, pkg::ResolveError> {
    match fuchsia_url::PackageUrl::parse(&package_url)
        .map_err(|e| super::handle_bad_package_url_error(e, &package_url))?
    {
        fuchsia_url::PackageUrl::Absolute(url) => {
            if !context.bytes.is_empty() {
                error!(
                    "ResolveWithContext context must be empty if url is absolute {} {:?}",
                    package_url, context,
                );
                return Err(pkg::ResolveError::InvalidContext);
            }
            super::resolve_absolute_url_and_send_cobalt_metrics(
                url,
                gc_protection,
                dir,
                package_resolver,
                eager_package_manager,
                cobalt_sender,
            )
            .await
        }
        fuchsia_url::PackageUrl::Relative(url) => {
            resolve_relative(&url, &context, dir, pkg_cache).await
        }
    }
}

async fn resolve_relative(
    url: &fuchsia_url::RelativePackageUrl,
    context: &fpkg::ResolutionContext,
    dir: fidl::endpoints::ServerEnd<fio::DirectoryMarker>,
    pkg_cache: &pkg::cache::Client,
) -> Result<fpkg::ResolutionContext, pkg::ResolveError> {
    let context = pkg::ResolutionContext::try_from(context).map_err(|e| {
        error!("failed to parse relative url {} context {:?}: {:#}", url, context, anyhow!(e));
        pkg::ResolveError::InvalidContext
    })?;

    let child_context =
        resolve_relative_impl(url, &context, dir, pkg_cache).await.map_err(|e| {
            let fidl_err = e.to_fidl_err();
            error!(
                "failed to resolve relative url {} with parent {:?} {:#}",
                url,
                context.blob_id(),
                anyhow!(e)
            );
            fidl_err
        })?;

    info!("resolved relative url {} with parent {:?}", url, context.blob_id());

    Ok(child_context.into())
}

async fn resolve_relative_impl(
    url: &fuchsia_url::RelativePackageUrl,
    context: &pkg::ResolutionContext,
    dir: fidl::endpoints::ServerEnd<fio::DirectoryMarker>,
    pkg_cache: &pkg::cache::Client,
) -> Result<pkg::ResolutionContext, ResolveWithContextError> {
    let super_blob = if let Some(blob) = context.blob_id() {
        blob
    } else {
        return Err(ResolveWithContextError::EmptyContext);
    };
    let superpackage = pkg_cache
        .get_cached(*super_blob)
        .await
        .map_err(ResolveWithContextError::MissingSuperpackage)?;
    let subpackages = superpackage.meta_subpackages().await?;
    let subpackage = if let Some(hash) = subpackages.subpackages().get(url) {
        pkg::BlobId::from(*hash)
    } else {
        return Err(ResolveWithContextError::NotASubpackage);
    };

    // Resolving the superpackage resolves all subpackages and superpackages protect subpackages
    // from GC, so the subpackage should already be cached.
    // https://fuchsia.dev/fuchsia-src/contribute/governance/rfcs/0154_subpackages#eager_package_loading
    let () = pkg_cache
        .get_cached(subpackage)
        .await
        .map_err(ResolveWithContextError::MissingSubpackage)?
        .reopen(dir)
        .map_err(ResolveWithContextError::Reopen)?;

    Ok(subpackage.into())
}

#[derive(thiserror::Error, Debug)]
enum ResolveWithContextError {
    #[error("invalid context")]
    InvalidContext(#[from] pkg::ResolutionContextError),

    #[error("resolving a relative url requires a populated resolution context")]
    EmptyContext,

    #[error("the superpackage was not cached")]
    MissingSuperpackage(#[source] pkg::cache::GetCachedError),

    #[error("loading superpackage's subpackage manifest")]
    SubpackageManifest(#[from] fuchsia_pkg::package_directory::LoadMetaSubpackagesError),

    #[error("the relative url is not a subpackage of the superpackage indicated by the context")]
    NotASubpackage,

    #[error("the subpackage was not cached")]
    MissingSubpackage(#[source] pkg::cache::GetCachedError),

    #[error("reopening subpackage onto the request handle")]
    Reopen(#[source] fuchsia_pkg::package_directory::CloneError),
}

impl ResolveWithContextError {
    fn to_fidl_err(&self) -> pkg::ResolveError {
        use ResolveWithContextError::*;
        match self {
            InvalidContext(_) => pkg::ResolveError::InvalidContext,
            EmptyContext => pkg::ResolveError::InvalidContext,
            MissingSuperpackage(_) => pkg::ResolveError::Internal,
            SubpackageManifest(_) => pkg::ResolveError::Io,
            NotASubpackage => pkg::ResolveError::PackageNotFound,
            MissingSubpackage(_) => pkg::ResolveError::Internal,
            Reopen(_) => pkg::ResolveError::Internal,
        }
    }
}
