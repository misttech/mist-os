// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::{anyhow, Error};
use fidl::endpoints::ServerEnd;
use fidl_fuchsia_component as fcomponent;
use fidl_fuchsia_component_decl as fdecl;
use fidl_fuchsia_component_runner as frunner;
use fidl_fuchsia_io as fio;
use fuchsia_component::client::{self as fclient, connect_to_protocol};
use fuchsia_component::server::ServiceFs;
use futures::{StreamExt, TryStreamExt};

/// The name of the collection that the starnix_kernel is run in.
const KERNEL_COLLECTION: &str = "kernels";

/// The component URL of the Starnix kernel.
const KERNEL_URL: &str = "fuchsia-pkg://fuchsia.com/starnix_kernel#meta/starnix_kernel.cm";

/// The name of the protocol the kernel exposes for running containers.
///
/// This protocol is actually fuchsia.component.runner.ComponentRunner. We
/// expose the implementation using this name to avoid confusion with copy
/// of the fuchsia.component.runner.ComponentRunner protocol used for
/// running component inside the container.
const CONTAINER_RUNNER_PROTOCOL: &str = "fuchsia.starnix.container.Runner";

enum Services {
    ComponentRunner(frunner::ComponentRunnerRequestStream),
}

#[fuchsia::main(logging_tags = ["starnix_runner"])]
async fn main() -> Result<(), Error> {
    let mut fs = ServiceFs::new_local();
    fs.dir("svc").add_fidl_service(Services::ComponentRunner);
    fs.take_and_serve_directory_handle()?;
    fs.for_each_concurrent(None, |request: Services| async {
        match request {
            Services::ComponentRunner(stream) => {
                serve_component_runner(stream).await.expect("failed to start component runner")
            }
        }
    })
    .await;
    Ok(())
}

async fn serve_component_runner(
    mut stream: frunner::ComponentRunnerRequestStream,
) -> Result<(), Error> {
    while let Some(event) = stream.try_next().await? {
        match event {
            frunner::ComponentRunnerRequest::Start { start_info, controller, .. } => {
                create_new_kernel(start_info, controller).await?;
            }
        }
    }
    Ok(())
}

fn generate_kernel_name(start_info: &frunner::ComponentStartInfo) -> Result<String, Error> {
    let container_url = start_info.resolved_url.clone().ok_or(anyhow!("Missing resolved URL"))?;
    let container_name = container_url
        .split('/')
        .last()
        .expect("Could not find last path component in resolved URL");
    Ok(kernel_config::generate_kernel_name(container_name))
}

/// Creates a new instance of `starnix_kernel`.
///
/// This is done by creating a new child in the `kernels` collection.
async fn create_new_kernel(
    start_info: frunner::ComponentStartInfo,
    controller: ServerEnd<frunner::ComponentControllerMarker>,
) -> Result<(), Error> {
    let kernel_name = generate_kernel_name(&start_info)?;

    // Create a new instance of starnix_kernel in the kernel collection.
    let realm =
        connect_to_protocol::<fcomponent::RealmMarker>().expect("Failed to connect to realm.");
    realm
        .create_child(
            &fdecl::CollectionRef { name: KERNEL_COLLECTION.into() },
            &fdecl::Child {
                name: Some(kernel_name.clone()),
                url: Some(KERNEL_URL.to_string()),
                startup: Some(fdecl::StartupMode::Lazy),
                ..Default::default()
            },
            Default::default(),
        )
        .await?
        .map_err(|e| anyhow::anyhow!("failed to create kernel: {:?}", e))?;

    let exposed_dir = open_exposed_directory(&realm, &kernel_name, KERNEL_COLLECTION).await?;
    let container_runner = fclient::connect_to_named_protocol_at_dir_root::<
        frunner::ComponentRunnerMarker,
    >(&exposed_dir, CONTAINER_RUNNER_PROTOCOL)?;

    container_runner.start(start_info, controller)?;
    Ok(())
}

async fn open_exposed_directory(
    realm: &fcomponent::RealmProxy,
    child_name: &str,
    collection_name: &str,
) -> Result<fio::DirectoryProxy, Error> {
    let (directory_proxy, server_end) = fidl::endpoints::create_proxy::<fio::DirectoryMarker>()?;
    realm
        .open_exposed_dir(
            &fdecl::ChildRef { name: child_name.into(), collection: Some(collection_name.into()) },
            server_end,
        )
        .await?
        .map_err(|e| {
            anyhow!(
                "failed to bind to child {} in collection {:?}: {:?}",
                child_name,
                collection_name,
                e
            )
        })?;
    Ok(directory_proxy)
}
