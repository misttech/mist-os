// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::emulator::EMULATOR_ROOT_DRIVER_URL;
use crate::host_realm::mpsc::Receiver;
use anyhow::{format_err, Error};
use fidl::endpoints::ClientEnd;
use fidl_fuchsia_bluetooth_host::{HostMarker, ReceiverMarker, ReceiverRequestStream};
use fidl_fuchsia_component::{CreateChildArgs, RealmMarker};
use fidl_fuchsia_component_decl::{
    Child, CollectionRef, ConfigOverride, ConfigSingleValue, ConfigValue, Durability, StartupMode,
};
use fidl_fuchsia_logger::LogSinkMarker;
use fuchsia_bluetooth::constants::{
    BT_HOST, BT_HOST_COLLECTION, BT_HOST_URL, DEV_DIR, HCI_DEVICE_DIR,
};
use fuchsia_component::server::ServiceFs;
use fuchsia_component_test::{
    Capability, ChildOptions, LocalComponentHandles, RealmBuilder, RealmInstance, Ref, Route,
    ScopedInstance,
};
use fuchsia_driver_test::{DriverTestRealmBuilder, DriverTestRealmInstance};
use futures::channel::mpsc;
use futures::{SinkExt, StreamExt};
use std::sync::{Arc, Mutex};
use {fidl_fuchsia_driver_test as fdt, fidl_fuchsia_io as fio};

mod constants {
    pub mod receiver {
        pub const MONIKER: &str = "receiver";
    }
}

pub async fn add_host_routes(
    builder: &RealmBuilder,
    to: impl Into<fuchsia_component_test::Ref> + Clone,
) -> Result<(), Error> {
    // Route config capabilities from root to bt-init
    builder
        .add_capability(cm_rust::CapabilityDecl::Config(cm_rust::ConfigurationDecl {
            name: "fuchsia.bluetooth.LegacyPairing".parse()?,
            value: cm_rust::ConfigValue::Single(cm_rust::ConfigSingleValue::Bool(false)),
        }))
        .await?;
    builder
        .add_capability(cm_rust::CapabilityDecl::Config(cm_rust::ConfigurationDecl {
            name: "fuchsia.bluetooth.ScoOffloadPathIndex".parse()?,
            value: cm_rust::ConfigValue::Single(cm_rust::ConfigSingleValue::Uint8(6)),
        }))
        .await?;
    builder
        .add_capability(cm_rust::CapabilityDecl::Config(cm_rust::ConfigurationDecl {
            name: "fuchsia.power.SuspendEnabled".parse()?,
            value: cm_rust::ConfigValue::Single(cm_rust::ConfigSingleValue::Bool(false)),
        }))
        .await?;

    builder
        .add_route(
            Route::new()
                .capability(Capability::configuration("fuchsia.bluetooth.LegacyPairing"))
                .from(Ref::self_())
                .to(to.clone()),
        )
        .await?;
    builder
        .add_route(
            Route::new()
                .capability(Capability::configuration("fuchsia.bluetooth.ScoOffloadPathIndex"))
                .from(Ref::self_())
                .to(to.clone()),
        )
        .await?;

    builder
        .add_route(
            Route::new()
                .capability(Capability::configuration("fuchsia.power.SuspendEnabled"))
                .from(Ref::self_())
                .to(to.clone()),
        )
        .await?;

    // Add directory routing between components within CoreRealm
    builder
        .add_route(
            Route::new()
                .capability(Capability::directory("dev-class").subdir("bt-hci").as_("dev-bt-hci"))
                .from(Ref::child(fuchsia_driver_test::COMPONENT_NAME))
                .to(to),
        )
        .await?;
    Ok(())
}

pub struct HostRealm {
    realm: RealmInstance,
    receiver: Mutex<Option<Receiver<ClientEnd<HostMarker>>>>,
}

