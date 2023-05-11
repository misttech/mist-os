// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use fidl::endpoints::DiscoverableProtocolMarker;
use fidl_fuchsia_component::{CreateChildArgs, RealmMarker};
use fidl_fuchsia_component_decl::{Child, CollectionRef, StartupMode};
use fidl_test_config_parentoverrides::ReporterMarker;
use fuchsia_component::client::{
    connect_to_protocol, connect_to_protocol_at_dir_root, open_childs_exposed_directory,
};
use fuchsia_component_test::{Capability, ChildOptions, RealmBuilder, Ref, Route};

const RECEIVER_URL: &str = "#meta/config_receiver.cm";

#[fuchsia::test]
async fn realm_api_without_override_returns_default() {
    let realm = connect_to_protocol::<RealmMarker>().unwrap();

    // TODO(https://fxbug.dev/96254) set a dynamic-child-specific config value here
    let expected_value = "default value";

    let child_name = String::from("dynamic_child_realm_api");
    let collection_name = String::from("realm_api_collection");

    let mut collection_ref = CollectionRef { name: collection_name.clone() };
    let child_decl = Child {
        name: Some(child_name.clone()),
        url: Some(String::from(RECEIVER_URL)),
        startup: Some(StartupMode::Lazy),
        ..Child::default()
    };

    realm
        .create_child(&mut collection_ref, &child_decl, CreateChildArgs::default())
        .await
        .unwrap()
        .unwrap();

    let exposed_dir =
        open_childs_exposed_directory(child_name, Some(collection_name)).await.unwrap();

    let reporter = connect_to_protocol_at_dir_root::<ReporterMarker>(&exposed_dir).unwrap();

    let value = reporter.get_parent_provided_config_string().await.unwrap();
    assert_eq!(value, expected_value);
}

#[fuchsia::test]
async fn realm_builder_without_override_returns_default() {
    let builder = RealmBuilder::new().await.unwrap();

    // TODO(https://fxbug.dev/102211) set a realm-builder-specific config value here
    let expected_value = "default value";

    let config_receiver = builder
        .add_child("realm_builder_config_receiver", RECEIVER_URL, ChildOptions::new())
        .await
        .unwrap();
    builder
        .add_route(
            Route::new()
                .capability(Capability::protocol_by_name("fuchsia.logger.LogSink"))
                .from(Ref::parent())
                .to(&config_receiver),
        )
        .await
        .unwrap();
    builder
        .add_route(
            Route::new()
                .capability(Capability::protocol_by_name(ReporterMarker::PROTOCOL_NAME))
                .from(&config_receiver)
                .to(Ref::parent()),
        )
        .await
        .unwrap();
    let realm = builder.build().await.unwrap();

    let reporter = realm.root.connect_to_protocol_at_exposed_dir::<ReporterMarker>().unwrap();
    let value = reporter.get_parent_provided_config_string().await.unwrap();
    assert_eq!(value, expected_value);
}

#[fuchsia::test]
async fn static_child_without_overrides_returns_default() {
    // TODO(https://fxbug.dev/126578) set a static-child-specific config value here
    let expected_value = "default value";

    let reporter = connect_to_protocol::<ReporterMarker>().unwrap();
    let value = reporter.get_parent_provided_config_string().await.unwrap();
    assert_eq!(value, expected_value);
}
