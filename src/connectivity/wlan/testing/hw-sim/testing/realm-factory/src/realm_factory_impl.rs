// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    anyhow::*,
    fidl::endpoints::ServerEnd,
    fidl_fuchsia_io as fio, fidl_test_wlan_realm as ftest,
    fuchsia_component_test::{
        Capability, ChildOptions, ChildRef, RealmBuilder, RealmInstance, Ref, Route,
    },
    tracing::info,
};

pub(crate) struct RealmFactoryImpl {
    realm_options: Option<ftest::RealmOptions>,
}

impl RealmFactoryImpl {
    pub fn new() -> Self {
        Self { realm_options: Some(ftest::RealmOptions { ..Default::default() }) }
    }

    pub fn set_realm_options(&mut self, options: ftest::RealmOptions) -> Result<(), Error> {
        match self.realm_options {
            None => bail!("the realm has already been created"),
            Some(_) => self.realm_options.replace(options),
        };

        info!("set_realm_options: {:?}", self.realm_options);
        Ok(())
    }

    pub async fn create_realm(&mut self) -> Result<RealmInstance, Error> {
        let realm_options = self.realm_options.take().unwrap();
        let inst = build_realm(realm_options).await?;

        Ok(inst)
    }
}

async fn setup_archivist(builder: &RealmBuilder, wlan_components: &ChildRef) -> Result<(), Error> {
    let archivist = builder
        .add_child("archivist", "#meta/archivist-for-embedding.cm", ChildOptions::new())
        .await?;

    builder
        .add_route(
            Route::new()
                .capability(Capability::protocol_by_name("fuchsia.diagnostics.ArchiveAccessor"))
                .capability(Capability::protocol_by_name("fuchsia.logger.Log"))
                .from(&archivist)
                .to(Ref::parent())
                .to(wlan_components),
        )
        .await?;

    builder
        .add_route(
            Route::new()
                .capability(Capability::protocol_by_name("fuchsia.logger.LogSink"))
                .capability(Capability::protocol_by_name("fuchsia.sys2.EventSource"))
                .capability(Capability::event_stream("directory_ready"))
                .capability(Capability::event_stream("capability_requested"))
                .from(Ref::parent())
                .to(&archivist),
        )
        .await?;

    Ok(())
}

async fn build_realm(mut options: ftest::RealmOptions) -> Result<RealmInstance, Error> {
    info!("building the realm using options {:?}", options);

    let builder = RealmBuilder::new().await?;

    let wlan_components = builder
        .add_child("wlan-hw-sim", "#meta/wlan-hw-sim.cm", ChildOptions::new().eager())
        .await?;

    setup_archivist(&builder, &wlan_components).await?;

    builder
        .add_route(
            Route::new()
                .capability(Capability::protocol_by_name("fuchsia.driver.test.Realm"))
                .capability(Capability::protocol_by_name("fuchsia.wlan.policy.ClientProvider"))
                .capability(Capability::protocol_by_name("fuchsia.wlan.policy.AccessPointProvider"))
                .capability(Capability::protocol_by_name(
                    "fuchsia.wlan.device.service.DeviceMonitor",
                ))
                .capability(Capability::directory("dev-topological"))
                .capability(Capability::directory("dev-class"))
                .from(&wlan_components)
                .to(Ref::parent()),
        )
        .await?;

    builder
        .add_route(
            Route::new()
                .capability(Capability::protocol_by_name("fuchsia.logger.LogSink"))
                .capability(Capability::protocol_by_name("fuchsia.process.Launcher"))
                .capability(Capability::storage("data"))
                .capability(Capability::storage("cache"))
                .from(Ref::parent())
                .to(&wlan_components),
        )
        .await?;

    let realm = builder.build().await?;

    let devfs = options.devfs_server_end.take().unwrap();

    realm.root.get_exposed_dir().open(
        fio::OpenFlags::DIRECTORY,
        fio::ModeType::empty(),
        "dev-topological",
        ServerEnd::new(devfs.into_channel()),
    )?;

    Ok(realm)
}