impl HostRealm {
    pub async fn create(test_component: String) -> Result<Self, Error> {
        // We need to resolve our test component manually. Eventually component framework could provide
        // an introspection way of resolving your own component.
        let resolved_test_component = {
            let client = fuchsia_component::client::connect_to_protocol_at_path::<
                fidl_fuchsia_component_resolution::ResolverMarker,
            >("/svc/fuchsia.component.resolution.Resolver-hermetic")
            .unwrap();
            client
                .resolve(test_component.as_str())
                .await
                .unwrap()
                .expect("Failed to resolve test component")
        };

        let builder = RealmBuilder::new().await?;
        let _ = builder.driver_test_realm_setup().await?;

        // Mock the fuchsia.bluetooth.host.Receiver API by creating a channel where the client end
        // of the Host protocol can be extracted from |receiver|.
        // Note: The word "receiver" is overloaded. One refers to the Receiver API, the other
        // refers to the receiver end of the mpsc channel.
        let (sender, receiver) = mpsc::channel(128);
        let host_receiver = builder
            .add_local_child(
                constants::receiver::MONIKER,
                move |handles| {
                    let sender_clone = sender.clone();
                    Box::pin(Self::fake_receiver_component(sender_clone, handles))
                },
                ChildOptions::new().eager(),
            )
            .await?;

        // Create bt-host collection
        let mut realm_decl = builder.get_realm_decl().await?;
        realm_decl.collections.push(cm_rust::CollectionDecl {
            name: BT_HOST_COLLECTION.parse().unwrap(),
            durability: Durability::SingleRun,
            environment: None,
            allowed_offers: cm_types::AllowedOffers::StaticOnly,
            allow_long_names: false,
            persistent_storage: None,
        });
        builder.replace_realm_decl(realm_decl).await.unwrap();

        add_host_routes(&builder, Ref::collection(BT_HOST_COLLECTION.to_string())).await?;

        // Route capabilities between realm components and bt-host-collection
        builder
            .add_route(
                Route::new()
                    .capability(Capability::protocol::<LogSinkMarker>())
                    .capability(Capability::dictionary("diagnostics"))
                    .from(Ref::parent())
                    .to(Ref::collection(BT_HOST_COLLECTION.to_string())),
            )
            .await?;
        builder
            .add_route(
                Route::new()
                    .capability(Capability::protocol::<ReceiverMarker>())
                    .from(&host_receiver)
                    .to(Ref::collection(BT_HOST_COLLECTION.to_string())),
            )
            .await?;
        builder
            .add_route(
                Route::new()
                    .capability(Capability::protocol::<RealmMarker>())
                    .from(Ref::framework())
                    .to(Ref::parent()),
            )
            .await?;

        let instance = builder.build().await?;

        // Start DriverTestRealm
        let args = fdt::RealmArgs {
            root_driver: Some(EMULATOR_ROOT_DRIVER_URL.to_string()),
            software_devices: Some(vec![fidl_fuchsia_driver_test::SoftwareDevice {
                device_name: "bt-hci-emulator".to_string(),
                device_id: bind_fuchsia_platform::BIND_PLATFORM_DEV_DID_BT_HCI_EMULATOR,
            }]),
            test_component: Some(resolved_test_component),
            ..Default::default()
        };
        instance.driver_test_realm_start(args).await?;

        Ok(Self { realm: instance, receiver: Some(receiver).into() })
    }

    // Create bt-host component with |filename| and add it to bt-host collection in HostRealm.
    // Wait for the component to register itself with Receiver and get the client end of the Host
    // protocol.
    pub async fn create_bt_host_in_collection(
        realm: &Arc<HostRealm>,
        filename: &str,
    ) -> Result<ClientEnd<HostMarker>, Error> {
        let component_name = format!("{BT_HOST}_{filename}"); // Name must only contain [a-z0-9-_]
        let device_path = format!("{DEV_DIR}/{HCI_DEVICE_DIR}/{filename}");
        let collection_ref = CollectionRef { name: BT_HOST_COLLECTION.to_owned() };
        let child_decl = Child {
            name: Some(component_name.to_owned()),
            url: Some(BT_HOST_URL.to_owned()),
            startup: Some(StartupMode::Lazy),
            config_overrides: Some(vec![ConfigOverride {
                key: Some("device_path".to_string()),
                value: Some(ConfigValue::Single(ConfigSingleValue::String(
                    device_path.to_string(),
                ))),
                ..ConfigOverride::default()
            }]),
            ..Default::default()
        };

        let realm_proxy =
            realm.instance().connect_to_protocol_at_exposed_dir::<RealmMarker>().unwrap();
        let _ = realm_proxy
            .create_child(&collection_ref, &child_decl, CreateChildArgs::default())
            .await
            .map_err(|e| format_err!("{e:?}"))?
            .map_err(|e| format_err!("{e:?}"))?;

        let host = realm.receiver().next().await.unwrap();
        Ok(host)
    }

    async fn fake_receiver_component(
        sender: mpsc::Sender<ClientEnd<HostMarker>>,
        handles: LocalComponentHandles,
    ) -> Result<(), Error> {
        let mut fs = ServiceFs::new();
        let _ = fs.dir("svc").add_fidl_service(move |mut req_stream: ReceiverRequestStream| {
            let mut sender_clone = sender.clone();
            fuchsia_async::Task::local(async move {
                let (host_server, _) =
                    req_stream.next().await.unwrap().unwrap().into_add_host().unwrap();
                sender_clone.send(host_server).await.expect("Host sent successfully");
            })
            .detach()
        });

        let _ = fs.serve_connection(handles.outgoing_dir)?;
        fs.collect::<()>().await;
        Ok(())
    }

    pub fn instance(&self) -> &ScopedInstance {
        &self.realm.root
    }

    pub fn dev(&self) -> Result<fio::DirectoryProxy, Error> {
        self.realm.driver_test_realm_connect_to_dev()
    }

    pub fn receiver(&self) -> Receiver<ClientEnd<HostMarker>> {
        self.receiver.lock().expect("REASON").take().unwrap()
    }
}
