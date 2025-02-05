// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use super::context_authenticator::ContextAuthenticator;
use super::ResolverError;
use crate::upgradable_packages::UpgradablePackages;
use anyhow::Context as _;
use fidl::endpoints::Proxy as _;
use futures::stream::TryStreamExt as _;
use log::{error, warn};
use std::collections::HashMap;
use std::sync::Arc;
use version_history::AbiRevision;
use {
    fidl_fuchsia_component_decl as fcomponent_decl,
    fidl_fuchsia_component_resolution as fcomponent_resolution, fidl_fuchsia_io as fio,
    fidl_fuchsia_pkg as fpkg,
};

pub(crate) async fn serve_request_stream(
    mut stream: fcomponent_resolution::ResolverRequestStream,
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
            fcomponent_resolution::ResolverRequest::Resolve { component_url, responder } => {
                let () = responder
                    .send(
                        resolve(
                            &component_url,
                            &base_packages,
                            authenticator.clone(),
                            &open_packages,
                            scope.clone(),
                            &upgradable_packages,
                        )
                        .await
                        .map_err(|e| {
                            let fidl_err = (&e).into();
                            error!(
                                "failed to resolve component {}: {:#}",
                                component_url,
                                anyhow::anyhow!(e)
                            );
                            fidl_err
                        }),
                    )
                    .context("sending fuchsia.component.resolution/Resolver.Resolve response")?;
            }
            fcomponent_resolution::ResolverRequest::ResolveWithContext {
                component_url,
                context,
                responder,
            } => {
                let () = responder
                    .send(
                        resolve_with_context(
                            &component_url,
                            context,
                            &base_packages,
                            authenticator.clone(),
                            &open_packages,
                            scope.clone(),
                            &upgradable_packages,
                        )
                        .await
                        .map_err(|e| {
                            let fidl_err = (&e).into();
                            error!(
                                "failed to resolve with context component {}: {:#}",
                                component_url,
                                anyhow::anyhow!(e)
                            );
                            fidl_err
                        }),
                    )
                    .context(
                        "sending fuchsia.component.resolution/Resolver.ResolveWithContext response",
                    )?;
            }
            fcomponent_resolution::ResolverRequest::_UnknownMethod { ordinal, .. } => {
                warn!(ordinal:%; "Unknown Resolver request");
            }
        }
    }
    Ok(())
}

async fn resolve(
    url: &str,
    base_packages: &HashMap<fuchsia_url::UnpinnedAbsolutePackageUrl, fuchsia_hash::Hash>,
    authenticator: ContextAuthenticator,
    open_packages: &crate::RootDirCache,
    scope: package_directory::ExecutionScope,
    upgradable_packages: &Option<Arc<UpgradablePackages>>,
) -> Result<fcomponent_resolution::Component, ResolverError> {
    let url = fuchsia_url::ComponentUrl::parse(url)?;
    let (package, server_end) = fidl::endpoints::create_proxy();
    let context = super::package::resolve_impl(
        match url.package_url() {
            fuchsia_url::PackageUrl::Absolute(url) => url,
            fuchsia_url::PackageUrl::Relative(_) => Err(ResolverError::AbsoluteUrlRequired)?,
        },
        server_end,
        base_packages,
        authenticator,
        open_packages,
        scope,
        upgradable_packages,
    )
    .await?;
    resolve_from_package(&url, package, fcomponent_resolution::Context { bytes: context.bytes })
        .await
}

async fn resolve_with_context(
    url: &str,
    context: fcomponent_resolution::Context,
    base_packages: &HashMap<fuchsia_url::UnpinnedAbsolutePackageUrl, fuchsia_hash::Hash>,
    authenticator: ContextAuthenticator,
    open_packages: &crate::RootDirCache,
    scope: package_directory::ExecutionScope,
    upgradable_packages: &Option<Arc<UpgradablePackages>>,
) -> Result<fcomponent_resolution::Component, ResolverError> {
    let url = fuchsia_url::ComponentUrl::parse(url)?;
    let (package, server_end) = fidl::endpoints::create_proxy();
    let context = super::package::resolve_with_context_impl(
        url.package_url(),
        fpkg::ResolutionContext { bytes: context.bytes },
        server_end,
        base_packages,
        authenticator,
        open_packages,
        scope,
        upgradable_packages,
    )
    .await?;
    resolve_from_package(&url, package, fcomponent_resolution::Context { bytes: context.bytes })
        .await
}

async fn load_config(
    decl: &fcomponent_decl::Component,
    package: &fio::DirectoryProxy,
) -> Result<Option<fidl_fuchsia_mem::Data>, ResolverError> {
    let Some(config_decl) = decl.config.as_ref() else {
        return Ok(None);
    };
    let strategy = config_decl.value_source.as_ref().ok_or(ResolverError::InvalidConfigSource)?;
    let config_path = match strategy {
        fcomponent_decl::ConfigValueSource::Capabilities(_) => return Ok(None),
        fcomponent_decl::ConfigValueSource::PackagePath(path) => path,
        other => return Err(ResolverError::UnsupportedConfigSource(other.to_owned())),
    };

    Ok(Some(
        mem_util::open_file_data(package, config_path)
            .await
            .map_err(ResolverError::ConfigValuesNotFound)?,
    ))
}

async fn resolve_from_package(
    url: &fuchsia_url::ComponentUrl,
    package: fio::DirectoryProxy,
    outgoing_context: fcomponent_resolution::Context,
) -> Result<fcomponent_resolution::Component, ResolverError> {
    let data = mem_util::open_file_data(&package, url.resource())
        .await
        .map_err(ResolverError::ComponentNotFound)?;
    let decl: fcomponent_decl::Component = fidl::unpersist(
        mem_util::bytes_from_data(&data).map_err(ResolverError::ReadManifest)?.as_ref(),
    )
    .map_err(ResolverError::ParsingManifest)?;
    let config_values = load_config(&decl, &package).await?;
    let abi_revision =
        fidl_fuchsia_component_abi_ext::read_abi_revision_optional(&package, AbiRevision::PATH)
            .await
            .map_err(ResolverError::AbiRevision)?;
    Ok(fcomponent_resolution::Component {
        url: Some(url.to_string()),
        resolution_context: Some(outgoing_context),
        decl: Some(data),
        package: Some(fcomponent_resolution::Package {
            url: Some(url.package_url().to_string()),
            directory: Some(
                package
                    .into_channel()
                    .map_err(|_| ResolverError::ConvertProxyToChannel)?
                    .into_zx_channel()
                    .into(),
            ),
            ..Default::default()
        }),
        config_values,
        abi_revision: abi_revision.map(Into::into),
        ..Default::default()
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use assert_matches::assert_matches;

    #[fuchsia::test]
    async fn resolve_rejects_relative_url() {
        assert_matches!(
            resolve(
                "relative#meta/missing",
                &HashMap::new(),
                ContextAuthenticator::new(),
                &crate::root_dir::new_test(blobfs::Client::new_test().0).await.1,
                package_directory::ExecutionScope::new(),
                &None,
            )
            .await,
            Err(ResolverError::AbsoluteUrlRequired)
        )
    }
}
