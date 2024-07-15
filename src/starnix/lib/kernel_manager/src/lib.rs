// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::{anyhow, Error};
use fidl::endpoints::{DiscoverableProtocolMarker, Proxy, ServerEnd};
use fidl::HandleBased;
use fuchsia_component::client as fclient;
use futures::TryStreamExt;
use rand::Rng;
use std::future::Future;
use std::sync::Arc;
use {
    fidl_fuchsia_component as fcomponent, fidl_fuchsia_component_decl as fdecl,
    fidl_fuchsia_component_runner as frunner, fidl_fuchsia_io as fio,
    fidl_fuchsia_starnix_container as fstarnix, fuchsia_zircon as zx,
};

/// The name of the collection that the starnix_kernel is run in.
const KERNEL_COLLECTION: &str = "kernels";

/// The name of the protocol the kernel exposes for running containers.
///
/// This protocol is actually fuchsia.component.runner.ComponentRunner. We
/// expose the implementation using this name to avoid confusion with copy
/// of the fuchsia.component.runner.ComponentRunner protocol used for
/// running component inside the container.
const CONTAINER_RUNNER_PROTOCOL: &str = "fuchsia.starnix.container.Runner";

pub struct StarnixKernel {
    /// The controller used to control the kernel component's lifecycle.
    ///
    /// The kernel runs in a "kernels" collection within that realm.
    controller_proxy: fcomponent::ControllerProxy,

    /// The directory exposed by the Starnix kernel.
    ///
    /// This directory can be used to connect to services offered by the kernel.
    exposed_dir: fio::DirectoryProxy,

    /// An opaque token representing the container component.
    component_instance: zx::Event,

    /// The job the kernel lives under.
    job: Arc<zx::Job>,
}

impl StarnixKernel {
    /// Creates a new instance of `starnix_kernel`.
    ///
    /// This is done by creating a new child in the `kernels` collection.
    ///
    /// Returns the kernel and a future that will resolve when the kernel has stopped.
    pub async fn create(
        realm: fcomponent::RealmProxy,
        kernel_url: &str,
        start_info: frunner::ComponentStartInfo,
        controller: ServerEnd<frunner::ComponentControllerMarker>,
    ) -> Result<(Self, impl Future<Output = ()>), Error> {
        let kernel_name = generate_kernel_name(&start_info)?;
        let component_instance = start_info
            .component_instance
            .as_ref()
            .ok_or_else(|| {
                anyhow::anyhow!("expected to find component_instance in ComponentStartInfo")
            })?
            .duplicate_handle(zx::Rights::SAME_RIGHTS)?;

        // Create the `starnix_kernel`.
        let (controller_proxy, controller_server_end) = fidl::endpoints::create_proxy()?;
        realm
            .create_child(
                &fdecl::CollectionRef { name: KERNEL_COLLECTION.into() },
                &fdecl::Child {
                    name: Some(kernel_name.clone()),
                    url: Some(kernel_url.to_string()),
                    startup: Some(fdecl::StartupMode::Lazy),
                    ..Default::default()
                },
                fcomponent::CreateChildArgs {
                    controller: Some(controller_server_end),
                    ..Default::default()
                },
            )
            .await?
            .map_err(|e| anyhow::anyhow!("failed to create kernel: {:?}", e))?;

        // Start the kernel to obtain an `ExecutionController`.
        let (execution_controller_proxy, execution_controller_server_end) =
            fidl::endpoints::create_proxy()?;
        controller_proxy
            .start(fcomponent::StartChildArgs::default(), execution_controller_server_end)
            .await?
            .map_err(|e| anyhow::anyhow!("failed to start kernel: {:?}", e))?;

        let exposed_dir = open_exposed_directory(&realm, &kernel_name, KERNEL_COLLECTION).await?;
        let container_runner = fclient::connect_to_named_protocol_at_dir_root::<
            frunner::ComponentRunnerMarker,
        >(&exposed_dir, CONTAINER_RUNNER_PROTOCOL)?;

        // Actually start the container.
        container_runner.start(start_info, controller)?;

        // Ask the kernel for its job.
        let container_controller =
            fclient::connect_to_protocol_at_dir_root::<fstarnix::ControllerMarker>(&exposed_dir)?;
        let fstarnix::ControllerGetJobHandleResponse { job, .. } =
            container_controller.get_job_handle().await?;
        let Some(job) = job else {
            anyhow::bail!("expected to find job in ControllerGetJobHandleResponse");
        };

        let kernel = Self { controller_proxy, exposed_dir, component_instance, job: Arc::new(job) };
        let on_stop = async move {
            _ = execution_controller_proxy.into_channel().unwrap().on_closed().await;
        };
        Ok((kernel, on_stop))
    }

    /// Gets the opaque token representing the container component.
    pub fn component_instance(&self) -> &zx::Event {
        &self.component_instance
    }

    /// Gets the job the kernel lives under.
    pub fn job(&self) -> &Arc<zx::Job> {
        &self.job
    }

    /// Connect to the specified protocol exposed by the kernel.
    pub fn connect_to_protocol<P: DiscoverableProtocolMarker>(&self) -> Result<P::Proxy, Error> {
        fclient::connect_to_protocol_at_dir_root::<P>(&self.exposed_dir)
    }

    /// Destroys the Starnix kernel that is running the given test.
    pub async fn destroy(self) -> Result<(), Error> {
        self.controller_proxy
            .destroy()
            .await?
            .map_err(|e| anyhow!("kernel component destruction failed: {e:?}"))?;
        let mut event_stream = self.controller_proxy.take_event_stream();
        loop {
            match event_stream.try_next().await {
                Ok(Some(_)) => continue,
                Ok(None) => return Ok(()),
                Err(e) => return Err(e.into()),
            }
        }
    }
}

/// Generate a random name for the kernel.
///
/// We used to include some human-readable parts in the name, but people were
/// tempted to make them load-bearing. We now just generate 7 random alphanumeric
/// characters.
fn generate_kernel_name(_start_info: &frunner::ComponentStartInfo) -> Result<String, Error> {
    let random_id: String = rand::thread_rng()
        .sample_iter(&rand::distributions::Alphanumeric)
        .take(7)
        .map(char::from)
        .collect();
    Ok(random_id)
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
