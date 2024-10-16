// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::StarnixKernel;
use anyhow::Error;
use attribution_server::{AttributionServer, AttributionServerHandle};
use fidl::endpoints::{Proxy, ServerEnd};
use fidl::HandleBased;
#[cfg(feature = "wake_locks")]
use fidl_fuchsia_power_system as fpower;
use frunner::{ComponentControllerMarker, ComponentStartInfo};
use fuchsia_component::client::connect_to_protocol;
use fuchsia_sync::Mutex;
use std::collections::HashMap;
use std::sync::Arc;
use vfs::execution_scope::ExecutionScope;
use zx::AsHandleRef;
use {
    fidl_fuchsia_component as fcomponent, fidl_fuchsia_component_runner as frunner,
    fidl_fuchsia_memory_attribution as fattribution, zx,
};

/// The component URL of the Starnix kernel.
const KERNEL_URL: &str = "#meta/starnix_kernel.cm";

/// [`Kernels`] manages a collection of starnix kernels.
///
/// It also reports the memory usage attribution of each kernel.
pub struct Kernels {
    kernels: Arc<Mutex<HashMap<zx::Koid, StarnixKernel>>>,
    memory_attribution_server: AttributionServerHandle,
    memory_update_publisher: attribution_server::Publisher,
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
        let kernel_job = kernel.job.clone();
        let kernel_koid = kernel.job.get_koid()?;

        self.memory_update_publisher.on_update(attribution_info_for_kernel(&kernel));
        self.kernels.lock().insert(kernel_koid, kernel);

        let on_removed_publisher = self.memory_attribution_server.new_publisher();
        let kernels = self.kernels.clone();
        self.background_tasks.spawn(async move {
            on_stop.await;
            if let Some(kernel) = kernels.lock().remove(&kernel_koid) {
                let koid = kernel.component_instance().get_koid().unwrap().raw_koid();
                _ = kernel.destroy().await.inspect_err(|e| tracing::error!("{e:?}"));
                on_removed_publisher.on_update(vec![fattribution::AttributionUpdate::Remove(koid)]);
            }
        });
        self.acquire_wake_lease(&kernel_job).await?;
        Ok(())
    }

    /// Gets a momentary snapshot of all kernel jobs.
    pub fn all_jobs(&self) -> Vec<Arc<zx::Job>> {
        self.kernels.lock().iter().map(|(_, k)| Arc::clone(k.job())).collect()
    }

    pub fn new_memory_attribution_observer(
        &self,
        control_handle: fattribution::ProviderControlHandle,
    ) -> attribution_server::Observer {
        self.memory_attribution_server.new_observer(control_handle)
    }

    /// Drops any active wake lease for the container running in the given `container_job`.
    #[cfg(feature = "wake_locks")]
    pub fn drop_wake_lease(&self, container_job: &zx::Job) -> Result<(), Error> {
        let job_koid = container_job.get_koid()?;
        if let Some(kernel) = self.kernels.lock().get(&job_koid) {
            kernel.wake_lease.lock().take();
            tracing::info!("Dropped wake lease for {:?}", container_job);
        }
        Ok(())
    }

    #[cfg(not(feature = "wake_locks"))]
    pub fn drop_wake_lease(&self, _container_job: &zx::Job) -> Result<(), Error> {
        Ok(())
    }

    /// Acquires a wake lease for the container running in the given `container_job`.
    #[cfg(feature = "wake_locks")]
    pub async fn acquire_wake_lease(&self, container_job: &zx::Job) -> Result<(), Error> {
        let job_koid = container_job.get_koid()?;
        if let Some(kernel) = self.kernels.lock().get(&job_koid) {
            let activity_governor = connect_to_protocol::<fpower::ActivityGovernorMarker>()?;
            let wake_lease =
                match activity_governor.take_application_activity_lease(&kernel.name).await {
                    Ok(l) => l,
                    Err(e) => {
                        tracing::warn!(
                            "Failed to acquire application activity lease for kernel: {:?}",
                            e
                        );
                        return Ok(());
                    }
                };
            *kernel.wake_lease.lock() = Some(wake_lease);
            tracing::info!("Acquired wake lease for {:?}", container_job);
        }
        Ok(())
    }

    #[cfg(not(feature = "wake_locks"))]
    pub async fn acquire_wake_lease(&self, _container_job: &zx::Job) -> Result<(), Error> {
        Ok(())
    }
}

impl Drop for Kernels {
    fn drop(&mut self) {
        self.background_tasks.shutdown();
    }
}

fn get_attribution(
    kernels: Arc<Mutex<HashMap<zx::Koid, StarnixKernel>>>,
) -> Vec<fattribution::AttributionUpdate> {
    let kernels = kernels.lock();
    let mut updates = vec![];
    for kernel in kernels.iter().map(|(_, v)| v) {
        updates.extend(attribution_info_for_kernel(kernel));
    }
    vec![]
}

fn attribution_info_for_kernel(kernel: &StarnixKernel) -> Vec<fattribution::AttributionUpdate> {
    let new_principal = fattribution::NewPrincipal {
        identifier: Some(kernel.component_instance().get_koid().unwrap().raw_koid()),
        description: Some(fattribution::Description::Component(
            kernel.component_instance().duplicate_handle(zx::Rights::SAME_RIGHTS).unwrap(),
        )),
        principal_type: Some(fattribution::PrincipalType::Runnable),
        detailed_attribution: kernel
            .connect_to_protocol::<fattribution::ProviderMarker>()
            .inspect_err(|e|
                tracing::error!(%e, "Error connecting to memory attribution of the starnix kernel")
            )
            .ok()
            .map(|proxy| proxy.into_channel().unwrap().into_zx_channel().into()),
        ..Default::default()
    };
    let attribution = fattribution::UpdatedPrincipal {
        identifier: Some(kernel.component_instance().get_koid().unwrap().raw_koid()),
        resources: Some(fattribution::Resources::Data(fattribution::Data {
            resources: vec![fattribution::Resource::KernelObject(
                kernel.job().basic_info().unwrap().koid.raw_koid(),
            )],
        })),
        ..Default::default()
    };
    vec![
        fattribution::AttributionUpdate::Add(new_principal),
        fattribution::AttributionUpdate::Update(attribution),
    ]
}
