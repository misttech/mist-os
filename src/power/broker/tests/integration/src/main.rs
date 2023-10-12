// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::Result;
use fidl_fuchsia_power_broker::{
    BinaryPowerLevel, LessorMarker, LevelControlMarker, PowerLevel, StatusMarker,
};
use fuchsia_component_test::{Capability, ChildOptions, RealmBuilder, Ref, Route};
use std::{thread, time};

// TODO(b/299463665): Remove these hard-coded element IDs once we can edit
// the topology.
// A <- B <- C -> D
const ELEMENT_A: &str = "A";
const ELEMENT_B: &str = "B";
const ELEMENT_C: &str = "C";
const ELEMENT_D: &str = "D";

#[fuchsia::test]
async fn test_single_element() -> Result<()> {
    let builder = RealmBuilder::new().await?;

    // Create a Power Broker component
    let power_broker = builder
        .add_child("power_broker", "power-broker#meta/power-broker.cm", ChildOptions::new())
        .await?;

    // Create a MockPowerElement that continuously connects to the PowerBroker
    let fake_driver = builder
        .add_child("fake_driver", "#meta/fake-driver.cm", ChildOptions::new().eager())
        .await?;
    builder.init_mutable_config_to_empty(&fake_driver).await.unwrap();
    builder.set_config_value_string("fake_driver", "element_id", ELEMENT_A).await?;

    builder
        .add_route(
            Route::new()
                .capability(Capability::protocol::<LevelControlMarker>())
                .from(&power_broker)
                .to(&fake_driver),
        )
        .await?;

    builder
        .add_route(
            Route::new()
                .capability(Capability::protocol::<StatusMarker>())
                .from(&power_broker)
                .to(Ref::parent()),
        )
        .await?;

    builder
        .add_route(
            Route::new()
                .capability(Capability::protocol::<LessorMarker>())
                .from(&power_broker)
                .to(Ref::parent()),
        )
        .await?;

    let realm = builder.build().await?;

    let power_lease = realm.root.connect_to_protocol_at_exposed_dir::<LessorMarker>()?;
    let power_status: fidl_fuchsia_power_broker::StatusProxy =
        realm.root.connect_to_protocol_at_exposed_dir::<StatusMarker>()?;

    // TODO: Replace hardcoded device ID
    // Wait for the fake device to connect
    let initial: time::Instant = time::Instant::now();
    let timeout = time::Duration::from_millis(5000);
    loop {
        let power_level = power_status.get_power_level(ELEMENT_A.into()).await?;
        if power_level == Ok(PowerLevel::Binary(BinaryPowerLevel::Off)) {
            break;
        }
        if time::Instant::now().duration_since(initial) >= timeout {
            assert!(
                false,
                "timed out after waiting {:?} for fake device to connect to Status",
                timeout
            );
        }
        thread::sleep(time::Duration::from_millis(100));
    }

    // acquire lease with PB, device should turn on
    let lease_id = power_lease
        .lease(ELEMENT_B.into(), &PowerLevel::Binary(BinaryPowerLevel::On))
        .await
        .expect("Lease response not ok");
    let turning_on: time::Instant = time::Instant::now();
    let timeout = time::Duration::from_millis(5000);
    loop {
        let power_level = power_status.get_power_level(ELEMENT_A.into()).await?;
        if power_level == Ok(PowerLevel::Binary(BinaryPowerLevel::On)) {
            break;
        }
        if time::Instant::now().duration_since(turning_on) >= timeout {
            assert!(
                false,
                "timed out after waiting {:?} for fake device to connect to Status",
                timeout
            );
        }
        thread::sleep(time::Duration::from_millis(100));
    }

    // drop lease with PB, device should turn off again
    power_lease.drop_lease(&lease_id).expect("drop failed");
    let turning_off: time::Instant = time::Instant::now();
    let timeout = time::Duration::from_millis(5000);
    loop {
        let power_level = power_status.get_power_level(ELEMENT_A.into()).await?;
        if power_level == Ok(PowerLevel::Binary(BinaryPowerLevel::Off)) {
            break;
        }
        if time::Instant::now().duration_since(turning_off) >= timeout {
            assert!(
                false,
                "timed out after waiting {:?} for fake device to connect to Status",
                timeout
            );
        }
        thread::sleep(time::Duration::from_millis(100));
    }

    Ok(())
}

