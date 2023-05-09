// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    anyhow::Error,
    fidl::endpoints::create_proxy,
    fidl_fuchsia_logger::LogSinkMarker,
    fidl_fuchsia_scheduler::ProfileProviderMarker,
    fidl_fuchsia_sysmem::AllocatorMarker,
    fidl_fuchsia_tracing_provider::RegistryMarker,
    fidl_fuchsia_ui_composition::FlatlandMarker,
    fidl_fuchsia_ui_display_singleton::InfoMarker,
    fidl_fuchsia_ui_input3::KeyboardMarker,
    fidl_fuchsia_ui_test_conformance as ui_conformance,
    fidl_fuchsia_ui_test_context as ui_test_context, fidl_fuchsia_ui_test_input as ui_input,
    fidl_fuchsia_ui_test_scene as test_scene,
    fidl_fuchsia_vulkan_loader::LoaderMarker,
    fuchsia_component::server::ServiceFs,
    fuchsia_component_test::{Capability, ChildOptions, RealmBuilder, RealmInstance, Ref, Route},
    fuchsia_scenic as scenic,
    futures::{StreamExt, TryStreamExt},
};

// TODO(fxbug.dev/117852): Use subpackages here.
const TEST_UI_STACK: &str = "ui";
const TEST_UI_STACK_URL: &str = "#meta/test-ui-stack.cm";
const PUPPET_UNDER_TEST_FACTORY: &str = "puppet-under-test-factory";
const PUPPET_UNDER_TEST_FACTORY_URL: &str = "#meta/ui-puppet.cm";
const PUPPET_UNDER_TEST_FACTORY_SERVICE: &str = "puppet-under-test-factory-service";
const AUXILIARY_PUPPET_FACTORY: &str = "auxiliary-puppet-factory";
const AUXILIARY_PUPPET_FACTORY_URL: &str = "#meta/ui-puppet.cm";
const AUXILIARY_PUPPET_FACTORY_SERVICE: &str = "auxiliary-puppet-factory-service";

#[fuchsia::main(logging_tags = ["ui_launcher"])]
async fn main() -> Result<(), Error> {
    let mut fs = ServiceFs::new_local();
    fs.dir("svc").add_fidl_service(std::convert::identity);
    fs.take_and_serve_directory_handle()?;
    fs.for_each_concurrent(None, run_factory_server).await;
    Ok(())
}

async fn run_factory_server(stream: ui_test_context::FactoryRequestStream) {
    stream
        .try_for_each_concurrent(None, |request| async {
            match request {
                ui_test_context::FactoryRequest::Create { payload, .. } => {
                    let context_server =
                        payload.context_server.expect("missing context server endpoint");
                    run_context_server(
                        context_server.into_stream().expect("invalid context server"),
                    )
                    .await
                    .expect("failed to run context server");
                }
            }

            Ok(())
        })
        .await
        .expect("failed to serve test context factory request stream");
}

// Serve the test suite protocol.
async fn run_context_server(
    mut stream: ui_test_context::ContextRequestStream,
) -> Result<(), Error> {
    // Create the puppet + ui stack for this test context.
    let puppet_realm = assemble_puppet_realm().await;

    // We only allow one "root puppet" per test case, so we will fail if we see
    // multiple `GetRootViewToken` requests.
    let mut created_root_view_token = false;

    // We only allow one "puppet under test" per test case, so we will fail if
    // we see multiple `ConnectToPuppetUnderTest` requests.
    let mut connected_to_puppet_under_test = false;
    while let Some(event) = stream.try_next().await? {
        match event {
            ui_test_context::ContextRequest::GetDisplayDimensions { responder, .. } => {
                let info_proxy = puppet_realm
                    .root
                    .connect_to_protocol_at_exposed_dir::<InfoMarker>()
                    .expect("failed to connect to display info service");
                let display_metrics =
                    info_proxy.get_metrics().await.expect("failed to get display metrics");
                let dimensions =
                    display_metrics.extent_in_px.expect("display metrics missing dimensions");
                responder
                    .send(ui_test_context::ContextGetDisplayDimensionsResponse {
                        width_in_physical_px: Some(dimensions.width),
                        height_in_physical_px: Some(dimensions.height),
                        ..Default::default()
                    })
                    .expect("failed to respond to get display dimensions request");
            }
            ui_test_context::ContextRequest::GetRootViewToken { responder, .. } => {
                assert!(!created_root_view_token);
                created_root_view_token = true;
                let scene_controller = puppet_realm
                    .root
                    .connect_to_protocol_at_exposed_dir::<test_scene::ControllerMarker>()
                    .expect("failed to connect to scene controller");
                let root_view_token_pair = scenic::flatland::ViewCreationTokenPair::new()
                    .expect("failed to create root view token pair");
                let present_view_request = test_scene::ControllerPresentClientViewRequest {
                    viewport_creation_token: Some(root_view_token_pair.viewport_creation_token),
                    ..Default::default()
                };
                scene_controller
                    .present_client_view(present_view_request)
                    .expect("failed to present root puppet view");
                responder
                    .send(root_view_token_pair.view_creation_token)
                    .expect("failed to respond to `GetRootViewToken`");
            }
            ui_test_context::ContextRequest::ConnectToPuppetUnderTest {
                payload,
                responder,
                ..
            } => {
                assert!(!connected_to_puppet_under_test);
                connected_to_puppet_under_test = true;
                let (puppet_factory_proxy, puppet_factory_server) =
                    create_proxy::<ui_conformance::PuppetFactoryMarker>()
                        .expect("failed to open puppet factory channel");
                puppet_realm
                    .root
                    .connect_request_to_named_protocol_at_exposed_dir(
                        PUPPET_UNDER_TEST_FACTORY_SERVICE,
                        puppet_factory_server.into_channel(),
                    )
                    .expect("failed to connect to puppet");
                puppet_factory_proxy
                    .create(payload)
                    .await
                    .expect("failed to create puppet-under-test instance");
                responder.send().expect("failed to respond to ConnectToPuppetUnderTest request");
            }
            ui_test_context::ContextRequest::ConnectToAuxiliaryPuppet {
                payload,
                responder,
                ..
            } => {
                let (puppet_factory_proxy, puppet_factory_server) =
                    create_proxy::<ui_conformance::PuppetFactoryMarker>()
                        .expect("failed to open puppet factory channel");
                puppet_realm
                    .root
                    .connect_request_to_named_protocol_at_exposed_dir(
                        AUXILIARY_PUPPET_FACTORY_SERVICE,
                        puppet_factory_server.into_channel(),
                    )
                    .expect("failed to connect to puppet");
                puppet_factory_proxy
                    .create(payload)
                    .await
                    .expect("failed to create auxilliary puppet instance");
                responder.send().expect("failed to respond to ConnectToAuxiliaryPuppet request");
            }
            ui_test_context::ContextRequest::ConnectToFlatland { server_end, .. } => {
                puppet_realm
                    .root
                    .connect_request_to_protocol_at_exposed_dir(server_end)
                    .expect("failed to connect to flatland");
            }
            ui_test_context::ContextRequest::ConnectToInputRegistry { server_end, .. } => {
                puppet_realm
                    .root
                    .connect_request_to_protocol_at_exposed_dir(server_end)
                    .expect("failed to connect to input registry");
            }
        }
    }

    Ok(())
}

