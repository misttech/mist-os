// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    fidl_fuchsia_component as fcomp, fidl_fuchsia_component_decl as fdecl,
    fidl_fuchsia_sys2 as fsys, fuchsia_component::client::connect_to_protocol,
};

#[fuchsia::test]
async fn static_child() {
    let lifecycle_controller = connect_to_protocol::<fsys::LifecycleControllerMarker>().unwrap();
    let realm_query = connect_to_protocol::<fsys::RealmQueryMarker>().unwrap();

    // echo server is unresolved
    let instance = realm_query.get_instance("./echo_server").await.unwrap().unwrap();
    assert!(instance.resolved_info.is_none());

    lifecycle_controller.resolve("./echo_server").await.unwrap().unwrap();

    // echo server is resolved
    let instance = realm_query.get_instance("./echo_server").await.unwrap().unwrap();
    let resolved_info = instance.resolved_info.unwrap();
    assert!(resolved_info.execution_info.is_none());

    lifecycle_controller.start("./echo_server").await.unwrap().unwrap();

    // echo server is running
    let instance = realm_query.get_instance("./echo_server").await.unwrap().unwrap();
    let resolved_info = instance.resolved_info.unwrap();
    assert!(resolved_info.execution_info.is_some());

    lifecycle_controller.stop("./echo_server", false).await.unwrap().unwrap();

    // echo server is not running
    let instance = realm_query.get_instance("./echo_server").await.unwrap().unwrap();
    let resolved_info = instance.resolved_info.unwrap();
    assert!(resolved_info.execution_info.is_none());
}

#[fuchsia::test]
async fn dynamic_child() {
    let lifecycle_controller = connect_to_protocol::<fsys::LifecycleControllerMarker>().unwrap();
    let realm_query = connect_to_protocol::<fsys::RealmQueryMarker>().unwrap();

    // dynamic echo server doesn't exist
    let error =
        realm_query.get_instance("./servers:dynamic_echo_server").await.unwrap().unwrap_err();
    assert_eq!(error, fsys::GetInstanceError::InstanceNotFound);

    lifecycle_controller
        .create_child(
            ".",
            &mut fdecl::CollectionRef { name: "servers".to_string() },
            fdecl::Child {
                name: Some("dynamic_echo_server".to_string()),
                url: Some("#meta/echo_server.cm".to_string()),
                startup: Some(fdecl::StartupMode::Lazy),
                ..fdecl::Child::EMPTY
            },
            fcomp::CreateChildArgs::EMPTY,
        )
        .await
        .unwrap()
        .unwrap();

    // dynamic echo server is unresolved
    let instance =
        realm_query.get_instance("./servers:dynamic_echo_server").await.unwrap().unwrap();
    assert!(instance.resolved_info.is_none());

    lifecycle_controller.resolve("./servers:dynamic_echo_server").await.unwrap().unwrap();

    // dynamic echo server is resolved
    let instance =
        realm_query.get_instance("./servers:dynamic_echo_server").await.unwrap().unwrap();
    let resolved_info = instance.resolved_info.unwrap();
    assert!(resolved_info.execution_info.is_none());

    lifecycle_controller.start("./servers:dynamic_echo_server").await.unwrap().unwrap();

    // dynamic echo server is running
    let instance =
        realm_query.get_instance("./servers:dynamic_echo_server").await.unwrap().unwrap();
    let resolved_info = instance.resolved_info.unwrap();
    assert!(resolved_info.execution_info.is_some());

    lifecycle_controller.stop("./servers:dynamic_echo_server", false).await.unwrap().unwrap();

    // dynamic echo server is not running
    let instance =
        realm_query.get_instance("./servers:dynamic_echo_server").await.unwrap().unwrap();
    let resolved_info = instance.resolved_info.unwrap();
    assert!(resolved_info.execution_info.is_none());

    lifecycle_controller
        .destroy_child(
            ".",
            &mut fdecl::ChildRef {
                name: "dynamic_echo_server".to_string(),
                collection: Some("servers".to_string()),
            },
        )
        .await
        .unwrap()
        .unwrap();

    // dynamic echo server doesn't exist
    let error =
        realm_query.get_instance("./servers:dynamic_echo_server").await.unwrap().unwrap_err();
    assert_eq!(error, fsys::GetInstanceError::InstanceNotFound);
}
