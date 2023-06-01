// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    anyhow::{anyhow, Error},
    fidl::endpoints::{create_proxy, Proxy},
    fidl_fuchsia_component as fcomponent, fidl_fuchsia_component_runner as frunner,
    fuchsia_zircon as zx,
    kernel_manager::StarnixKernel,
    tracing::debug,
};

/// The component URL of the Starnix kernel.
const KERNEL_URL: &str = "starnix_kernel#meta/starnix_kernel.cm";

pub struct TestContainer {
    /// The Starnix kernel that is running the container.
    kernel: StarnixKernel,

    /// The runner used to run the test.
    ///
    /// This runner runs the test inside the container.
    pub component_runner: frunner::ComponentRunnerProxy,
}

impl TestContainer {
    /// Creates a Starnix container in the realm associated with the given `ComponentStartInfo`.
    ///
    /// # Parameters
    ///
    ///   - `start_info`: The component start info used to create the configuration for the
    ///                   starnix container that will run the test.
    pub async fn create(mut start_info: frunner::ComponentStartInfo) -> Result<Self, Error> {
        // Grab the realm from the component's namespace.
        let realm =
            get_realm(start_info.ns.as_mut().ok_or(anyhow!("Couldn't get realm from namespace"))?)?;

        let (_container_controller, container_controller_server_end) =
            create_proxy::<frunner::ComponentControllerMarker>()?;

        let kernel =
            StarnixKernel::create(realm, KERNEL_URL, start_info, container_controller_server_end)
                .await?;

        let component_runner = kernel.connect_to_protocol::<frunner::ComponentRunnerMarker>()?;

        debug!(%kernel.name, "instantiated starnix test kernel");
        Ok(Self { kernel, component_runner })
    }

    /// Destroys this Starnix container.
    pub async fn destroy(&self) -> Result<(), Error> {
        self.kernel.destroy().await
    }
}

fn get_realm(
    namespace: &mut Vec<frunner::ComponentNamespaceEntry>,
) -> Result<fcomponent::RealmProxy, Error> {
    namespace
        .iter_mut()
        .flat_map(|entry| {
            if entry.path.as_ref().unwrap() == "/svc" {
                let directory = entry
                    .directory
                    .take()
                    .expect("Missing directory handle for /svc")
                    .into_proxy()
                    .expect("Failed to grab directory proxy.");
                let realm = Some(fuchsia_component::client::connect_to_protocol_at_dir_root::<
                    fcomponent::RealmMarker,
                >(&directory));

                let dir_channel: zx::Channel =
                    directory.into_channel().expect("Could not extract channel from proxy").into();
                entry.directory = Some(dir_channel.into());
                realm
            } else {
                None
            }
        })
        .next()
        .ok_or_else(|| anyhow!("Unable to find /svc"))?
}
