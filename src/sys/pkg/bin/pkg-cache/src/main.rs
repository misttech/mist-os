// Copyright 2018 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#![allow(clippy::let_unit_value)]
#![allow(clippy::enum_variant_names)]
#![allow(clippy::from_over_into)]
#![allow(clippy::too_many_arguments)]

use crate::base_packages::{BasePackages, CachePackages};
use crate::index::PackageIndex;
use anyhow::{anyhow, format_err, Context as _, Error};
use fidl::endpoints::DiscoverableProtocolMarker as _;
use fidl_contrib::protocol_connector::ConnectedProtocol;
use fidl_contrib::ProtocolConnector;
use fidl_fuchsia_metrics::{
    MetricEvent, MetricEventLoggerFactoryMarker, MetricEventLoggerProxy, ProjectSpec,
};
use fidl_fuchsia_update::CommitStatusProviderMarker;
use fuchsia_async::Task;
use fuchsia_component::client::connect_to_protocol;
use futures::join;
use futures::prelude::*;
use std::collections::HashMap;
use std::sync::atomic::AtomicU32;
use std::sync::Arc;
use tracing::{error, info};
use vfs::directory::entry_container::Directory;
use vfs::directory::helper::DirectlyMutable as _;
use vfs::remote::remote_dir;
use vfs::ObjectRequest;
use {
    cobalt_sw_delivery_registry as metrics, fidl_fuchsia_io as fio, fuchsia_async as fasync,
    fuchsia_inspect as finspect,
};

mod base_packages;
mod base_resolver;
mod cache_service;
mod compat;
mod gc_service;
mod index;
mod reboot;
mod required_blobs;
mod retained_packages_service;
mod root_dir;
mod upgradable_packages;

use root_dir::{RootDir, RootDirCache, RootDirFactory};

#[cfg(test)]
mod test_utils;

const COBALT_CONNECTOR_BUFFER_SIZE: usize = 1000;

struct CobaltConnectedService;
impl ConnectedProtocol for CobaltConnectedService {
    type Protocol = MetricEventLoggerProxy;
    type ConnectError = Error;
    type Message = MetricEvent;
    type SendError = Error;

    fn get_protocol(&mut self) -> future::BoxFuture<'_, Result<MetricEventLoggerProxy, Error>> {
        async {
            let (logger_proxy, server_end) = fidl::endpoints::create_proxy();
            let metric_event_logger_factory =
                connect_to_protocol::<MetricEventLoggerFactoryMarker>()
                    .context("Failed to connect to fuchsia::metrics::MetricEventLoggerFactory")?;

            metric_event_logger_factory
                .create_metric_event_logger(
                    &ProjectSpec { project_id: Some(metrics::PROJECT_ID), ..Default::default() },
                    server_end,
                )
                .await?
                .map_err(|e| format_err!("Connection to MetricEventLogger refused {e:?}"))?;
            Ok(logger_proxy)
        }
        .boxed()
    }

    fn send_message<'a>(
        &'a mut self,
        protocol: &'a MetricEventLoggerProxy,
        msg: MetricEvent,
    ) -> future::BoxFuture<'a, Result<(), Error>> {
        async move {
            let fut = protocol.log_metric_events(&[msg]);
            fut.await?.map_err(|e| format_err!("Failed to log metric {e:?}"))?;
            Ok(())
        }
        .boxed()
    }
}

#[fuchsia::main(logging_tags = ["pkg-cache"])]
pub fn main() -> Result<(), Error> {
    fuchsia_trace_provider::trace_provider_create_with_fdio();

    let mut executor = fasync::LocalExecutor::new();
    executor.run_singlethreaded(async move {
        match main_inner().await {
            Err(err) => {
                let err = anyhow!(err);
                error!("error running pkg-cache: {:#}", err);
                let () = reboot::reboot().await;
                Err(err)
            }
            ok => ok,
        }
    })
}

