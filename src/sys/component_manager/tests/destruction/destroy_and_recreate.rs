// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    fidl::endpoints, fidl_fidl_test_components as ftest, fidl_fuchsia_component as fcomponent,
    fidl_fuchsia_component_decl as fdecl, fidl_fuchsia_io as fio, fuchsia_async as fasync,
    fuchsia_component::client, fuchsia_zircon as zx, tracing::*,
};

#[fuchsia::main]
async fn main() {
    info!("Started collection realm");
    let realm = client::connect_to_protocol::<fcomponent::RealmMarker>()
        .expect("could not connect to Realm service");

    // Create a "trigger realm" child component.
    info!("Creating child");
    {
        let mut collection_ref = fdecl::CollectionRef { name: "coll".to_string() };
        let child_decl = fdecl::Child {
            name: Some("trigger".to_string()),
            url: Some("#meta/trigger_realm.cm".to_string()),
            startup: Some(fdecl::StartupMode::Lazy),
            environment: None,
            ..fdecl::Child::EMPTY
        };
        realm
            .create_child(&mut collection_ref, child_decl, fcomponent::CreateChildArgs::EMPTY)
            .await
            .unwrap_or_else(|e| panic!("create_child failed: {:?}", e))
            .unwrap_or_else(|e| panic!("failed to create child: {:?}", e));
    }

    // Bind to child, causing it to start (along with its eager children).
    info!("Binding to child");
    {
        let mut child_ref =
            fdecl::ChildRef { name: "trigger".to_string(), collection: Some("coll".to_string()) };
        let (dir, server_end) = endpoints::create_proxy::<fio::DirectoryMarker>().unwrap();
        realm
            .open_exposed_dir(&mut child_ref, server_end)
            .await
            .unwrap_or_else(|e| panic!("open_exposed_dir failed: {:?}", e))
            .unwrap_or_else(|e| panic!("failed to open child exposed dir: {:?}", e));
        let trigger = client::connect_to_protocol_at_dir_root::<ftest::TriggerMarker>(&dir)
            .expect("failed to open trigger service");
        trigger.run().await.expect("trigger failed");
    }

    // Destroy the child.
    info!("Destroying and recreating child");
    {
        let mut child_ref =
            fdecl::ChildRef { name: "trigger".to_string(), collection: Some("coll".to_string()) };
        realm
            .destroy_child(&mut child_ref)
            .await
            .expect("destroy_child failed")
            .expect("failed to destroy child");
    }

    // Recreate the child immediately.
    {
        let mut collection_ref = fdecl::CollectionRef { name: "coll".to_string() };
        let child_decl = fdecl::Child {
            name: Some("trigger".to_string()),
            url: Some("#meta/trigger_realm.cm".to_string()),
            startup: Some(fdecl::StartupMode::Lazy),
            environment: None,
            ..fdecl::Child::EMPTY
        };
        realm
            .create_child(&mut collection_ref, child_decl, fcomponent::CreateChildArgs::EMPTY)
            .await
            .unwrap_or_else(|e| panic!("create_child failed: {:?}", e))
            .unwrap_or_else(|e| panic!("failed to create child: {:?}", e));
    }

    // Sleep so that it's more likely that, if the new component is destroyed incorrectly,
    // this test will fail.
    fasync::Timer::new(fasync::Time::after(zx::Duration::from_seconds(5))).await;

    // Restart the child.
    info!("Restarting to child");
    {
        let mut child_ref =
            fdecl::ChildRef { name: "trigger".to_string(), collection: Some("coll".to_string()) };
        let (dir, server_end) = endpoints::create_proxy::<fio::DirectoryMarker>().unwrap();
        realm
            .open_exposed_dir(&mut child_ref, server_end)
            .await
            .unwrap_or_else(|e| panic!("open_exposed_dir failed: {:?}", e))
            .unwrap_or_else(|e| panic!("failed to open child exposed dir: {:?}", e));
        let trigger = client::connect_to_protocol_at_dir_root::<ftest::TriggerMarker>(&dir)
            .expect("failed to open trigger service");
        trigger.run().await.expect("trigger failed");
    }
}
