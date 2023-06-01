// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    anyhow::{anyhow, Error},
    fidl::endpoints::{create_proxy, Proxy},
    fidl_fuchsia_component as fcomponent, fidl_fuchsia_component_decl as fdecl,
    fidl_fuchsia_component_runner as frunner, fidl_fuchsia_io as fio,
    fuchsia_component::client as fclient,
    fuchsia_zircon as zx,
    tracing::debug,
};

/// The name of the collection in which the starnix kernel is instantiated.
const RUNNERS_COLLECTION: &str = "runners";

/// The component URL of the Starnix kernel.
const KERNEL_URL: &str = "starnix_kernel#meta/starnix_kernel.cm";

/// The protocol name the kernel uses to expose the container runner.
const CONTAINER_RUNNER_PROTOCOL: &str = "fuchsia.starnix.container.Runner";

fn generate_kernel_name(start_info: &frunner::ComponentStartInfo) -> Result<String, Error> {
    let container_url = start_info.resolved_url.clone().ok_or(anyhow!("Missing resolved URL"))?;
    let container_name = container_url
        .split('/')
        .last()
        .expect("Could not find last path component in resolved URL");
    Ok(kernel_config::generate_kernel_name(container_name))
}

pub struct TestContainer {
    /// The realm in which the test runs.
    realm: fcomponent::RealmProxy,

    /// The name of the kernel used to run the test.
    kernel_name: String,

    /// The component controller for the container.
    _container_controller: frunner::ComponentControllerProxy,

    /// The runner use to run the test.
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
        let kernel_name = generate_kernel_name(&start_info)?;

        // Grab the realm from the component's namespace.
        let realm =
            get_realm(start_info.ns.as_mut().ok_or(anyhow!("Couldn't get realm from namespace"))?)?;

        realm
            .create_child(
                &fdecl::CollectionRef { name: RUNNERS_COLLECTION.into() },
                &fdecl::Child {
                    name: Some(kernel_name.clone()),
                    url: Some(KERNEL_URL.to_string()),
                    startup: Some(fdecl::StartupMode::Lazy),
                    ..Default::default()
                },
                Default::default(),
            )
            .await?
            .map_err(|e| anyhow::anyhow!("failed to create runner child: {:?}", e))?;

        let kernel_outgoing =
            open_exposed_directory(&realm, &kernel_name, RUNNERS_COLLECTION).await?;

        let (container_controller, container_controller_server_end) =
            create_proxy::<frunner::ComponentControllerMarker>()?;

        let container_runner = fclient::connect_to_named_protocol_at_dir_root::<
            frunner::ComponentRunnerMarker,
        >(&kernel_outgoing, CONTAINER_RUNNER_PROTOCOL)?;
        container_runner.start(start_info, container_controller_server_end)?;

        let component_runner = fclient::connect_to_protocol_at_dir_root::<
            frunner::ComponentRunnerMarker,
        >(&kernel_outgoing)?;

        debug!(%kernel_name, "instantiated starnix test kernel");
        Ok(Self {
            realm,
            kernel_name,
            _container_controller: container_controller,
            component_runner,
        })
    }

    /// Destroys the Starnix kernel that is running the given test.
    pub async fn destroy(&self) -> Result<(), Error> {
        self.realm
            .destroy_child(&fdecl::ChildRef {
                name: self.kernel_name.clone(),
                collection: Some(RUNNERS_COLLECTION.into()),
            })
            .await?
            .map_err(|e| anyhow::anyhow!("failed to destory runner child: {:?}", e))?;
        Ok(())
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
                    .unwrap()
                    .into_proxy()
                    .expect("Failed to grab directory proxy.");
                let r = Some(fuchsia_component::client::connect_to_protocol_at_dir_root::<
                    fcomponent::RealmMarker,
                >(&directory));

                let dir_channel: zx::Channel = directory.into_channel().expect("").into();
                entry.directory = Some(dir_channel.into());
                r
            } else {
                None
            }
        })
        .next()
        .ok_or_else(|| anyhow!("Unable to find /svc"))?
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
