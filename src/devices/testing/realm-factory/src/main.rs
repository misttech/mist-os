// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    anyhow::{Context, Error, Result},
    fidl::endpoints::{ClientEnd, ControlHandle, ServerEnd},
    fidl_fuchsia_driver_test as fdt,
    fidl_fuchsia_driver_testing::*,
    fidl_fuchsia_io as fio,
    fidl_fuchsia_testing_harness::OperationError,
    fuchsia_async as fasync,
    fuchsia_component::directory::AsRefDirectory,
    fuchsia_component::server::ServiceFs,
    fuchsia_component_test::{
        Capability, ChildOptions, LocalComponentHandles, RealmBuilder, RealmInstance, Ref, Route,
    },
    futures::{FutureExt, StreamExt, TryStreamExt},
    std::sync::Arc,
    tracing::*,
};

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
    let mut realms = vec![];
    while let Ok(Some(request)) = stream.try_next().await {
        match request {
            RealmFactoryRequest::CreateRealm { options, realm_server, responder } => {
                let realm_result = create_realm(options).await;
                match realm_result {
                    Ok(realm) => {
                        let request_stream = realm_server.into_stream()?;
                        task_group.spawn(async move {
                            realm_proxy::service::serve(realm, request_stream).await.unwrap();
                        });

                        responder.send(Ok(()))?;
                    }
                    Err(e) => {
                        error!("Failed to create realm: {:?}", e);
                        responder.send(Err(OperationError::Invalid))?;
                    }
                }
            }
            RealmFactoryRequest::CreateRealm2 { options, dictionary, responder } => {
                let realm_result = create_realm(options).await;
                match realm_result {
                    Ok(realm) => {
                        realm.root.controller().get_exposed_dictionary(dictionary).await?.unwrap();
                        realms.push(realm);
                        responder.send(Ok(()))?;
                    }
                    Err(e) => {
                        error!("Failed to create realm: {:?}", e);
                        responder.send(Err(OperationError::Invalid))?;
                    }
                }
            }

            RealmFactoryRequest::_UnknownMethod { control_handle, .. } => {
                control_handle.shutdown_with_epitaph(fuchsia_zircon_status::Status::NOT_SUPPORTED);
            }
        }
    }

    task_group.join().await;
    Ok(())
}

async fn run_offers_forward(
    handles: LocalComponentHandles,
    client: Arc<ClientEnd<fio::DirectoryMarker>>,
) -> Result<(), Error> {
    let mut fs = ServiceFs::new();

    let (proxy, server_end) = fidl::endpoints::create_proxy::<fio::DirectoryMarker>()?;
    client.as_ref_directory().open(
        "svc",
        fio::OpenFlags::empty(),
        server_end.into_channel().into(),
    )?;
    fs.add_remote("svc", proxy);
    fs.serve_connection(handles.outgoing_dir)?;
    Ok(fs.collect::<()>().await)
}

async fn add_dynamic_expose(
    expose: String,
    builder: &RealmBuilder,
    driver_test_realm_component_name: &str,
    driver_test_realm_ref: &fuchsia_component_test::ChildRef,
) -> Result<(), Error> {
    let expose_parsed =
        expose.parse::<cm_types::Name>().expect("service name is not a valid capability name");

    // Dynamic exposes are not hardcoded in the driver test realm cml so we have to add them here.
    // The source of this capability is the driver test realm itself because it has manually
    // forwarded all of the requested exposes from its inner realm.
    let mut decl = builder.get_component_decl(driver_test_realm_component_name).await?;
    decl.capabilities.push(cm_rust::CapabilityDecl::Service(cm_rust::ServiceDecl {
        name: expose_parsed.clone(),
        source_path: Some(
            ("/svc/".to_owned() + &expose)
                .parse::<cm_types::Path>()
                .expect("service name is not a valid capability name"),
        ),
    }));
    decl.exposes.push(cm_rust::ExposeDecl::Service(cm_rust::ExposeServiceDecl {
        source: cm_rust::ExposeSource::Self_,
        source_name: expose_parsed.clone(),
        source_dictionary: Default::default(),
        target_name: expose_parsed.clone(),
        target: cm_rust::ExposeTarget::Parent,
        availability: cm_rust::Availability::Required,
    }));
    builder.replace_component_decl(driver_test_realm_component_name, decl).await?;

    // Add the route through the realm builder.
    builder
        .add_route(
            Route::new()
                .capability(Capability::service_by_name(&expose))
                .from(driver_test_realm_ref)
                .to(Ref::parent()),
        )
        .await?;
    Ok(())
}

async fn add_dynamic_offer(
    offer: String,
    builder: &RealmBuilder,
    offer_provider_ref: &fuchsia_component_test::ChildRef,
    driver_test_realm_component_name: &str,
    driver_test_realm_ref: &fuchsia_component_test::ChildRef,
) -> Result<(), Error> {
    let offer_parsed =
        offer.parse::<cm_types::Name>().expect("offer name is not a valid capability name");

    // Dynamic offers are not hardcoded in the driver test realm cml so we have to add them here.
    // The target of this capability is the realm_builder collection which is where the
    // driver_test_realm's own realm builder will spin up the driver framework and drivers.
    let mut decl = builder.get_component_decl(driver_test_realm_component_name).await?;
    decl.offers.push(cm_rust::OfferDecl::Protocol(cm_rust::OfferProtocolDecl {
        source: cm_rust::OfferSource::Parent,
        source_name: offer_parsed.clone(),
        source_dictionary: Default::default(),
        target_name: offer_parsed.clone(),
        target: cm_rust::OfferTarget::Collection(
            "realm_builder".parse::<cm_types::Name>().unwrap(),
        ),
        dependency_type: cm_rust::DependencyType::Strong,
        availability: cm_rust::Availability::Required,
    }));
    builder.replace_component_decl(driver_test_realm_component_name, decl).await?;

    // Add the route through the realm builder.
    builder
        .add_route(
            Route::new()
                .capability(Capability::protocol_by_name(&offer))
                .from(offer_provider_ref)
                .to(driver_test_realm_ref),
        )
        .await?;
    Ok(())
}

