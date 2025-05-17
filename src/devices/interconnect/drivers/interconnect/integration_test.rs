// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::Result;
use fidl::endpoints::create_endpoints;
use fidl_fuchsia_driver_development::ManagerMarker;
use fidl_fuchsia_driver_framework::{NodePropertyKey, NodePropertyValue};
use fidl_fuchsia_driver_test::RealmArgs;
use fuchsia_component_test::RealmBuilder;
use fuchsia_driver_test::{DriverTestRealmBuilder, DriverTestRealmInstance};

#[fuchsia::test]
async fn test_interconnect_driver() -> Result<()> {
    // Create the RealmBuilder.
    let builder = RealmBuilder::new().await?;
    builder.driver_test_realm_setup().await?;
    // Build the Realm.
    let instance = builder.build().await?;
    // Start DriverTestRealm
    instance
        .driver_test_realm_start(RealmArgs {
            root_driver: Some("#meta/fake_interconnect.cm".to_owned()),
            ..Default::default()
        })
        .await?;

    let manager = instance.root.connect_to_protocol_at_exposed_dir::<ManagerMarker>()?;
    let (node_iter, node_iter_server) = create_endpoints();
    manager.get_node_info(
        &[
            "dev.fake_interconnect.path_a-0".to_owned(),
            "dev.fake_interconnect.path_b-1".to_owned(),
            "dev.fake_interconnect.path_c-2".to_owned(),
        ],
        node_iter_server,
        true,
    )?;
    let node_iter = node_iter.into_proxy();
    let nodes = node_iter.get_next().await?;
    if nodes.len() != 3 {
        panic!("Didn't find all 3 paths");
    }

    let expected_props = [0, 1, 2];
    for (node, expected_prop) in nodes.iter().zip(&expected_props) {
        let expected_key =
            NodePropertyKey::StringValue(bind_fuchsia::BIND_INTERCONNECT_PATH_ID.to_owned());
        let expected_value = NodePropertyValue::IntValue(*expected_prop);
        let prop_found = node
            .node_property_list
            .as_ref()
            .expect("node property list to be filled in")
            .into_iter()
            .any(|prop| prop.key == expected_key && prop.value == expected_value);
        assert!(prop_found);
    }

    Ok(())
}
