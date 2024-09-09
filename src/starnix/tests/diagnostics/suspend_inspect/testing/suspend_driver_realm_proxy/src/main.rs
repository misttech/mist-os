// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::{Context, Result};
use fidl_fuchsia_testing_harness::RealmProxy_RequestStream;
use fuchsia_component::server::ServiceFs;
use fuchsia_component_test::{
    Capability, ChildOptions, LocalComponentHandles, RealmBuilder, RealmInstance, Ref, Route,
};
use fuchsia_driver_test::{DriverTestRealmBuilder, DriverTestRealmInstance};
use futures_util::{StreamExt, TryStreamExt};
use std::rc::Rc;
use {
    fidl_fuchsia_component_test as ffct, fidl_fuchsia_driver_test as fdt,
    fidl_fuchsia_hardware_suspend as ffhs, fidl_fuchsia_kernel as ffk,
    fidl_fuchsia_test_syscalls as ffts, fuchsia_async as fasync, fuchsia_zircon as zx,
};

enum IncomingService {
    /// The RealmProxy allows serving any protocol by name.
    RealmProxy(RealmProxy_RequestStream),
}

// Since our suspend "driver" isn't really a driver, we need a custom test realm
// manifest for the driver test realm.
const DRIVER_TEST_REALM_MANIFEST: &str = "#meta/suspend_driver_test_realm.cm";

// fuchsia.kernel.CpuResource
async fn serve_fake_cpu_resource(mock_handles: LocalComponentHandles) -> Result<()> {
    let mut fs = ServiceFs::new();

    fs.dir("svc").add_fidl_service(move |mut stream: ffk::CpuResourceRequestStream| {
        fasync::Task::local(async move {
            while let Some(ffk::CpuResourceRequest::Get { responder }) =
                stream.try_next().await.expect("failed to serve CpuResource service")
            {
                // This is adequate so long as we don't actually call zx_system_suspend_enter.
                let handle = zx::Handle::invalid();
                responder.send(handle.into()).expect("failed to send a fake CpuResource handle");
            }
        })
        .detach();
    });

    // Run the ServiceFs on the outgoing directory handle from the mock handles
    fs.serve_connection(mock_handles.outgoing_dir)?;
    fs.collect::<()>().await;
    Ok(())
}

async fn setup_driver_realm_builder() -> Result<RealmBuilder> {
    let realm_builder = RealmBuilder::new().await?;
    // This initializes static routes from a fixed textual manifest.
    realm_builder
        //.driver_test_realm_manifest_setup(DRIVER_TEST_REALM_MANIFEST)
        .driver_test_realm_manifest_setup(DRIVER_TEST_REALM_MANIFEST)
        .await
        .context("while adding the static routes")?;

    // TODO: fmil - How do I provide from here to #driver_test_realm?
    let fake_cpuresource = realm_builder
        .add_local_child(
            "fake_cpuresource",
            |mock_handles: LocalComponentHandles| Box::pin(serve_fake_cpu_resource(mock_handles)),
            ChildOptions::new(),
        )
        .await?;
    realm_builder
        .add_route(
            Route::new()
                .capability(Capability::protocol::<ffk::CpuResourceMarker>())
                .from(&fake_cpuresource)
                .to(Ref::parent()),
        )
        .await?;

    realm_builder.driver_test_realm_add_offer::<ffk::CpuResourceMarker>(Ref::parent()).await?;

    // These are the services exposed by the driver test realm in devfs.
    // Extract them for use "here".
    realm_builder
        .driver_test_realm_add_expose::<ffhs::SuspendServiceMarker>()
        .await
        .context("while exposing SuspendService")?;
    realm_builder
        .driver_test_realm_add_expose::<ffts::ControlServiceMarker>()
        .await
        .context("while exposing ControlService")?;
    Ok(realm_builder)
}

async fn create_realm() -> Result<RealmInstance> {
    let realm_builder =
        setup_driver_realm_builder().await.context("while setting up realm builder")?;
    let driver_realm = realm_builder.build().await.context("while building realm")?;
    driver_realm
        .driver_test_realm_start(fdt::RealmArgs {
            // Offered from driver's internal test_realm to drivers.
            dtr_offers: Some(vec![
                ffct::Capability::Protocol(ffct::Protocol {
                    name: Some("fuchsia.kernel.CpuResource".into()),
                    //from_dictionary: Some("parent".into()),
                    ..ffct::Protocol::default()
                }),
                ffct::Capability::Protocol(ffct::Protocol {
                    name: Some("fuchsia.boot.WriteOnlyLog".into()),
                    ..ffct::Protocol::default()
                }),
            ]),
            // Exposed from driver's internal test_realm to driver_test_realm.
            dtr_exposes: Some(vec![
                ffct::Capability::Service(ffct::Service {
                    name: Some("fuchsia.test.syscalls.ControlService".into()),
                    ..ffct::Service::default()
                }),
                ffct::Capability::Service(ffct::Service {
                    name: Some("fuchsia.hardware.suspend.SuspendService".into()),
                    ..ffct::Service::default()
                }),
            ]),
            ..fdt::RealmArgs::default()
        })
        .await
        .context("while starting driver realm")?;
    Ok(driver_realm)
}

struct RealmServer {}

impl RealmServer {
    fn new() -> Rc<Self> {
        Rc::new(Self {})
    }

    async fn serve_realm_factory(self: Rc<Self>, service: IncomingService) {
        match service {
            IncomingService::RealmProxy(stream) => {
                // Task needed because realm_proxy::service::serve is blocking,
                // and we do not want to block here.
                fasync::Task::local(async move {
                    let realm = create_realm().await.expect("failed to build the test realm");
                    realm_proxy::service::serve(realm, stream)
                        .await
                        .expect("failed to serve the realm proxy");
                })
                .detach();
            }
        };
    }
}

#[fuchsia::main(logging_tags = ["suspend_driver_realm_proxy", "test"])]
async fn main() -> Result<()> {
    let mut fs = ServiceFs::new();
    fs.dir("svc").add_fidl_service(IncomingService::RealmProxy);
    fs.take_and_serve_directory_handle()?;

    // Maybe simplify.
    let server = RealmServer::new();
    tracing::info!("now serving suspend driver realm");
    fs.for_each(|stream| async {
        server.clone().serve_realm_factory(stream).await;
    })
    .await;
    Ok(())
}