async fn add_capabilities(
    builder: &RealmBuilder,
    driver_test_realm_ref: fuchsia_component_test::ChildRef,
    driver_test_realm_start_args: &Option<fdt::RealmArgs>,
    offers_client: Option<ClientEnd<fio::DirectoryMarker>>,
    driver_test_realm_component_name: &str,
) -> Result<(), Error> {
    builder
        .add_route(
            Route::new()
                .capability(Capability::directory("dev-topological").rights(fio::R_STAR_DIR))
                .capability(Capability::directory("dev-class").rights(fio::R_STAR_DIR))
                .from(&driver_test_realm_ref)
                .to(Ref::parent()),
        )
        .await?;

    builder
        .add_route(
            Route::new()
                .capability(Capability::protocol_by_name("fuchsia.device.manager.Administrator"))
                .capability(Capability::protocol_by_name(
                    "fuchsia.driver.development.DriverDevelopment",
                ))
                .capability(Capability::protocol_by_name(
                    "fuchsia.driver.registrar.DriverRegistrar",
                ))
                .capability(Capability::protocol_by_name("fuchsia.driver.test.Realm"))
                .from(&driver_test_realm_ref)
                .to(Ref::parent()),
        )
        .await?;

    builder
        .add_route(
            Route::new()
                .capability(Capability::protocol_by_name("fuchsia.logger.LogSink"))
                .capability(Capability::protocol_by_name("fuchsia.process.Launcher"))
                .capability(Capability::protocol_by_name("fuchsia.inspect.InspectSink"))
                .from(Ref::parent())
                .to(&driver_test_realm_ref),
        )
        .await?;

    let exposes =
        driver_test_realm_start_args.as_ref().and_then(|args| args.exposes.as_ref()).and_then(
            |exposes| Some(exposes.iter().map(|e| e.service_name.clone()).collect::<Vec<_>>()),
        );

    if let Some(exposes) = exposes {
        for expose in exposes {
            add_dynamic_expose(
                expose,
                builder,
                driver_test_realm_component_name,
                &driver_test_realm_ref,
            )
            .await?;
        }
    }

    let offers =
        driver_test_realm_start_args.as_ref().and_then(|args| args.offers.as_ref()).and_then(
            |offers| Some(offers.iter().map(|o| o.protocol_name.clone()).collect::<Vec<_>>()),
        );

    if let Some(offers) = offers {
        let client = offers_client.expect("Offers provided without an offers_client.");
        let arc_client = Arc::new(client);
        let offer_provider_ref = builder
            .add_local_child(
                "offer_provider",
                move |handles: LocalComponentHandles| {
                    run_offers_forward(handles, arc_client.clone()).boxed()
                },
                ChildOptions::new(),
            )
            .await?;

        for offer in offers {
            add_dynamic_offer(
                offer,
                builder,
                &offer_provider_ref,
                driver_test_realm_component_name,
                &driver_test_realm_ref,
            )
            .await?;
        }
    }

    Ok(())
}

async fn create_realm(options: RealmOptions) -> Result<RealmInstance, Error> {
    info!("building the realm using options {:?}", options);
    let driver_test_realm_component_name = "driver_test_realm";
    let url = options.driver_test_realm_url.unwrap_or("#meta/driver_test_realm.cm".to_string());

    // Add the driver_test_realm child.
    let builder = RealmBuilder::new().await?;
    let driver_test_realm_ref = builder
        .add_child(driver_test_realm_component_name, url, ChildOptions::new().eager())
        .await?;

    // Adds the capabilities and routes we need for the realm.
    add_capabilities(
        &builder,
        driver_test_realm_ref,
        &options.driver_test_realm_start_args,
        options.offers_client,
        driver_test_realm_component_name,
    )
    .await?;

    // Build the realm.
    let realm = builder.build().await?;

    // Connect to the realm and start it.
    let start_args = options
        .driver_test_realm_start_args
        .expect("No driver_test_realm_start_args was provided.");

    let dtr = realm.root.connect_to_protocol_at_exposed_dir::<fdt::RealmMarker>()?;
    dtr.start(start_args)
        .await?
        .map_err(fuchsia_zircon_status::Status::from_raw)
        .context("DriverTestRealm Start failed")?;

    // Connect dev-class.
    if let Some(dev_class) = options.dev_class {
        realm.root.get_exposed_dir().open(
            fio::OpenFlags::DIRECTORY,
            fio::ModeType::empty(),
            "dev-class",
            ServerEnd::new(dev_class.into_channel()),
        )?;
    }

    // Connect dev-topological.
    if let Some(dev_topological) = options.dev_topological {
        realm.root.get_exposed_dir().open(
            fio::OpenFlags::DIRECTORY,
            fio::ModeType::empty(),
            "dev-topological",
            ServerEnd::new(dev_topological.into_channel()),
        )?;
    }

    Ok(realm)
}
