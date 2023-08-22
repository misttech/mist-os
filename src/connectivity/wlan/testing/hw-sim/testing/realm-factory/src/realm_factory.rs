// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    anyhow::*,
    fidl::endpoints::ServerEnd,
    fidl_test_wlan_realm as fidl_realm,
    fuchsia_component_test::{
        Capability, ChildOptions, ChildRef, RealmBuilder, RealmBuilderParams, RealmInstance, Ref,
        Route,
    },
    tracing::info,
};

pub(crate) struct WlanTestRealmFactory {
    realm_options: Option<fidl_realm::RealmOptions>,
}

impl WlanTestRealmFactory {
    pub fn new() -> Self {
        Self { realm_options: Some(fidl_realm::RealmOptions { ..Default::default() }) }
    }

    pub fn set_realm_options(&mut self, options: fidl_realm::RealmOptions) -> Result<(), Error> {
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
                .capability(Capability::event_stream("directory_ready"))
                .capability(Capability::event_stream("capability_requested"))
                .from(Ref::parent())
                .to(&archivist),
        )
        .await?;

    Ok(())
}

async fn setup_wlandevicemonitor(
    builder: &RealmBuilder,
    wlan_components: &ChildRef,
    use_legacy_privacy: bool,
) -> Result<(), Error> {
    let wlandevicemonitor = builder
        .add_child("wlandevicemonitor", "#meta/wlandevicemonitor.cm", ChildOptions::new())
        .await?;

    builder.init_mutable_config_to_empty(&wlandevicemonitor).await?;

    builder.set_config_value_bool(&wlandevicemonitor, "wep_supported", use_legacy_privacy).await?;
    builder.set_config_value_bool(&wlandevicemonitor, "wpa1_supported", use_legacy_privacy).await?;

    builder
        .add_route(
            Route::new()
                .capability(
                    Capability::protocol_by_name("fuchsia.wlan.device.service.DeviceMonitor")
                        .weak(),
                )
                .from(&wlandevicemonitor)
                .to(wlan_components),
        )
        .await?;

    builder
        .add_route(
            Route::new()
                .capability(Capability::directory("dev-class").subdir("wlanphy").as_("dev-wlanphy"))
                .from(wlan_components)
                .to(&wlandevicemonitor),
        )
        .await?;

    builder
        .add_route(
            Route::new()
                .capability(Capability::protocol_by_name(
                    "fuchsia.wlan.device.service.DeviceMonitor",
                ))
                .from(&wlandevicemonitor)
                .to(Ref::parent()),
        )
        .await?;

    builder
        .add_route(
            Route::new()
                .capability(Capability::protocol_by_name("fuchsia.logger.LogSink"))
                .from(Ref::parent())
                .to(&wlandevicemonitor),
        )
        .await?;

    Ok(())
}

async fn setup_regulatory_region(
    builder: &RealmBuilder,
    wlan_components: &ChildRef,
) -> Result<(), Error> {
    let regulatory_region = builder
        .add_child("regulatory_region", "#meta/regulatory_region.cm", ChildOptions::new())
        .await?;

    builder
        .add_route(
            Route::new()
                .capability(
                    Capability::protocol_by_name(
                        "fuchsia.location.namedplace.RegulatoryRegionWatcher",
                    )
                    .weak(),
                )
                .from(&regulatory_region)
                .to(wlan_components),
        )
        .await?;

    builder
        .add_route(
            Route::new()
                .capability(Capability::protocol_by_name("fuchsia.logger.LogSink"))
                .capability(Capability::storage("cache"))
                .from(Ref::parent())
                .to(&regulatory_region),
        )
        .await?;

    Ok(())
}

async fn build_realm(mut options: fidl_realm::RealmOptions) -> Result<RealmInstance, Error> {
    info!("building the realm using options {:?}", options);

    let wlan_config =
        options.wlan_config.unwrap_or(fidl_realm::WlanConfig { ..Default::default() });

    let mut params = RealmBuilderParams::new();
    if let Some(name) = wlan_config.name {
        params = params.realm_name(name);
    }
    let builder = RealmBuilder::with_params(params).await?;

    let wlan_components = builder
        .add_child("wlan-hw-sim", "#meta/wlan-hw-sim.cm", ChildOptions::new().eager())
        .await?;

    setup_archivist(&builder, &wlan_components).await?;

    setup_wlandevicemonitor(
        &builder,
        &wlan_components,
        wlan_config.use_legacy_privacy.unwrap_or(false),
    )
    .await?;

    if wlan_config.with_regulatory_region.unwrap_or(true) {
        setup_regulatory_region(&builder, &wlan_components).await?;
    } else {
        info!("No regulatory region");
    }

    builder
        .add_route(
            Route::new()
                .capability(Capability::protocol_by_name("fuchsia.driver.test.Realm"))
                .capability(Capability::protocol_by_name("fuchsia.wlan.policy.ClientProvider"))
                .capability(Capability::protocol_by_name("fuchsia.wlan.policy.AccessPointProvider"))
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
                .from(Ref::parent())
                .to(&wlan_components),
        )
        .await?;

    let realm = builder.build().await?;

    let devfs = options.devfs_server_end.take().unwrap();

    realm.root.get_exposed_dir().open(
        fidl_fuchsia_io::OpenFlags::DIRECTORY,
        fidl_fuchsia_io::ModeType::empty(),
        "dev-topological",
        ServerEnd::new(devfs.into_channel()),
    )?;

    Ok(realm)
}