#[fuchsia::test]
async fn test_multiple_elements() -> Result<()> {
    let builder = RealmBuilder::new().await?;

    // Create a Power Broker component
    let power_broker = builder
        .add_child("power_broker", "power-broker#meta/power-broker.cm", ChildOptions::new())
        .await?;

    // Create two fake elements
    // TODO(b/299463665): Specify the topology here
    // Using hard-coded topology:
    // A <- B <- C -> D
    let element_ids = [ELEMENT_A, ELEMENT_D];
    for element_id in element_ids {
        let element_name = format!("fake_element_{}", element_id);
        let fake_element: fuchsia_component_test::ChildRef = builder
            .add_child(element_name.clone(), "#meta/fake-driver.cm", ChildOptions::new().eager())
            .await?;
        builder.init_mutable_config_to_empty(&fake_element).await.unwrap();
        builder.set_config_value_string(element_name.clone(), "element_id", element_id).await?;
        builder
            .add_route(
                Route::new()
                    .capability(Capability::protocol::<LevelControlMarker>())
                    .from(&power_broker)
                    .to(&fake_element),
            )
            .await?;
    }

    builder
        .add_route(
            Route::new()
                .capability(Capability::protocol::<StatusMarker>())
                .from(&power_broker)
                .to(Ref::parent()),
        )
        .await?;

    builder
        .add_route(
            Route::new()
                .capability(Capability::protocol::<LessorMarker>())
                .from(&power_broker)
                .to(Ref::parent()),
        )
        .await?;

    let realm = builder.build().await?;

    let power_lease = realm.root.connect_to_protocol_at_exposed_dir::<LessorMarker>()?;
    let power_status: fidl_fuchsia_power_broker::StatusProxy =
        realm.root.connect_to_protocol_at_exposed_dir::<StatusMarker>()?;

    // Wait for the elements to connect
    let initial: time::Instant = time::Instant::now();
    let timeout = time::Duration::from_millis(5000);
    loop {
        let mut all_connected: bool = true;
        for element_id in element_ids {
            let power_level = power_status.get_power_level(element_id.into()).await?;
            if power_level != Ok(PowerLevel::Binary(BinaryPowerLevel::Off)) {
                all_connected = false;
            }
        }
        if all_connected {
            break;
        }
        if time::Instant::now().duration_since(initial) >= timeout {
            assert!(
                false,
                "timed out after waiting {:?} for fake device to connect to Status",
                timeout
            );
        }
        thread::sleep(time::Duration::from_millis(100));
    }

    // acquire lease for B with PB, A should turn on
    let lease_id = power_lease
        .lease(ELEMENT_B.into(), &PowerLevel::Binary(BinaryPowerLevel::On))
        .await
        .expect("Lease response not ok");
    let turning_on: time::Instant = time::Instant::now();
    let timeout = time::Duration::from_millis(5000);
    loop {
        let power_level = power_status.get_power_level(ELEMENT_A.into()).await?;
        if power_level == Ok(PowerLevel::Binary(BinaryPowerLevel::On)) {
            break;
        }
        if time::Instant::now().duration_since(turning_on) >= timeout {
            assert!(false, "timed out after waiting {:?} for fake device to turn ON", timeout);
        }
        thread::sleep(time::Duration::from_millis(100));
    }
    let power_level = power_status.get_power_level(ELEMENT_D.into()).await?;
    assert_eq!(
        power_level,
        Ok(PowerLevel::Binary(BinaryPowerLevel::Off)),
        "Element D should still be OFF"
    );

    // drop lease for B with PB, A should turn off again
    power_lease.drop_lease(&lease_id).expect("drop failed");
    let turning_off: time::Instant = time::Instant::now();
    let timeout = time::Duration::from_millis(5000);
    loop {
        let power_level = power_status.get_power_level(ELEMENT_A.into()).await?;
        if power_level == Ok(PowerLevel::Binary(BinaryPowerLevel::Off)) {
            break;
        }
        if time::Instant::now().duration_since(turning_off) >= timeout {
            assert!(false, "timed out after waiting {:?} for fake device to turn OFF", timeout);
        }
        thread::sleep(time::Duration::from_millis(100));
    }
    let power_level = power_status.get_power_level(ELEMENT_D.into()).await?;
    assert_eq!(
        power_level,
        Ok(PowerLevel::Binary(BinaryPowerLevel::Off)),
        "Element D should still be OFF"
    );

    Ok(())
}

