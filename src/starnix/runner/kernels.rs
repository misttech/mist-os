// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::Error;
use attribution::{AttributionServer, AttributionServerHandle};
use fidl::{endpoints::ServerEnd, HandleBased};
use fidl_fuchsia_component as fcomponent;
use fidl_fuchsia_component_runner as frunner;
use fidl_fuchsia_memory_attribution as fattribution;
use frunner::{ComponentControllerMarker, ComponentStartInfo};
use fuchsia_component::client::connect_to_protocol;
use fuchsia_sync::Mutex;
use fuchsia_zircon as zx;
use kernel_manager::StarnixKernel;
use std::{collections::HashMap, sync::Arc};
use vfs::execution_scope::ExecutionScope;
use zx::AsHandleRef;

/// The component URL of the Starnix kernel.
const KERNEL_URL: &str = "starnix_kernel#meta/starnix_kernel.cm";

/// [`Kernels`] manages a collection of starnix kernels.
///
/// It also reports the memory usage attribution of each kernel.
pub struct Kernels {
    /// Mapping from name to StarnixKernel.
    kernels: Arc<Mutex<HashMap<String, Arc<StarnixKernel>>>>,
    memory_attribution_server: AttributionServerHandle,
    memory_update_publisher: attribution::Publisher,
    background_tasks: ExecutionScope,
}

impl Kernels {
    /// Creates a new [`Kernels`] instance.
    pub fn new() -> Self {
        let kernels = Default::default();
        let weak_kernels = Arc::downgrade(&kernels);
        let memory_attribution_server = AttributionServer::new(Box::new(move || {
            weak_kernels.upgrade().map(get_attribution).unwrap_or_default()
        }));
        let memory_update_publisher = memory_attribution_server.new_publisher();
        Self {
            kernels,
            memory_attribution_server,
            memory_update_publisher,
            background_tasks: ExecutionScope::new(),
        }
    }

    /// Runs a new starnix kernel and adds it to the collection.
    pub async fn start(
        &self,
        start_info: ComponentStartInfo,
        controller: ServerEnd<ComponentControllerMarker>,
    ) -> Result<(), Error> {
        let realm =
            connect_to_protocol::<fcomponent::RealmMarker>().expect("Failed to connect to realm.");
        let (kernel, on_stop) =
            StarnixKernel::create(realm, KERNEL_URL, start_info, controller).await?;
        self.memory_update_publisher.on_update(attribution_info_for_kernel(&kernel)).unwrap();
        let name = kernel.name().to_string();
        self.kernels.lock().insert(name.clone(), Arc::new(kernel));
        let kernels = self.kernels.clone();
        let on_removed_publisher = self.memory_attribution_server.new_publisher();
        self.background_tasks.spawn(async move {
            on_stop.await;
            if let Some(kernel) = kernels.lock().remove(&name) {
                _ = kernel.destroy().await.inspect_err(|e| tracing::error!("{e:?}"));
                on_removed_publisher
                    .on_update(vec![fattribution::AttributionUpdate::Remove(
                        fattribution::Identifier::Component(
                            kernel
                                .component_instance()
                                .duplicate_handle(zx::Rights::SAME_RIGHTS)
                                .unwrap(),
                        ),
                    )])
                    .unwrap();
            }
        });
        Ok(())
    }

    /// Gets a momentary snapshot of the currently running kernels.
    pub fn list(&self) -> Vec<Arc<StarnixKernel>> {
        self.kernels.lock().values().cloned().collect()
    }

    pub fn new_memory_attribution_observer(
        &self,
        control_handle: fattribution::ProviderControlHandle,
    ) -> attribution::Observer {
        self.memory_attribution_server.new_observer(control_handle)
    }
}

impl Drop for Kernels {
    fn drop(&mut self) {
        self.background_tasks.shutdown();
    }
}

fn get_attribution(
    kernels: Arc<Mutex<HashMap<String, Arc<StarnixKernel>>>>,
) -> Vec<fattribution::AttributionUpdate> {
    let kernels = kernels.lock();
    let mut updates = vec![];
    for kernel in kernels.values() {
        updates.extend(attribution_info_for_kernel(kernel));
    }
    vec![]
}

fn attribution_info_for_kernel(kernel: &StarnixKernel) -> Vec<fattribution::AttributionUpdate> {
    let new_principal = fattribution::NewPrincipal {
        identifier: Some(fattribution::Identifier::Component(
            kernel.component_instance().duplicate_handle(zx::Rights::SAME_RIGHTS).unwrap(),
        )),
        type_: Some(fattribution::Type::Runnable),
        ..Default::default()
    };
    let attribution = fattribution::UpdatedPrincipal {
        identifier: Some(fattribution::Identifier::Component(
            kernel.component_instance().duplicate_handle(zx::Rights::SAME_RIGHTS).unwrap(),
        )),
        resources: Some(fattribution::Resources::Data(vec![fattribution::Resource::KernelObject(
            kernel.job().basic_info().unwrap().koid.raw_koid(),
        )])),
        ..Default::default()
    };
    vec![
        fattribution::AttributionUpdate::Add(new_principal),
        fattribution::AttributionUpdate::Update(attribution),
    ]
}
