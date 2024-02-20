// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    crate::{emulator::EMULATOR_ROOT_DRIVER_URL, host_realm::mpsc::Receiver},
    anyhow::{format_err, Error},
    cm_types,
    fidl::endpoints::ClientEnd,
    fidl_fuchsia_bluetooth_host::{HostMarker, ReceiverMarker, ReceiverRequestStream},
    fidl_fuchsia_component::{CreateChildArgs, RealmMarker},
    fidl_fuchsia_component_decl::{
        Child, CollectionRef, ConfigOverride, ConfigSingleValue, ConfigValue, Durability,
        StartupMode,
    },
    fidl_fuchsia_driver_test as fdt, fidl_fuchsia_io as fio,
    fidl_fuchsia_logger::LogSinkMarker,
    fuchsia_bluetooth::constants::{
        BT_HOST, BT_HOST_COLLECTION, BT_HOST_URL, DEV_DIR, HCI_DEVICE_DIR,
    },
    fuchsia_component::server::ServiceFs,
    fuchsia_component_test::{
        Capability, ChildOptions, LocalComponentHandles, RealmBuilder, RealmInstance, Ref, Route,
        ScopedInstance,
    },
    fuchsia_driver_test::{DriverTestRealmBuilder, DriverTestRealmInstance},
    futures::{channel::mpsc, SinkExt, StreamExt},
    std::sync::{Arc, Mutex},
};

mod constants {
    pub mod receiver {
        pub const MONIKER: &str = "receiver";
    }
}

pub struct HostRealm {
    realm: RealmInstance,
    receiver: Mutex<Option<Receiver<ClientEnd<HostMarker>>>>,
}

impl HostRealm {
    pub async fn create() -> Result<Self, Error> {
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

        // Route capabilities between realm components and bt-host-collection
        builder
            .add_route(
                Route::new()
                    .capability(Capability::protocol::<LogSinkMarker>())
                    .from(Ref::parent())
                    .to(Ref::collection(BT_HOST_COLLECTION.to_string())),
            )
            .await?;
        builder
            .add_route(
                Route::new()
                    .capability(
                        Capability::directory("dev-class").subdir("bt-hci").as_("dev-bt-hci"),
                    )
                    .from(Ref::child(fuchsia_driver_test::COMPONENT_NAME))
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
