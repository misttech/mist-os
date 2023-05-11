// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use assert_matches::assert_matches;
use fidl::endpoints::DiscoverableProtocolMarker;
use fidl_fuchsia_component::{self as fcomp, CreateChildArgs, RealmMarker};
use fidl_fuchsia_component_decl::{
    Child, ChildRef, CollectionRef, ConfigOverride, ConfigSingleValue, ConfigValue, StartupMode,
};
use fidl_fuchsia_io as fio;
use fidl_test_config_parentoverrides::ReporterMarker;
use fuchsia_component::client::{
    connect_to_protocol, connect_to_protocol_at_dir_root, open_childs_exposed_directory,
};
use fuchsia_component_test::{Capability, ChildOptions, RealmBuilder, Ref, Route};

const RECEIVER_URL: &str = "#meta/config_receiver.cm";

#[fuchsia::test]
async fn realm_api_overrides() {
    let realm = connect_to_protocol::<RealmMarker>().unwrap();

    let child_name = String::from("dynamic_child_realm_api_success");
    let collection_name = String::from("realm_api_collection");
    let field_name = String::from("parent_provided");
    let override_value = String::from("config value for dynamic child launched by realm api");

    let mut collection_ref = CollectionRef { name: collection_name.clone() };
    let child_decl = Child {
        name: Some(child_name.clone()),
        url: Some(String::from(RECEIVER_URL)),
        startup: Some(StartupMode::Lazy),
        config_overrides: Some(vec![ConfigOverride {
            key: Some(field_name),
            value: Some(ConfigValue::Single(ConfigSingleValue::String(override_value.clone()))),
            ..ConfigOverride::default()
        }]),
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
    assert_eq!(value, override_value);
}

#[fuchsia::test]
async fn realm_api_refuses_to_override_immutable_config() {
    let realm = connect_to_protocol::<RealmMarker>().unwrap();

    let child_name = String::from("dynamic_child_realm_api_fails_with_immutable_field");
    let collection_name = String::from("realm_api_collection");
    let field_name = String::from("not_parent_provided");
    let override_value = String::from("config value for dynamic child launched by realm api");

    let mut collection_ref = CollectionRef { name: collection_name.clone() };
    let child_decl = Child {
        name: Some(child_name.clone()),
        url: Some(String::from(RECEIVER_URL)),
        startup: Some(StartupMode::Lazy),
        config_overrides: Some(vec![ConfigOverride {
            key: Some(field_name),
            value: Some(ConfigValue::Single(ConfigSingleValue::String(override_value.clone()))),
            ..ConfigOverride::default()
        }]),
        ..Child::default()
    };

    realm
        .create_child(&mut collection_ref, &child_decl, CreateChildArgs::default())
        .await
        .unwrap()
        .unwrap();

    // open the child's exposed directory to start resolving it
    let mut child_ref = ChildRef { name: child_name, collection: Some(collection_name) };
    let (_exposed_dir, exposed_dir_server) =
        fidl::endpoints::create_proxy::<fio::DirectoryMarker>().unwrap();
    assert_matches!(
        realm
            .open_exposed_dir(&mut child_ref, exposed_dir_server)
            .await
            .expect("FIDL syscalls succeed"),
        Err(fcomp::Error::InstanceCannotResolve)
    );
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