#[fuchsia::test]
async fn test_transitive_leases() -> Result<()> {
    let builder = RealmBuilder::new().await?;

    // Create a Power Broker component
    // Topology:
    // A <- B <- C -> D
    // TODO(b/299463665): Assumes hardcoded topology, specify it here instead.
    let power_broker = builder
        .add_child("power_broker", "power-broker#meta/power-broker.cm", ChildOptions::new())
        .await?;

    // Create four fake elements
    let element_ids = [ELEMENT_A, ELEMENT_B, ELEMENT_C, ELEMENT_D];
    for element_id in element_ids {
        let element_name = format!("fake_element_{}", element_id);
        let fake_element: fuchsia_component_test::ChildRef = builder
            .add_child(element_name.clone(), "#meta/fake-driver.cm", ChildOptions::new().eager())
            .await?;
        builder.init_mutable_config_to_empty(&fake_element).await.unwrap();
        builder.set_config_value_string(element_name.clone(), "element_id", element_id).await?;
        builder
            .add_route(
                Route::new()
                    .capability(Capability::protocol::<LevelControlMarker>())
                    .from(&power_broker)
                    .to(&fake_element),
            )
            .await?;
    }

    builder
        .add_route(
            Route::new()
                .capability(Capability::protocol::<StatusMarker>())
                .from(&power_broker)
                .to(Ref::parent()),
        )
        .await?;

    builder
        .add_route(
            Route::new()
                .capability(Capability::protocol::<LessorMarker>())
                .from(&power_broker)
                .to(Ref::parent()),
        )
        .await?;

    let realm = builder.build().await?;

    let power_lease = realm.root.connect_to_protocol_at_exposed_dir::<LessorMarker>()?;
    let power_status: fidl_fuchsia_power_broker::StatusProxy =
        realm.root.connect_to_protocol_at_exposed_dir::<StatusMarker>()?;

    // Wait for the elements to connect
    let initial: time::Instant = time::Instant::now();
    let timeout = time::Duration::from_millis(5000);
    loop {
        let mut all_connected: bool = true;
        for element_id in element_ids {
            let power_level = power_status.get_power_level(element_id.into()).await?;
            if power_level != Ok(PowerLevel::Binary(BinaryPowerLevel::Off)) {
                all_connected = false;
            }
        }
        if all_connected {
            break;
        }
        if time::Instant::now().duration_since(initial) >= timeout {
            assert!(
                false,
                "timed out after waiting {:?} for fake device to connect to Status",
                timeout
            );
        }
        thread::sleep(time::Duration::from_millis(100));
    }

    // acquire lease for C with PB, A and B should turn on
    // because C has a dependency on B and B has a dependency on A
    // D should turn on because C has a dependency on it
    let lease_id = power_lease
        .lease(ELEMENT_C.into(), &PowerLevel::Binary(BinaryPowerLevel::On))
        .await
        .expect("Lease response not ok");
    let turning_on: time::Instant = time::Instant::now();
    let timeout = time::Duration::from_millis(5000);
    loop {
        let mut all_on: bool = true;
        for element_id in [ELEMENT_A, ELEMENT_B, ELEMENT_D] {
            let power_level = power_status.get_power_level(element_id.into()).await?;
            if power_level != Ok(PowerLevel::Binary(BinaryPowerLevel::On)) {
                all_on = false;
            }
        }
        if all_on {
            break;
        }
        if time::Instant::now().duration_since(turning_on) >= timeout {
            assert!(false, "timed out after waiting {:?} for fake device to turn ON", timeout);
        }
        thread::sleep(time::Duration::from_millis(100));
    }
    let power_level = power_status.get_power_level(ELEMENT_C.into()).await?;
    assert_eq!(
        power_level,
        Ok(PowerLevel::Binary(BinaryPowerLevel::Off)),
        "Element C should still be OFF"
    );

    // drop lease for B with PB, both A and B should turn off again
    power_lease.drop_lease(&lease_id).expect("drop failed");
    let turning_off: time::Instant = time::Instant::now();
    let timeout = time::Duration::from_millis(5000);
    loop {
        let mut all_off: bool = true;
        for element_id in [ELEMENT_A, ELEMENT_B] {
            let power_level = power_status.get_power_level(element_id.into()).await?;
            if power_level != Ok(PowerLevel::Binary(BinaryPowerLevel::Off)) {
                all_off = false;
            }
        }
        if all_off {
            break;
        }
        if time::Instant::now().duration_since(turning_off) >= timeout {
            assert!(false, "timed out after waiting {:?} for fake device to turn OFF", timeout);
        }
        thread::sleep(time::Duration::from_millis(100));
    }
    let power_level = power_status.get_power_level(ELEMENT_C.into()).await?;
    assert_eq!(
        power_level,
        Ok(PowerLevel::Binary(BinaryPowerLevel::Off)),
        "Element C should still be OFF"
    );

    Ok(())
}