async fn main_inner() -> Result<(), Error> {
    info!("starting package cache service");
    let inspector = finspect::Inspector::default();
    let config = pkg_cache_config::Config::take_from_startup_handle();
    inspector
        .root()
        .record_child("structured_config", |config_node| config.record_inspect(config_node));
    // TODO(https://fxbug.dev/331302451) Use the all_packages_executable config value instead of the
    // presence of file data/pkgfs_disable_executability_restrictions in the system_image package to
    // determine whether executability should be enforced.
    let pkg_cache_config::Config {
        all_packages_executable: _,
        use_fxblob,
        use_system_image,
        enable_upgradable_packages,
    } = config;
    let builder = blobfs::Client::builder().readable().writable().executable();
    let blobfs = if use_fxblob { builder.use_creator().use_reader() } else { builder }
        .build()
        .await
        .context("error opening blobfs")?;

    let authenticator = base_resolver::context_authenticator::ContextAuthenticator::new();

    let (executability_restrictions, base_packages, cache_packages) = if use_system_image {
        let boot_args = connect_to_protocol::<fidl_fuchsia_boot::ArgumentsMarker>()
            .context("error connecting to fuchsia.boot/Arguments")?;
        let system_image = system_image::SystemImage::new(blobfs.clone(), &boot_args)
            .await
            .context("Accessing contents of system_image package")?;
        info!("system_image package: {}", system_image.hash().to_string());
        inspector.root().record_string("system_image", system_image.hash().to_string());

        let (base_packages_res, cache_packages_res) =
            join!(BasePackages::new(&blobfs, &system_image), async {
                let cache_packages =
                    system_image.cache_packages().await.context("reading cache_packages")?;
                CachePackages::new(&blobfs, &cache_packages)
                    .await
                    .context("creating CachePackages index")
            });
        let base_packages = base_packages_res.context("loading base packages")?;
        let cache_packages = cache_packages_res.unwrap_or_else(|e: anyhow::Error| {
            error!("Failed to load cache packages, using empty: {e:#}");
            CachePackages::empty()
        });
        let executability_restrictions = system_image.load_executability_restrictions();

        (executability_restrictions, base_packages, cache_packages)
    } else {
        info!("not loading system_image due to structured config");
        inspector.root().record_string("system_image", "ignored");
        (
            system_image::ExecutabilityRestrictions::Enforce,
            BasePackages::empty(),
            CachePackages::empty(),
        )
    };

    inspector
        .root()
        .record_string("executability-restrictions", format!("{executability_restrictions:?}"));
    let base_resolver_base_packages =
        Arc::new(base_packages.root_package_urls_and_hashes().clone());
    let base_packages = Arc::new(base_packages);
    let cache_packages = Arc::new(cache_packages);
    inspector.root().record_lazy_child("base-packages", base_packages.record_lazy_inspect());
    inspector.root().record_lazy_child("cache-packages", cache_packages.record_lazy_inspect());
    let package_index = Arc::new(async_lock::RwLock::new(PackageIndex::new()));
    inspector.root().record_lazy_child("index", PackageIndex::record_lazy_inspect(&package_index));
    let scope = vfs::execution_scope::ExecutionScope::new();
    let (cobalt_sender, cobalt_fut) = ProtocolConnector::new_with_buffer_size(
        CobaltConnectedService,
        COBALT_CONNECTOR_BUFFER_SIZE,
    )
    .serve_and_log_errors();
    let cobalt_fut = Task::spawn(cobalt_fut);

    let (root_dir_factory, open_packages) = root_dir::new(
        fuchsia_fs::directory::open_in_namespace(
            "/bootfs-blobs",
            fio::PERM_READABLE | fio::PERM_EXECUTABLE,
        )
        .context("open bootfs blobs dir")?,
        blobfs.clone(),
    )
    .await
    .context("creating root dir helpers")?;
    inspector.root().record_lazy_child("open-packages", open_packages.record_lazy_inspect());

    let upgradable_packages = enable_upgradable_packages.then(|| {
        Arc::new(upgradable_packages::UpgradablePackages::new(Arc::clone(&cache_packages)))
    });

    // Use VFS to serve the out dir because ServiceFs does not support OPEN_RIGHT_EXECUTABLE and
    // pkgfs/{packages|system} require it.
    let svc_dir = vfs::pseudo_directory! {};
    let cache_inspect_node = inspector.root().create_child("fuchsia.pkg.PackageCache");
    {
        let package_index = Arc::clone(&package_index);
        let blobfs = blobfs.clone();
        let root_dir_factory = root_dir_factory.clone();
        let open_packages = open_packages.clone();
        let base_packages = Arc::clone(&base_packages);
        let cache_packages = Arc::clone(&cache_packages);
        let upgradable_packages = upgradable_packages.clone();
        let scope = scope.clone();
        let cobalt_sender = cobalt_sender.clone();
        let cache_inspect_id = Arc::new(AtomicU32::new(0));
        let cache_get_node = Arc::new(cache_inspect_node.create_child("get"));

        let () = svc_dir
            .add_entry(
                fidl_fuchsia_pkg::PackageCacheMarker::PROTOCOL_NAME,
                vfs::service::host(move |stream: fidl_fuchsia_pkg::PackageCacheRequestStream| {
                    cache_service::serve(
                        Arc::clone(&package_index),
                        blobfs.clone(),
                        root_dir_factory.clone(),
                        Arc::clone(&base_packages),
                        Arc::clone(&cache_packages),
                        upgradable_packages.clone(),
                        executability_restrictions,
                        scope.clone(),
                        open_packages.clone(),
                        stream,
                        cobalt_sender.clone(),
                        Arc::clone(&cache_inspect_id),
                        Arc::clone(&cache_get_node),
                    )
                    .unwrap_or_else(|e| {
                        error!(
                            "error handling fuchsia.pkg.PackageCache connection: {:#}",
                            anyhow!(e)
                        )
                    })
                }),
            )
            .context("adding fuchsia.pkg/PackageCache to /svc")?;
    }
    {
        let package_index = Arc::clone(&package_index);
        let blobfs = blobfs.clone();

        let () = svc_dir
            .add_entry(
                fidl_fuchsia_pkg::RetainedPackagesMarker::PROTOCOL_NAME,
                vfs::service::host(
                    move |stream: fidl_fuchsia_pkg::RetainedPackagesRequestStream| {
                        retained_packages_service::serve(
                            Arc::clone(&package_index),
                            blobfs.clone(),
                            stream,
                        )
                        .unwrap_or_else(|e| {
                            error!(
                                "error handling fuchsia.pkg/RetainedPackages connection: {:#}",
                                anyhow!(e)
                            )
                        })
                    },
                ),
            )
            .context("adding fuchsia.pkg/RetainedPackages to /svc")?;
    }
    {
        let blobfs = blobfs.clone();
        let base_packages = Arc::clone(&base_packages);
        let upgradable_packages = upgradable_packages.clone();
        let open_packages = open_packages.clone();
        let commit_status_provider =
            fuchsia_component::client::connect_to_protocol::<CommitStatusProviderMarker>()
                .context("while connecting to commit status provider")?;

        let () = svc_dir
            .add_entry(
                fidl_fuchsia_space::ManagerMarker::PROTOCOL_NAME,
                vfs::service::host(move |stream: fidl_fuchsia_space::ManagerRequestStream| {
                    gc_service::serve(
                        blobfs.clone(),
                        Arc::clone(&base_packages),
                        Arc::clone(&cache_packages),
                        upgradable_packages.clone(),
                        Arc::clone(&package_index),
                        open_packages.clone(),
                        commit_status_provider.clone(),
                        stream,
                    )
                    .unwrap_or_else(|e| {
                        error!("error handling fuchsia.space/Manager connection: {:#}", anyhow!(e))
                    })
                }),
            )
            .context("adding fuchsia.space/Manager to /svc")?;
    }
    {
        let base_resolver_base_packages = Arc::clone(&base_resolver_base_packages);
        let authenticator = authenticator.clone();
        let open_packages = open_packages.clone();
        let scope = scope.clone();
        let upgradable_packages = upgradable_packages.clone();
        let () = svc_dir
            .add_entry(
                fidl_fuchsia_pkg::PackageResolverMarker::PROTOCOL_NAME,
                vfs::service::host(
                    move |stream: fidl_fuchsia_pkg::PackageResolverRequestStream| {
                        base_resolver::package::serve_request_stream(
                            stream,
                            Arc::clone(&base_resolver_base_packages),
                            authenticator.clone(),
                            open_packages.clone(),
                            scope.clone(),
                            upgradable_packages.clone(),
                        )
                        .unwrap_or_else(|e| {
                            error!("failed to serve package resolver request: {:#}", e)
                        })
                    },
                ),
            )
            .context("adding fuchsia.space/Manager to /svc")?;
    }
    {
        let base_resolver_base_packages = Arc::clone(&base_resolver_base_packages);
        let authenticator = authenticator.clone();
        let open_packages = open_packages.clone();
        let scope = scope.clone();
        let upgradable_packages = upgradable_packages.clone();
        let () = svc_dir
            .add_entry(
                fidl_fuchsia_component_resolution::ResolverMarker::PROTOCOL_NAME,
                vfs::service::host(
                    move |stream: fidl_fuchsia_component_resolution::ResolverRequestStream| {
                        base_resolver::component::serve_request_stream(
                            stream,
                            Arc::clone(&base_resolver_base_packages),
                            authenticator.clone(),
                            open_packages.clone(),
                            scope.clone(),
                            upgradable_packages.clone(),
                        )
                        .unwrap_or_else(|e| {
                            error!("failed to serve component resolver request: {:#}", e)
                        })
                    },
                ),
            )
            .context("adding fuchsia.space/Manager to /svc")?;
    }

    let base_package_entry = |name: &'static str| {
        serve_base_package_if_present(
            fuchsia_url::UnpinnedAbsolutePackageUrl::new(
                "fuchsia-pkg://fuchsia.com".parse().expect("valid repo url"),
                name.parse().expect("valid package name"),
                None,
            ),
            base_resolver_base_packages.as_ref(),
            &open_packages,
            scope.clone(),
        )
        .map(move |result| result.map(remote_dir).with_context(|| format!("getting {name} dir")))
    };

    let out_dir = vfs::pseudo_directory! {
        "svc" => svc_dir,
        "pkgfs" =>
            crate::compat::pkgfs::make_dir(
                Arc::clone(&base_packages),
                blobfs.clone(),
            ),
        "specific-base-packages" => vfs::pseudo_directory! {
            "build-info" => base_package_entry("build-info").await?,
            "config-data" => base_package_entry("config-data").await?,
            "root_ssl_certificates" => base_package_entry("root_ssl_certificates").await?,
            "system_image" => base_package_entry("system_image").await?,
        }
    };

    let _inspect_server_task =
        inspect_runtime::publish(&inspector, inspect_runtime::PublishOptions::default());

    let flags = fio::PERM_READABLE | fio::PERM_WRITABLE | fio::PERM_EXECUTABLE;
    ObjectRequest::new(
        flags,
        &fio::Options::default(),
        fuchsia_runtime::take_startup_handle(fuchsia_runtime::HandleType::DirectoryRequest.into())
            .context("taking startup handle")?
            .into(),
    )
    .handle(|request| out_dir.open3(scope.clone(), vfs::path::Path::dot(), flags, request));

    let () = scope.wait().await;
    cobalt_fut.await;

    Ok(())
}

async fn serve_base_package_if_present(
    url: fuchsia_url::UnpinnedAbsolutePackageUrl,
    base_packages: &HashMap<fuchsia_url::UnpinnedAbsolutePackageUrl, fuchsia_hash::Hash>,
    open_packages: &RootDirCache,
    scope: package_directory::ExecutionScope,
) -> anyhow::Result<fio::DirectoryProxy> {
    let (proxy, server) = fidl::endpoints::create_proxy::<fio::DirectoryMarker>();
    match base_resolver::package::resolve_package(
        &url,
        server,
        base_packages,
        open_packages,
        scope,
        &None,
    )
    .await
    {
        Ok::<fuchsia_hash::Hash, _>(_) => (),
        Err(base_resolver::ResolverError::PackageNotInBase(_)) => {
            tracing::warn!(%url, "package not in base, so exposed directory will close connections")
        }
        Err(e) => Err(e).context("resolving specific base package")?,
    }
    Ok(proxy)
}