async fn assemble_puppet_realm() -> RealmInstance {
    let builder = RealmBuilder::new().await.expect("Failed to create RealmBuilder.");

    // Add test UI stack component.
    builder
        .add_child(TEST_UI_STACK, TEST_UI_STACK_URL, ChildOptions::new())
        .await
        .expect("Failed to add UI realm.");

    // Add factory for the puppet-under-test.
    builder
        .add_child(PUPPET_UNDER_TEST_FACTORY, PUPPET_UNDER_TEST_FACTORY_URL, ChildOptions::new())
        .await
        .expect("Failed to add puppet.");

    // Add factory for auxiliary puppets.
    builder
        .add_child(AUXILIARY_PUPPET_FACTORY, AUXILIARY_PUPPET_FACTORY_URL, ChildOptions::new())
        .await
        .expect("Failed to add puppet.");

    // Route capabilities to the test UI stack.
    builder
        .add_route(
            Route::new()
                .capability(Capability::protocol::<LogSinkMarker>())
                .capability(Capability::protocol::<ProfileProviderMarker>())
                .capability(Capability::protocol::<AllocatorMarker>())
                .capability(Capability::protocol::<LoaderMarker>())
                .capability(Capability::protocol::<RegistryMarker>())
                .from(Ref::parent())
                .to(Ref::child(TEST_UI_STACK)),
        )
        .await
        .expect("Failed to route capabilities.");

    // Route capabilities to puppet.
    builder
        .add_route(
            Route::new()
                .capability(Capability::protocol::<LogSinkMarker>())
                .from(Ref::parent())
                .to(Ref::child(PUPPET_UNDER_TEST_FACTORY))
                .to(Ref::child(AUXILIARY_PUPPET_FACTORY)),
        )
        .await
        .expect("Failed to route capabilities.");

    // Route capabilities from the test UI stack to the puppet.
    builder
        .add_route(
            Route::new()
                .capability(Capability::protocol::<FlatlandMarker>())
                .capability(Capability::protocol::<KeyboardMarker>())
                .from(Ref::child(TEST_UI_STACK))
                .to(Ref::child(PUPPET_UNDER_TEST_FACTORY))
                .to(Ref::child(AUXILIARY_PUPPET_FACTORY)),
        )
        .await
        .expect("Failed to route capabilities.");

    // Expose the puppet-under-test factory service.
    builder
        .add_route(
            Route::new()
                .capability(
                    Capability::protocol::<ui_conformance::PuppetFactoryMarker>()
                        .as_(PUPPET_UNDER_TEST_FACTORY_SERVICE),
                )
                .from(Ref::child(PUPPET_UNDER_TEST_FACTORY))
                .to(Ref::parent()),
        )
        .await
        .expect("Failed to route capabilities.");

    // Expose the puppet-under-test factory service.
    builder
        .add_route(
            Route::new()
                .capability(
                    Capability::protocol::<ui_conformance::PuppetFactoryMarker>()
                        .as_(AUXILIARY_PUPPET_FACTORY_SERVICE),
                )
                .from(Ref::child(AUXILIARY_PUPPET_FACTORY))
                .to(Ref::parent()),
        )
        .await
        .expect("Failed to route capabilities.");

    // Expose UI capabilities.
    builder
        .add_route(
            Route::new()
                .capability(Capability::protocol::<test_scene::ControllerMarker>())
                .capability(Capability::protocol::<FlatlandMarker>())
                .capability(Capability::protocol::<InfoMarker>())
                .capability(Capability::protocol::<ui_input::RegistryMarker>())
                .from(Ref::child(TEST_UI_STACK))
                .to(Ref::parent()),
        )
        .await
        .expect("Failed to route capabilities.");

    builder
        .add_route(
            Route::new()
                .capability(Capability::storage("tmp"))
                .from(Ref::parent())
                .to(Ref::child(TEST_UI_STACK)),
        )
        .await
        .expect("Failed to route capabilities.");

    // Create the test realm.
    builder.build().await.expect("Failed to create test realm.")
}
