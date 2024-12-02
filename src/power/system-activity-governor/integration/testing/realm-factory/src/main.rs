// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::{Error, Result};
use fidl::endpoints::ServerEnd;
use fidl_fuchsia_testing_harness::RealmProxy_Marker;
use fidl_test_systemactivitygovernor::*;
use fuchsia_async as fasync;
use fuchsia_component::server::ServiceFs;
use fuchsia_component_test::{
    Capability, ChildOptions, RealmBuilder, RealmInstance, Ref, Route, DEFAULT_COLLECTION_NAME,
};
use futures::{StreamExt, TryStreamExt};
use tracing::*;

const ACTIVITY_GOVERNOR_CHILD_NAME: &str = "system-activity-governor";

#[fuchsia::main]
async fn main() -> Result<(), Error> {
    let mut fs = ServiceFs::new();
    fs.dir("svc").add_fidl_service(|stream: RealmFactoryRequestStream| stream);
    fs.take_and_serve_directory_handle()?;
    fs.for_each_concurrent(0, serve_realm_factory).await;
    Ok(())
}

async fn serve_realm_factory(stream: RealmFactoryRequestStream) {
    if let Err(err) = handle_request_stream(stream).await {
        error!("{:?}", err);
    }
}

async fn handle_request_stream(mut stream: RealmFactoryRequestStream) -> Result<()> {
    let mut task_group = fasync::TaskGroup::new();
    while let Ok(Some(request)) = stream.try_next().await {
        match request {
            RealmFactoryRequest::CreateRealm { realm_server, responder } => {
                let realm = create_realm(RealmOptions::default()).await?;
                responder.send(Ok(&realm.moniker()))?;
                task_group.spawn(realm.serve(realm_server));
            }
            RealmFactoryRequest::CreateRealmExt { options, realm_server, responder } => {
                let realm = create_realm(options).await?;
                responder.send(Ok(&realm.moniker()))?;
                task_group.spawn(realm.serve(realm_server));
            }
            RealmFactoryRequest::_UnknownMethod { .. } => unreachable!(),
        }
    }

    task_group.join().await;
    Ok(())
}

struct SagRealm {
    realm: RealmInstance,
}

impl SagRealm {
    fn moniker(&self) -> String {
        format!(
            "{}:{}/{}",
            DEFAULT_COLLECTION_NAME,
            self.realm.root.child_name(),
            ACTIVITY_GOVERNOR_CHILD_NAME
        )
    }

    async fn serve(self, server_end: ServerEnd<RealmProxy_Marker>) {
        realm_proxy::service::serve(self.realm, server_end.into_stream()).await.unwrap()
    }
}

async fn create_realm(options: RealmOptions) -> Result<SagRealm, Error> {
    info!("building the realm");

    let use_fake_sag = options.use_fake_sag.unwrap_or(false);
    let wait_for_suspending_token = options.wait_for_suspending_token.unwrap_or(false);
    let use_suspender = options.use_suspender.unwrap_or(true);

    let builder = RealmBuilder::new().await?;

    let component_ref = builder
        .add_child(
            ACTIVITY_GOVERNOR_CHILD_NAME,
            if use_fake_sag {
                "fake-system-activity-governor#meta/fake-system-activity-governor.cm"
            } else {
                "#meta/system-activity-governor.cm"
            },
            ChildOptions::new(),
        )
        .await?;

    let power_broker_ref =
        builder.add_child("power-broker", "#meta/power-broker.cm", ChildOptions::new()).await?;

    let fake_suspend_ref =
        builder.add_child("fake-suspend", "#meta/fake-suspend.cm", ChildOptions::new()).await?;

    // Expose capabilities from power-broker.
    builder
        .add_route(
            Route::new()
                .capability(Capability::protocol_by_name("fuchsia.power.broker.Topology"))
                .from(&power_broker_ref)
                .to(Ref::parent()),
        )
        .await?;

    // Expose capabilities from fake-suspend.
    builder
        .add_route(
            Route::new()
                .capability(Capability::protocol_by_name("test.suspendcontrol.Device"))
                .from(&fake_suspend_ref)
                .to(Ref::parent()),
        )
        .await?;

    // Expose capabilities from power-broker to system-activity-governor.
    builder
        .add_route(
            Route::new()
                .capability(Capability::protocol_by_name("fuchsia.power.broker.Topology"))
                .from(&power_broker_ref)
                .to(&component_ref),
        )
        .await?;

    // Expose capabilities from fake-suspend to system-activity-governor.
    builder
        .add_route(
            Route::new()
                .capability(Capability::service_by_name("fuchsia.hardware.suspend.SuspendService"))
                .from(&fake_suspend_ref)
                .to(&component_ref),
        )
        .await?;

    // Expose config capabilities to system-activity-governor.
    builder
        .add_capability(cm_rust::CapabilityDecl::Config(cm_rust::ConfigurationDecl {
            name: "fuchsia.power.UseSuspender".parse()?,
            value: use_suspender.into(),
        }))
        .await?;

    builder
        .add_route(
            Route::new()
                .capability(Capability::configuration("fuchsia.power.UseSuspender"))
                .from(Ref::self_())
                .to(&component_ref),
        )
        .await?;

    builder
        .add_capability(cm_rust::CapabilityDecl::Config(cm_rust::ConfigurationDecl {
            name: "fuchsia.power.WaitForSuspendingToken".parse()?,
            value: wait_for_suspending_token.into(),
        }))
        .await?;

    builder
        .add_route(
            Route::new()
                .capability(Capability::configuration("fuchsia.power.WaitForSuspendingToken"))
                .from(Ref::self_())
                .to(&component_ref),
        )
        .await?;

    // Expose capabilities from system-activity-governor.
    builder
        .add_route(
            Route::new()
                .capability(Capability::protocol_by_name("fuchsia.power.suspend.Stats"))
                .capability(Capability::protocol_by_name("fuchsia.power.system.ActivityGovernor"))
                .capability(Capability::protocol_by_name("fuchsia.power.system.BootControl"))
                .capability(Capability::protocol_by_name("fuchsia.power.system.CpuElementManager"))
                .capability(Capability::service_by_name(
                    "fuchsia.power.broker.ElementInfoProviderService",
                ))
                .from(&component_ref)
                .to(Ref::parent()),
        )
        .await?;

    if use_fake_sag {
        builder
            .add_route(
                Route::new()
                    .capability(Capability::protocol_by_name("test.sagcontrol.State"))
                    .from(&component_ref)
                    .to(Ref::parent()),
            )
            .await?;
    }

    let realm = builder.build().await?;
    Ok(SagRealm { realm })
}
