// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    anyhow::{Error, Result},
    fidl::endpoints::{create_endpoints, ServerEnd},
    fidl_fuchsia_inspect::InspectSinkMarker,
    fidl_fuchsia_io as fio,
    fidl_fuchsia_settings::PrivacyMarker,
    fidl_fuchsia_settings_test::*,
    fidl_fuchsia_stash::StoreMarker,
    fuchsia_async as fasync,
    fuchsia_component::server::ServiceFs,
    fuchsia_component_test::{
        Capability, ChildOptions, LocalComponentHandles, RealmBuilder, RealmInstance, Ref, Route,
    },
    futures::{StreamExt, TryStreamExt},
    std::sync::Arc,
    tracing::*,
    vfs::{directory::entry_container::Directory, file::vmo::read_only, pseudo_directory},
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
    while let Ok(Some(request)) = stream.try_next().await {
        match request {
            RealmFactoryRequest::CreateRealm { options, realm_server, responder } => {
                let realm = create_realm(options).await?;
                let request_stream = realm_server.into_stream()?;
                task_group.spawn(async move {
                    realm_proxy::service::serve(realm, request_stream).await.unwrap();
                });
                responder.send(Ok(()))?;
            }

            RealmFactoryRequest::_UnknownMethod { .. } => unreachable!(),
        }
    }

    task_group.join().await;
    Ok(())
}

async fn create_realm(_: RealmOptions) -> Result<RealmInstance, Error> {
    let builder = RealmBuilder::new().await?;
    let setui =
        builder.add_child("setui_service", "#meta/setui_service.cm", ChildOptions::new()).await?;
    let stash = builder.add_child("stash", "#meta/stash.cm", ChildOptions::new().eager()).await?;
    let config_data = builder
        .add_local_child(
            "config_data",
            move |handles| Box::pin(serve_config_data(handles)),
            ChildOptions::new(),
        )
        .await?;

    builder
        .add_route(
            Route::new()
                .capability(
                    Capability::directory("config-data")
                        .path("/config/data")
                        .rights(fio::R_STAR_DIR),
                )
                .from(&config_data)
                .to(&setui)
                .to(&stash),
        )
        .await?;

    builder
        .add_route(
            Route::new()
                .capability(Capability::protocol::<InspectSinkMarker>())
                .capability(Capability::storage("data"))
                .from(Ref::parent())
                .to(&setui)
                .to(&stash),
        )
        .await?;

    builder
        .add_route(
            Route::new().capability(Capability::protocol::<StoreMarker>()).from(&stash).to(&setui),
        )
        .await?;

    builder
        .add_route(
            Route::new()
                .capability(Capability::protocol::<PrivacyMarker>())
                .from(&setui)
                .to(Ref::parent()),
        )
        .await?;

    let realm = builder.build().await?;
    Ok(realm)
}

async fn serve_config_data(handles: LocalComponentHandles) -> Result<(), Error> {
    // TODO(b/307569251): Inject this data from the test.
    let config_data_dir = pseudo_directory! {
        "data" => pseudo_directory! {
            // TODO(311204355): Docs should exist to explain how to use these JSON files.
            "interface_configuration.json" => read_only(r#"{"interfaces": ["Privacy"]}"#),
            "service_flags.json" => read_only(r#"{"controller_flags": []}"#),
            "board_agent_configuration.json" => read_only(r#"{"agent_types": []}"#),
            "agent_configuration.json" => read_only(r#"{"agent_types": []}"#),
        },
    };
    let mut fs = ServiceFs::new();
    fs.add_remote("config", spawn_vfs(config_data_dir));
    fs.serve_connection(handles.outgoing_dir).expect("failed to serve config-data");
    fs.collect::<()>().await;
    Ok(())
}

// Returns a `DirectoryProxy` that serves the directory entry `dir`.
fn spawn_vfs(dir: Arc<dyn Directory>) -> fio::DirectoryProxy {
    let (client_end, server_end) = create_endpoints::<fio::DirectoryMarker>();
    let scope = vfs::execution_scope::ExecutionScope::new();
    dir.open(
        scope,
        fio::OpenFlags::RIGHT_READABLE | fio::OpenFlags::RIGHT_WRITABLE,
        vfs::path::Path::dot(),
        ServerEnd::new(server_end.into_channel()),
    );
    client_end.into_proxy().unwrap()
}
