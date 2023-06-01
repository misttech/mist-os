// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::{anyhow, Error};
use fidl::endpoints::{DiscoverableProtocolMarker, ServerEnd};
use fidl_fuchsia_component as fcomponent;
use fidl_fuchsia_component_decl as fdecl;
use fidl_fuchsia_component_runner as frunner;
use fidl_fuchsia_io as fio;
use fuchsia_component::client as fclient;
use rand::Rng;

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
    /// The realm in which the Starnix kernel is runner.
    ///
    /// The kernel runs in a "kernels" collection within that realm.
    realm: fcomponent::RealmProxy,

    /// The name of the Starnix kernel within that realm.
    pub name: String,

    /// The directory exposed by the Starnix kernel.
    ///
    /// This directory can be used to connect to services offered by the kernel.
    exposed_dir: fio::DirectoryProxy,
}

impl StarnixKernel {
    /// Creates a new instance of `starnix_kernel`.
    ///
    /// This is done by creating a new child in the `kernels` collection.
    pub async fn create(
        realm: fcomponent::RealmProxy,
        kernel_url: &str,
        start_info: frunner::ComponentStartInfo,
        controller: ServerEnd<frunner::ComponentControllerMarker>,
    ) -> Result<Self, Error> {
        let kernel_name = generate_kernel_name(&start_info)?;
        realm
            .create_child(
                &fdecl::CollectionRef { name: KERNEL_COLLECTION.into() },
                &fdecl::Child {
                    name: Some(kernel_name.clone()),
                    url: Some(kernel_url.to_string()),
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

        // Actually start the container.
        container_runner.start(start_info, controller)?;

        Ok(Self { realm, name: kernel_name, exposed_dir })
    }

    /// Connect to the specified protocol exposed by the kernel.
    pub fn connect_to_protocol<P: DiscoverableProtocolMarker>(&self) -> Result<P::Proxy, Error> {
        fclient::connect_to_protocol_at_dir_root::<P>(&self.exposed_dir)
    }

    /// Destroys the Starnix kernel that is running the given test.
    pub async fn destroy(&self) -> Result<(), Error> {
        self.realm
            .destroy_child(&fdecl::ChildRef {
                name: self.name.clone(),
                collection: Some(KERNEL_COLLECTION.into()),
            })
            .await?
            .map_err(|e| anyhow::anyhow!("failed to destory kernel: {:?}", e))?;
        Ok(())
    }
}

/// Takes a `name` and generates a `String` suitable to use as the name of a component in a
/// collection.
///
/// Used to avoid collisions.
fn append_random_suffix(name: &str) -> String {
    let random_id: String = rand::thread_rng()
        .sample_iter(&rand::distributions::Alphanumeric)
        .take(7)
        .map(char::from)
        .collect();
    name.to_owned() + "_" + &random_id
}

fn generate_kernel_name(start_info: &frunner::ComponentStartInfo) -> Result<String, Error> {
    let container_url = start_info.resolved_url.clone().ok_or(anyhow!("Missing resolved URL"))?;
    let container_name = container_url
        .split('/')
        .last()
        .expect("Could not find last path component in resolved URL");
    Ok(append_random_suffix(container_name))
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
