// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    crate::builtin::capability::BuiltinCapability,
    anyhow::{format_err, Error},
    async_trait::async_trait,
    cm_types::Name,
    fidl_fuchsia_kernel as fkernel,
    fuchsia_zircon::{self as zx, HandleBased, Resource},
    futures::prelude::*,
    lazy_static::lazy_static,
    routing::capability_source::InternalCapability,
    std::sync::Arc,
};

lazy_static! {
    static ref ENERGY_INFO_RESOURCE_CAPABILITY_NAME: Name =
        "fuchsia.kernel.EnergyInfoResource".parse().unwrap();
}

/// An implementation of fuchsia.kernel.EnergyInfoResource protocol.
pub struct EnergyInfoResource {
    resource: Resource,
}

impl EnergyInfoResource {
    /// `resource` must be the energy info resource.
    pub fn new(resource: Resource) -> Result<Arc<Self>, Error> {
        let resource_info = resource.info()?;
        if resource_info.kind != zx::sys::ZX_RSRC_KIND_SYSTEM
            || resource_info.base != zx::sys::ZX_RSRC_SYSTEM_ENERGY_INFO_BASE
            || resource_info.size != 1
        {
            return Err(format_err!("Energy info resource not available."));
        }
        Ok(Arc::new(Self { resource }))
    }
}

#[async_trait]
impl BuiltinCapability for EnergyInfoResource {
    const NAME: &'static str = "EnergyInfoResource";
    type Marker = fkernel::EnergyInfoResourceMarker;

    async fn serve(
        self: Arc<Self>,
        mut stream: fkernel::EnergyInfoResourceRequestStream,
    ) -> Result<(), Error> {
        while let Some(fkernel::EnergyInfoResourceRequest::Get { responder }) =
            stream.try_next().await?
        {
            responder.send(self.resource.duplicate_handle(zx::Rights::SAME_RIGHTS)?)?;
        }
        Ok(())
    }

    fn matches_routed_capability(&self, capability: &InternalCapability) -> bool {
        capability.matches_protocol(&ENERGY_INFO_RESOURCE_CAPABILITY_NAME)
    }
}

#[cfg(test)]
mod tests {
    use {
        super::*,
        crate::{
            builtin::capability::BuiltinCapability,
            capability::CapabilitySource,
            model::hooks::{Event, EventPayload, Hooks},
        },
        cm_task_scope::TaskScope,
        fidl::endpoints::ClientEnd,
        fidl_fuchsia_io as fio, fidl_fuchsia_kernel as fkernel, fuchsia_async as fasync,
        fuchsia_component::client::connect_to_protocol,
        fuchsia_zircon::sys,
        fuchsia_zircon::AsHandleRef,
        futures::lock::Mutex,
        moniker::{AbsoluteMoniker, AbsoluteMonikerBase},
        std::path::PathBuf,
        std::sync::Weak,
    };

    async fn get_energy_info_resource() -> Result<Resource, Error> {
        let energy_info_resource_provider =
            connect_to_protocol::<fkernel::EnergyInfoResourceMarker>()?;
        let energy_info_resource_handle = energy_info_resource_provider.get().await?;
        Ok(Resource::from(energy_info_resource_handle))
    }

    async fn serve_energy_info_resource() -> Result<fkernel::EnergyInfoResourceProxy, Error> {
        let energy_info_resource = get_energy_info_resource().await?;

        let (proxy, stream) =
            fidl::endpoints::create_proxy_and_stream::<fkernel::EnergyInfoResourceMarker>()?;
        fasync::Task::local(
            EnergyInfoResource::new(energy_info_resource)
                .unwrap_or_else(|e| {
                    panic!("Error while creating energy info resource service: {}", e)
                })
                .serve(stream)
                .unwrap_or_else(|e| {
                    panic!("Error while serving energy info resource service: {}", e)
                }),
        )
        .detach();
        Ok(proxy)
    }

    #[fuchsia::test]
    async fn kind_type_is_energy_info() -> Result<(), Error> {
        let energy_info_resource_provider = serve_energy_info_resource().await?;
        let energy_info_resource: Resource = energy_info_resource_provider.get().await?;
        let resource_info = energy_info_resource.info()?;
        assert_eq!(resource_info.kind, zx::sys::ZX_RSRC_KIND_SYSTEM);
        assert_eq!(resource_info.base, zx::sys::ZX_RSRC_SYSTEM_ENERGY_INFO_BASE);
        assert_eq!(resource_info.size, 1);
        Ok(())
    }

    #[fuchsia::test]
    async fn can_connect_to_energy_info_service() -> Result<(), Error> {
        let energy_info_resource =
            EnergyInfoResource::new(get_energy_info_resource().await?).unwrap();
        let hooks = Hooks::new();
        hooks.install(energy_info_resource.hooks()).await;

        let provider = Arc::new(Mutex::new(None));
        let source = CapabilitySource::Builtin {
            capability: InternalCapability::Protocol(ENERGY_INFO_RESOURCE_CAPABILITY_NAME.clone()),
            top_instance: Weak::new(),
        };

        let event = Event::new_for_test(
            AbsoluteMoniker::root(),
            "fuchsia-pkg://root",
            EventPayload::CapabilityRouted { source, capability_provider: provider.clone() },
        );
        hooks.dispatch(&event).await;

        let (client, mut server) = zx::Channel::create();
        let task_scope = TaskScope::new();
        if let Some(provider) = provider.lock().await.take() {
            provider
                .open(task_scope.clone(), fio::OpenFlags::empty(), PathBuf::new(), &mut server)
                .await?;
        }

        let energy_info_client = ClientEnd::<fkernel::EnergyInfoResourceMarker>::new(client)
            .into_proxy()
            .expect("failed to create launcher proxy");
        let energy_info_resource = energy_info_client.get().await?;
        assert_ne!(energy_info_resource.raw_handle(), sys::ZX_HANDLE_INVALID);
        Ok(())
    }
}
