// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::Error;
use fuchsia_component::client::connect_to_protocol;
use fuchsia_component::server::ServiceFs;
use futures::StreamExt;
use std::sync::Arc;
use test_manager_lib::{constants, AboveRootCapabilitiesForTest, RootDiagnosticNode};
use tracing::{info, warn};
use {
    fidl_fuchsia_component_resolution as fresolution, fidl_fuchsia_pkg as fpkg,
    fuchsia_async as fasync,
};

const DEFAULT_MANIFEST_NAME: &str = "test_manager.cm";

/// Arguments passed to test manager.
struct TestManagerArgs {
    /// optional positional argument that specifies an override for the name of the manifest.
    manifest_name: Option<String>,
}

impl TryFrom<std::env::Args> for TestManagerArgs {
    type Error = Error;
    fn try_from(args: std::env::Args) -> Result<Self, Self::Error> {
        let mut args_vec: Vec<_> = args.collect();
        match args_vec.len() {
            1 => Ok(Self { manifest_name: None }),
            2 => Ok(Self { manifest_name: args_vec.pop() }),
            _ => anyhow::bail!("Unexpected number of arguments: {:?}", args_vec),
        }
    }
}

impl TestManagerArgs {
    pub fn manifest_name(&self) -> &str {
        self.manifest_name.as_ref().map(String::as_str).unwrap_or(DEFAULT_MANIFEST_NAME)
    }
}

#[fuchsia::main]
async fn main() -> Result<(), Error> {
    info!("started");
    let args: TestManagerArgs = std::env::args().try_into()?;
    let mut fs = ServiceFs::new();

    std::fs::create_dir_all(constants::KERNEL_DEBUG_DATA_FOR_SCP)?;
    std::fs::create_dir_all(constants::DEBUG_DATA_FOR_SCP)?;
    std::fs::create_dir_all(constants::ISOLATED_TMP)?;

    let _inspect_server_task = inspect_runtime::publish(
        fuchsia_inspect::component::inspector(),
        inspect_runtime::PublishOptions::default(),
    );

    info!("Reading capabilities from {}", args.manifest_name());
    let routing_info_for_run_builder =
        Arc::new(AboveRootCapabilitiesForTest::new(args.manifest_name()).await?);
    let routing_info_for_query = routing_info_for_run_builder.clone();
    let routing_info_for_task_test_case_enumerator = routing_info_for_run_builder.clone();
    let routing_info_for_suite_runner = routing_info_for_run_builder.clone();

    let resolver_for_run_builder = Arc::new(
        connect_to_protocol::<fresolution::ResolverMarker>()
            .expect("Cannot connect to component resolver"),
    );
    let resolver_for_query = resolver_for_run_builder.clone();
    let resolver_for_test_case_enumerator = resolver_for_run_builder.clone();
    let resolver_for_suite_runner = resolver_for_run_builder.clone();

    let pkg_resolver_for_run_builder = Arc::new(
        connect_to_protocol::<fpkg::PackageResolverMarker>()
            .expect("Cannot connect to pkg resolver"),
    );
    let pkg_resolver_for_query = pkg_resolver_for_run_builder.clone();
    let pkg_resolver_for_test_case_enumerator = pkg_resolver_for_run_builder.clone();
    let pkg_resolver_for_suite_runner = pkg_resolver_for_run_builder.clone();

    let root_inspect_for_run_builder = Arc::new(RootDiagnosticNode::new(
        fuchsia_inspect::component::inspector().root().clone_weak(),
    ));
    let root_inspect_for_query = root_inspect_for_run_builder.clone();
    let root_inspect_for_test_case_enumerator = root_inspect_for_run_builder.clone();
    let root_inspect_for_suite_runner = root_inspect_for_run_builder.clone();

    fs.dir("svc")
        .add_fidl_service(move |stream| {
            let resolver = resolver_for_run_builder.clone();
            let pkg_resolver = pkg_resolver_for_run_builder.clone();
            let routing_info = routing_info_for_run_builder.clone();
            let root_inspect = root_inspect_for_run_builder.clone();

            fasync::Task::spawn(async move {
                test_manager_lib::run_test_manager_run_builder_server(
                    stream,
                    resolver,
                    pkg_resolver,
                    routing_info,
                    &*root_inspect,
                )
                .await
                .unwrap_or_else(|error| warn!(?error, "test manager returned error"))
            })
            .detach();
        })
        .add_fidl_service(move |stream| {
            let resolver = resolver_for_query.clone();
            let pkg_resolver = pkg_resolver_for_query.clone();
            let routing_info = routing_info_for_query.clone();
            let root_inspect = root_inspect_for_query.clone();

            fasync::Task::local(async move {
                test_manager_lib::run_test_manager_query_server(
                    stream,
                    resolver,
                    pkg_resolver,
                    routing_info,
                    &*root_inspect,
                )
                .await
                .unwrap_or_else(|error| warn!(?error, "test manager returned error"))
            })
            .detach();
        })
        .add_fidl_service(move |stream| {
            fasync::Task::spawn(async move {
                test_manager_lib::serve_early_boot_profiles(stream)
                    .await
                    .unwrap_or_else(|error| warn!(?error, "test manager returned error"))
            })
            .detach();
        })
        .add_fidl_service(move |stream| {
            let resolver = resolver_for_test_case_enumerator.clone();
            let pkg_resolver = pkg_resolver_for_test_case_enumerator.clone();
            let routing_info = routing_info_for_task_test_case_enumerator.clone();
            let root_inspect = root_inspect_for_test_case_enumerator.clone();

            fasync::Task::local(async move {
                test_manager_lib::run_test_manager_test_case_enumerator_server(
                    stream,
                    resolver,
                    pkg_resolver,
                    routing_info,
                    &*root_inspect,
                )
                .await
                .unwrap_or_else(|error| warn!(?error, "test manager returned error"))
            })
            .detach();
        })
        .add_fidl_service(move |stream| {
            let resolver = resolver_for_suite_runner.clone();
            let pkg_resolver = pkg_resolver_for_suite_runner.clone();
            let routing_info = routing_info_for_suite_runner.clone();
            let root_inspect = root_inspect_for_suite_runner.clone();

            fasync::Task::spawn(async move {
                test_manager_lib::run_test_manager_suite_runner_server(
                    stream,
                    resolver,
                    pkg_resolver,
                    routing_info,
                    &*root_inspect,
                )
                .await
                .unwrap_or_else(|error| warn!(?error, "test manager returned error"))
            })
            .detach();
        });
    fs.take_and_serve_directory_handle()?;
    fs.collect::<()>().await;
    Ok(())
}
